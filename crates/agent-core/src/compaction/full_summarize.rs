//! Full Summarize layer implementation.
//!
//! Makes an async model call to produce a structured 9-section summary
//! of older conversation history. This is the heaviest compaction layer,
//! invoked only when lighter layers (Tools Compact, Session Memory) are insufficient.

use crate::message::{ContentBlock, Message};
use crate::model::{Model, ModelRequest, ModelResponse};

use super::layer::{CompactionContext, LayerResult};
use super::tokens::estimate_tokens;
use super::CompactionEvent;

/// Layer 3: Model-based structured summarization.
///
/// This layer does NOT implement the sync `CompactionLayer` trait because it
/// requires an async model call. Instead it exposes `apply_async`.
pub struct FullSummarizeLayer;

impl FullSummarizeLayer {
    /// Async execution path for the full summarize layer.
    ///
    /// Separates system vs non-system messages, determines which to summarize
    /// based on `preserve_recent_messages`, builds a 9-section prompt, calls the
    /// model, and replaces summarized messages with a single system message.
    ///
    /// Returns `Failed` on model error without modifying messages.
    pub async fn apply_async(
        &self,
        messages: &mut Vec<Message>,
        context: &CompactionContext,
        model: &dyn Model,
    ) -> LayerResult {
        let config = &context.config;

        // Collect indices of all non-system messages.
        let non_system_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| !matches!(msg, Message::System { .. }))
            .map(|(idx, _)| idx)
            .collect();

        let non_system_count = non_system_indices.len();

        // Guard: if we don't have more non-system messages than preserve_recent_messages,
        // there's nothing to summarize.
        if non_system_count <= config.preserve_recent_messages {
            return LayerResult::Noop;
        }

        // Split: first (count - preserve_recent_messages) non-system messages get summarized.
        let summarize_count = non_system_count - config.preserve_recent_messages;
        let indices_to_summarize: Vec<usize> = non_system_indices[..summarize_count].to_vec();

        if indices_to_summarize.is_empty() {
            return LayerResult::Noop;
        }

        // Ensure tool-use/tool-result pair integrity at the boundary.
        let adjusted_indices = ensure_pair_integrity(messages, &indices_to_summarize);

        // Collect references to messages we're going to summarize.
        let messages_to_summarize: Vec<&Message> =
            adjusted_indices.iter().map(|&idx| &messages[idx]).collect();

        // Build the 9-section summarization prompt.
        let prompt = build_summarization_prompt(&messages_to_summarize);

        // Build the model request.
        let request = ModelRequest {
            system: prompt,
            messages: vec![Message::User {
                content: vec![ContentBlock::Text {
                    text: "Produce a structured summary of the conversation above.".into(),
                }],
            }],
            tools: vec![],
            max_tokens: Some(4096),
            temperature: Some(0.0),
            output_schema: None,
        };

        // Call the model. On failure, return Failed WITHOUT modifying messages.
        let response: ModelResponse = match model.complete(request).await {
            Ok(resp) => resp,
            Err(e) => {
                return LayerResult::Failed(format!("model call failed: {}", e));
            }
        };

        // Extract summary text from response content blocks.
        let summary_text: String = response
            .content
            .iter()
            .filter_map(|block| match block {
                crate::model::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if summary_text.is_empty() {
            return LayerResult::Failed("model returned empty summary".into());
        }

        // Now perform the mutation.
        let tokens_before = estimate_tokens(messages);
        let messages_affected = adjusted_indices.len();

        // Remove summarized messages (back to front to preserve indices).
        for &idx in adjusted_indices.iter().rev() {
            messages.remove(idx);
        }

        // Insert summary after the last system-instruction message.
        let insert_pos = messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| matches!(msg, Message::System { .. }))
            .map(|(idx, _)| idx + 1)
            .next_back()
            .unwrap_or(0);

        messages.insert(
            insert_pos,
            Message::System {
                content: format!("[Conversation Summary]\n\n{}", summary_text),
            },
        );

        let tokens_after = estimate_tokens(messages);
        LayerResult::Applied(CompactionEvent {
            stage: "full_summarize".to_string(),
            messages_affected,
            tokens_before,
            tokens_after,
        })
    }
}

/// Ensure tool-use/tool-result pair integrity at the summarize/preserve boundary.
///
/// Rules:
/// - If the last message in `indices_to_summarize` is a ToolResult, find its
///   matching Assistant ToolUse and include it in the summarize set (pull it in).
/// - If the first preserved message (first non-system index NOT in summarize set)
///   is a ToolResult, move the boundary back to also include that ToolResult and
///   its matching Assistant ToolUse.
///
/// This ensures we never split a ToolUse from its ToolResult across the boundary.
pub fn ensure_pair_integrity(messages: &[Message], indices_to_summarize: &[usize]) -> Vec<usize> {
    if indices_to_summarize.is_empty() {
        return Vec::new();
    }

    let mut adjusted = indices_to_summarize.to_vec();

    // Check if the first preserved non-system message is a ToolResult.
    // If so, we need to pull it (and its ToolUse) into the summarize set.
    loop {
        // Find the first non-system index that's NOT in our adjusted set.
        let first_preserved_idx = messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| !matches!(msg, Message::System { .. }))
            .map(|(idx, _)| idx)
            .find(|idx| !adjusted.contains(idx));

        match first_preserved_idx {
            Some(idx) => {
                if let Message::ToolResult { tool_use_id, .. } = &messages[idx] {
                    // This ToolResult's pair (the Assistant ToolUse) must also move.
                    // Find the preceding Assistant message with a matching ToolUse.
                    let tool_use_idx = find_tool_use_index(messages, tool_use_id, idx);

                    // Add both the ToolResult and its matching ToolUse to the summarize set.
                    if !adjusted.contains(&idx) {
                        adjusted.push(idx);
                    }
                    if let Some(tu_idx) = tool_use_idx {
                        if !adjusted.contains(&tu_idx) {
                            adjusted.push(tu_idx);
                        }
                    }
                    // Sort to maintain order and loop again in case the new first
                    // preserved is also a ToolResult.
                    adjusted.sort_unstable();
                } else {
                    break;
                }
            }
            None => break,
        }
    }

    // Also check: if the last index in our adjusted set is an Assistant ToolUse
    // message, we need to pull in its corresponding ToolResult.
    while let Some(&last_idx) = adjusted.last() {
        if let Message::Assistant { content, .. } = &messages[last_idx] {
            // Collect tool_use_ids from this assistant message.
            let tool_use_ids: Vec<&str> = content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { block } => Some(block.id.as_str()),
                    _ => None,
                })
                .collect();

            if tool_use_ids.is_empty() {
                break;
            }

            // Find any ToolResult messages that follow this Assistant and match these IDs.
            let mut added_any = false;
            for &tu_id in &tool_use_ids {
                for (idx, msg) in messages.iter().enumerate() {
                    if idx <= last_idx {
                        continue;
                    }
                    if let Message::ToolResult { tool_use_id, .. } = msg {
                        if tool_use_id == tu_id && !adjusted.contains(&idx) {
                            adjusted.push(idx);
                            added_any = true;
                        }
                    }
                }
            }

            if added_any {
                adjusted.sort_unstable();
            } else {
                break;
            }
        } else {
            break;
        }
    }

    adjusted
}

/// Find the index of the Assistant message containing a ToolUse with the given ID,
/// searching backwards from `before_idx`.
fn find_tool_use_index(
    messages: &[Message],
    tool_use_id: &str,
    before_idx: usize,
) -> Option<usize> {
    for idx in (0..before_idx).rev() {
        if let Message::Assistant { content, .. } = &messages[idx] {
            let has_matching_tool_use = content.iter().any(|block| match block {
                ContentBlock::ToolUse { block } => block.id == tool_use_id,
                _ => false,
            });
            if has_matching_tool_use {
                return Some(idx);
            }
        }
    }
    None
}

/// Build the 9-section structured summarization prompt from messages to summarize.
pub fn build_summarization_prompt(messages: &[&Message]) -> String {
    let conversation = format_messages_for_summary(messages);
    format!(
        r#"You are a conversation summarizer. Analyze the following conversation history and produce a structured summary with exactly these 9 sections:

## 1. Primary Request
What is the user's main goal or request?

## 2. Key Concepts
Important technical concepts, domain terms, or decisions established.

## 3. Files and Code State
Which files have been read, modified, or created. Current state of the codebase.

## 4. Errors and Fixes
Any errors encountered and how they were resolved.

## 5. Problem Solving Context
The reasoning process, approaches tried, and why certain decisions were made.

## 6. User Messages Summary
Key points from user messages (preferences, corrections, clarifications).

## 7. Pending Tasks
Tasks mentioned but not yet completed.

## 8. Current Work State
What was being worked on when this summary was created.

## 9. Next Step
What should happen next based on the conversation flow.

---
Conversation to summarize:

{conversation}"#
    )
}

/// Format messages into a human-readable text representation for the summarization prompt.
pub fn format_messages_for_summary(messages: &[&Message]) -> String {
    let mut output = String::new();

    for msg in messages {
        match msg {
            Message::System { content } => {
                output.push_str("[System]: ");
                output.push_str(content);
                output.push('\n');
            }
            Message::User { content } => {
                output.push_str("[User]: ");
                for block in content {
                    match block {
                        ContentBlock::Text { text } => {
                            output.push_str(text);
                        }
                        ContentBlock::Image { .. } => {
                            output.push_str("[image]");
                        }
                        ContentBlock::ToolUse { block } => {
                            output.push_str(&format!("[tool_use: {}]", block.name));
                        }
                    }
                }
                output.push('\n');
            }
            Message::Assistant { content, .. } => {
                output.push_str("[Assistant]: ");
                for block in content {
                    match block {
                        ContentBlock::Text { text } => {
                            output.push_str(text);
                        }
                        ContentBlock::Image { .. } => {
                            output.push_str("[image]");
                        }
                        ContentBlock::ToolUse { block } => {
                            output
                                .push_str(&format!("[tool_use: {}({})]", block.name, block.input));
                        }
                    }
                }
                output.push('\n');
            }
            Message::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let error_marker = if *is_error { " ERROR" } else { "" };
                output.push_str(&format!(
                    "[ToolResult{}({})]: {}",
                    error_marker, tool_use_id, content
                ));
                output.push('\n');
            }
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::config::CompactionLayerConfig;
    use crate::error::ModelError;
    use crate::message::ToolUseBlock;
    use crate::message::Usage;
    use crate::model::ModelStream;
    use crate::stream::StopReason;
    use async_trait::async_trait;
    use serde_json::json;

    /// Helper to create a system message.
    fn system_msg(content: &str) -> Message {
        Message::System {
            content: content.to_string(),
        }
    }

    /// Helper to create a user message with text.
    fn user_msg(text: &str) -> Message {
        Message::User {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    /// Helper to create an assistant message with text.
    fn assistant_msg(text: &str) -> Message {
        Message::Assistant {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            usage: None,
        }
    }

    /// Helper to create an assistant message with a tool use.
    fn assistant_tool_use(tool_id: &str, tool_name: &str) -> Message {
        Message::Assistant {
            content: vec![ContentBlock::ToolUse {
                block: ToolUseBlock {
                    id: tool_id.to_string(),
                    name: tool_name.to_string(),
                    input: json!({}),
                },
            }],
            usage: None,
        }
    }

    /// Helper to create a tool result message.
    fn tool_result(tool_use_id: &str, content: &str) -> Message {
        Message::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error: false,
        }
    }

    // ========================================================================
    // Tests for ensure_pair_integrity
    // ========================================================================

    #[test]
    fn test_pair_integrity_no_tool_messages() {
        let messages = vec![
            user_msg("Hello"),
            assistant_msg("Hi"),
            user_msg("Bye"),
            assistant_msg("Goodbye"),
        ];
        let indices = vec![0, 1]; // summarize first two
        let result = ensure_pair_integrity(&messages, &indices);
        assert_eq!(result, vec![0, 1]);
    }

    #[test]
    fn test_pair_integrity_tool_result_at_boundary_pulled_in() {
        // Scenario: first preserved message is a ToolResult,
        // so we need to pull it and its matching ToolUse into the summarize set.
        let messages = vec![
            user_msg("Turn 1"),                    // 0
            assistant_tool_use("t1", "file_read"), // 1
            tool_result("t1", "file content"),     // 2
            user_msg("Turn 2"),                    // 3
            assistant_msg("response"),             // 4
        ];
        // If we try to summarize indices [0] (just the first user msg),
        // the first preserved non-system is index 1 (assistant tool use), not a ToolResult, so no change.
        let indices = vec![0];
        let result = ensure_pair_integrity(&messages, &indices);
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn test_pair_integrity_tool_result_is_first_preserved() {
        // Scenario: indices to summarize = [0, 1], first preserved = [2] which is a ToolResult
        let messages = vec![
            user_msg("Turn 1"),                    // 0
            assistant_tool_use("t1", "file_read"), // 1
            tool_result("t1", "file content"),     // 2
            user_msg("Turn 2"),                    // 3
            assistant_msg("response"),             // 4
        ];
        let indices = vec![0, 1]; // summarize user msg + assistant tool use
        let result = ensure_pair_integrity(&messages, &indices);
        // ToolResult at index 2 is first preserved non-system, and it's a ToolResult,
        // so we pull it in along with its ToolUse (index 1, already included).
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn test_pair_integrity_last_is_assistant_tool_use() {
        // Scenario: last summarized message is an assistant ToolUse, so we
        // also pull in the matching ToolResult.
        let messages = vec![
            user_msg("Turn 1"),                    // 0
            assistant_tool_use("t1", "file_read"), // 1
            tool_result("t1", "file content"),     // 2
            user_msg("Turn 2"),                    // 3
            assistant_msg("response"),             // 4
        ];
        let indices = vec![0, 1]; // summarize [user, assistant_tool_use]
                                  // The last item (index 1) is an Assistant with ToolUse.
                                  // Its ToolResult is at index 2 — should be pulled in.
                                  // Then index 2 is a ToolResult which is first preserved... same logic pulls it in.
        let result = ensure_pair_integrity(&messages, &indices);
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn test_pair_integrity_empty_indices() {
        let messages = vec![user_msg("Hello")];
        let indices: Vec<usize> = vec![];
        let result = ensure_pair_integrity(&messages, &indices);
        assert!(result.is_empty());
    }

    // ========================================================================
    // Tests for build_summarization_prompt
    // ========================================================================

    #[test]
    fn test_prompt_contains_all_9_sections() {
        let msg = user_msg("Hello");
        let messages = vec![&msg as &Message];
        let prompt = build_summarization_prompt(&messages);

        assert!(prompt.contains("## 1. Primary Request"));
        assert!(prompt.contains("## 2. Key Concepts"));
        assert!(prompt.contains("## 3. Files and Code State"));
        assert!(prompt.contains("## 4. Errors and Fixes"));
        assert!(prompt.contains("## 5. Problem Solving Context"));
        assert!(prompt.contains("## 6. User Messages Summary"));
        assert!(prompt.contains("## 7. Pending Tasks"));
        assert!(prompt.contains("## 8. Current Work State"));
        assert!(prompt.contains("## 9. Next Step"));
    }

    #[test]
    fn test_prompt_contains_conversation() {
        let msg = user_msg("Fix the bug in auth.rs");
        let msgs: Vec<&Message> = vec![&msg];
        let prompt = build_summarization_prompt(&msgs);
        assert!(prompt.contains("Fix the bug in auth.rs"));
    }

    // ========================================================================
    // Tests for format_messages_for_summary
    // ========================================================================

    #[test]
    fn test_format_user_message() {
        let msg = user_msg("Hello world");
        let formatted = format_messages_for_summary(&[&msg]);
        assert!(formatted.contains("[User]: Hello world"));
    }

    #[test]
    fn test_format_assistant_message() {
        let msg = assistant_msg("Let me help");
        let formatted = format_messages_for_summary(&[&msg]);
        assert!(formatted.contains("[Assistant]: Let me help"));
    }

    #[test]
    fn test_format_system_message() {
        let msg = system_msg("Be helpful");
        let formatted = format_messages_for_summary(&[&msg]);
        assert!(formatted.contains("[System]: Be helpful"));
    }

    #[test]
    fn test_format_tool_result() {
        let msg = tool_result("t1", "some output");
        let formatted = format_messages_for_summary(&[&msg]);
        assert!(formatted.contains("[ToolResult(t1)]: some output"));
    }

    #[test]
    fn test_format_tool_result_error() {
        let msg = Message::ToolResult {
            tool_use_id: "t1".to_string(),
            content: "permission denied".to_string(),
            is_error: true,
        };
        let formatted = format_messages_for_summary(&[&msg]);
        assert!(formatted.contains("[ToolResult ERROR(t1)]: permission denied"));
    }

    #[test]
    fn test_format_assistant_with_tool_use() {
        let msg = assistant_tool_use("t1", "file_read");
        let formatted = format_messages_for_summary(&[&msg]);
        assert!(formatted.contains("[Assistant]:"));
        assert!(formatted.contains("[tool_use: file_read"));
    }

    // ========================================================================
    // Mock Model implementations for property tests
    // ========================================================================

    /// A mock model that always returns a fixed summary text.
    struct SuccessMockModel {
        summary_text: String,
    }

    impl SuccessMockModel {
        fn new(summary: &str) -> Self {
            Self {
                summary_text: summary.to_string(),
            }
        }
    }

    #[async_trait]
    impl Model for SuccessMockModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            unimplemented!("SuccessMockModel only supports complete()")
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                content: vec![crate::model::ContentBlock::Text {
                    text: self.summary_text.clone(),
                }],
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: None,
                },
                stop_reason: StopReason::EndTurn,
            })
        }

        fn name(&self) -> &str {
            "success-mock-model"
        }
        fn provider(&self) -> &str {
            "mock"
        }
        fn context_window(&self) -> usize {
            128000
        }
        fn max_output_tokens(&self) -> usize {
            4096
        }
        fn supports_tools(&self) -> bool {
            false
        }
        fn input_cost_per_million(&self) -> f64 {
            0.0
        }
        fn output_cost_per_million(&self) -> f64 {
            0.0
        }
    }

    /// A mock model that always returns an error (simulating model failure).
    struct FailMockModel;

    #[async_trait]
    impl Model for FailMockModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            unimplemented!("FailMockModel only supports complete()")
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            Err(ModelError::Connection(
                "simulated model failure".to_string(),
            ))
        }

        fn name(&self) -> &str {
            "fail-mock-model"
        }
        fn provider(&self) -> &str {
            "mock"
        }
        fn context_window(&self) -> usize {
            128000
        }
        fn max_output_tokens(&self) -> usize {
            4096
        }
        fn supports_tools(&self) -> bool {
            false
        }
        fn input_cost_per_million(&self) -> f64 {
            0.0
        }
        fn output_cost_per_million(&self) -> f64 {
            0.0
        }
    }

    /// Helper to build a CompactionContext for tests.
    fn test_compaction_context(preserve_recent: usize) -> CompactionContext {
        CompactionContext {
            token_count: 200_000,
            trigger_threshold: 167_000,
            current_turn: 20,
            config: CompactionLayerConfig {
                preserve_recent_messages: preserve_recent,
                ..CompactionLayerConfig::default()
            },
        }
    }

    // ========================================================================
    // Property-based tests for FullSummarizeLayer
    // Validates: Requirements 4.3, 4.4, 4.6
    // ========================================================================

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// Strategy to generate a simple non-system message (user or assistant text).
        fn arb_non_system_message(idx: usize) -> impl Strategy<Value = Message> {
            prop_oneof![
                Just(user_msg(&format!("User message {}", idx))),
                Just(assistant_msg(&format!("Assistant response {}", idx))),
            ]
        }

        /// Strategy to generate a vector of non-system messages with length > N.
        /// This ensures there are enough messages to trigger summarization.
        fn arb_messages_exceeding(min_non_system: usize) -> impl Strategy<Value = Vec<Message>> {
            // Generate between min_non_system+1 and min_non_system+20 non-system messages
            let count_range = (min_non_system + 1)..=(min_non_system + 20);
            count_range.prop_flat_map(move |count| {
                let strategies: Vec<_> = (0..count).map(arb_non_system_message).collect();
                strategies
            })
        }

        /// Strategy for preserve_recent_messages config value (2-10).
        fn arb_preserve_recent() -> impl Strategy<Value = usize> {
            2usize..=10
        }

        // ====================================================================
        // **Property 7: Full Summarize preserves recent messages verbatim**
        //
        // For any message history processed by the Full Summarize layer, the
        // last N non-system messages (configurable, default 10) SHALL remain in
        // the history unchanged, and all summarized messages SHALL be replaced
        // by exactly one system message.
        //
        // **Validates: Requirements 4.3, 4.4, 4.6**
        // ====================================================================
        proptest! {
            #[test]
            fn prop_full_summarize_preserves_recent_messages(
                non_system_msgs in arb_messages_exceeding(10),
                preserve_recent in arb_preserve_recent(),
            ) {
                // Use tokio runtime to run the async apply_async method
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    // Optionally prepend a system message
                    let mut messages = vec![
                        Message::System { content: "You are a helpful assistant.".to_string() },
                    ];
                    messages.extend(non_system_msgs.clone());

                    // Snapshot the last N non-system messages before applying
                    let non_system_before: Vec<&Message> = messages
                        .iter()
                        .filter(|m| !matches!(m, Message::System { .. }))
                        .collect();
                    let total_non_system = non_system_before.len();

                    // Guard: only proceed if we have more non-system msgs than preserve_recent
                    if total_non_system <= preserve_recent {
                        return Ok(());
                    }

                    let expected_preserved: Vec<Message> = non_system_before
                        [total_non_system - preserve_recent..]
                        .iter()
                        .map(|m| (*m).clone())
                        .collect();

                    let ctx = test_compaction_context(preserve_recent);
                    let model = SuccessMockModel::new("This is a test summary of the conversation.");
                    let layer = FullSummarizeLayer;

                    let result = layer.apply_async(&mut messages, &ctx, &model).await;

                    // The layer should have successfully applied
                    match &result {
                        LayerResult::Applied(event) => {
                            prop_assert_eq!(event.stage.as_str(), "full_summarize");
                            prop_assert!(event.messages_affected > 0);
                        }
                        other => {
                            prop_assert!(false, "Expected Applied, got {:?}", other);
                        }
                    }

                    // Verify: the last N non-system messages in the result are preserved verbatim
                    let non_system_after: Vec<&Message> = messages
                        .iter()
                        .filter(|m| !matches!(m, Message::System { .. }))
                        .collect();

                    prop_assert_eq!(
                        non_system_after.len(),
                        preserve_recent,
                        "After summarization, there should be exactly {} non-system messages, got {}",
                        preserve_recent,
                        non_system_after.len()
                    );

                    for (i, (expected, actual)) in expected_preserved.iter().zip(non_system_after.iter()).enumerate() {
                        prop_assert_eq!(
                            expected, *actual,
                            "Preserved message at position {} was modified", i
                        );
                    }

                    // Verify: exactly one "[Conversation Summary]" system message was inserted
                    let summary_msgs: Vec<&Message> = messages
                        .iter()
                        .filter(|m| matches!(m, Message::System { content } if content.contains("[Conversation Summary]")))
                        .collect();
                    prop_assert_eq!(
                        summary_msgs.len(),
                        1,
                        "Expected exactly 1 summary system message, got {}",
                        summary_msgs.len()
                    );

                    Ok(())
                })?;
            }
        }

        // ====================================================================
        // **Property 8: Full Summarize failure leaves messages unchanged**
        //
        // For any message history where the Full Summarize model call fails,
        // the message history SHALL be identical before and after the failed
        // attempt — no messages removed, no messages added.
        //
        // **Validates: Requirements 4.3, 4.4, 4.6**
        // ====================================================================
        proptest! {
            #[test]
            fn prop_full_summarize_failure_leaves_messages_unchanged(
                non_system_msgs in arb_messages_exceeding(10),
                preserve_recent in arb_preserve_recent(),
            ) {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    // Build messages with a system message prefix
                    let mut messages = vec![
                        Message::System { content: "You are a helpful assistant.".to_string() },
                    ];
                    messages.extend(non_system_msgs.clone());

                    // Ensure we have enough to trigger summarization
                    let non_system_count = messages
                        .iter()
                        .filter(|m| !matches!(m, Message::System { .. }))
                        .count();
                    if non_system_count <= preserve_recent {
                        return Ok(());
                    }

                    // Take a complete snapshot of messages before applying
                    let messages_before = messages.clone();

                    let ctx = test_compaction_context(preserve_recent);
                    let model = FailMockModel;
                    let layer = FullSummarizeLayer;

                    let result = layer.apply_async(&mut messages, &ctx, &model).await;

                    // The layer should have returned Failed
                    match &result {
                        LayerResult::Failed(reason) => {
                            prop_assert!(reason.contains("model call failed"),
                                "Expected 'model call failed' in reason, got: {}", reason);
                        }
                        other => {
                            prop_assert!(false, "Expected Failed, got {:?}", other);
                        }
                    }

                    // Verify: messages are completely unchanged
                    prop_assert_eq!(
                        messages.len(),
                        messages_before.len(),
                        "Message count changed after failed summarization: before={}, after={}",
                        messages_before.len(),
                        messages.len()
                    );

                    for (i, (before, after)) in messages_before.iter().zip(messages.iter()).enumerate() {
                        prop_assert_eq!(
                            before, after,
                            "Message at index {} was modified after failed summarization", i
                        );
                    }

                    Ok(())
                })?;
            }
        }
    }
}
