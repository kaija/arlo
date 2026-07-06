//! Tools Compact layer implementation.
//!
//! Clears stale tool result content from compactable tools without any model call.
//! This is the lightest compaction layer — it only replaces the `content` field of
//! old tool results with `"[content cleared]"`, preserving `tool_use_id` and `is_error`.

use std::collections::HashMap;

use crate::message::{ContentBlock, Message};

use super::layer::{CompactionContext, CompactionLayer, LayerResult};
use super::tokens::estimate_tokens;
use super::CompactionEvent;

/// Layer 1: Clear stale tool result content from compactable tools.
///
/// Walks the message history, identifies tool results that:
/// 1. Belong to a compactable tool (matched by tool name from preceding Assistant ToolUse blocks)
/// 2. Are older than the exempt boundary (determined by user message count)
///
/// Eligible results have their `content` replaced with `"[content cleared]"`.
pub struct ToolsCompactLayer;

impl CompactionLayer for ToolsCompactLayer {
    fn apply(
        &self,
        messages: &mut Vec<Message>,
        context: &CompactionContext,
    ) -> LayerResult {
        let config = &context.config;

        // Build a map of tool_use_id -> tool_name from Assistant messages.
        // This lets us determine which tool produced each ToolResult.
        let tool_use_names: HashMap<String, String> = messages
            .iter()
            .filter_map(|msg| match msg {
                Message::Assistant { content, .. } => Some(content),
                _ => None,
            })
            .flatten()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { block } => Some((block.id.clone(), block.name.clone())),
                _ => None,
            })
            .collect();

        // Count total User messages to determine the exempt boundary.
        // The most recent `tools_compact_exempt_turns` user messages mark the exempt zone.
        // Tool results whose turn (user_msg_count at that point) is <= exempt_start_turn
        // are eligible for clearing.
        let total_user_msgs = messages
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count() as u32;

        let exempt_start_turn =
            total_user_msgs.saturating_sub(config.tools_compact_exempt_turns);

        // If exempt_start_turn is 0, there are no old-enough messages to clear.
        if exempt_start_turn == 0 {
            return LayerResult::Noop;
        }

        let tokens_before = estimate_tokens(messages);
        let mut cleared_count = 0;

        // Walk messages in order, tracking the user message count as a "turn clock".
        // A ToolResult is considered to be in the turn defined by the number of User
        // messages seen so far (i.e., it follows the most recent User message).
        let mut user_msg_count: u32 = 0;
        for msg in messages.iter_mut() {
            if matches!(msg, Message::User { .. }) {
                user_msg_count += 1;
            }

            if let Message::ToolResult {
                tool_use_id,
                content,
                ..
            } = msg
            {
                // Check if this tool result is from a compactable tool
                let is_compactable = tool_use_names
                    .get(tool_use_id.as_str())
                    .map(|name| config.compactable_tools.contains(name))
                    .unwrap_or(false);

                // Check if it's in the old zone (before exempt boundary)
                let is_old = user_msg_count <= exempt_start_turn;

                // Check if already cleared
                let already_cleared = content == "[content cleared]";

                if is_compactable && is_old && !already_cleared {
                    *content = "[content cleared]".to_string();
                    cleared_count += 1;
                }
            }
        }

        if cleared_count == 0 {
            return LayerResult::Noop;
        }

        let tokens_after = estimate_tokens(messages);
        LayerResult::Applied(CompactionEvent {
            stage: "tools_compact".to_string(),
            messages_affected: cleared_count,
            tokens_before,
            tokens_after,
        })
    }

    fn name(&self) -> &str {
        "tools_compact"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolUseBlock;
    use serde_json::json;

    use super::super::config::CompactionLayerConfig;
    use super::super::layer::CompactionContext;

    /// Helper to create a default context for tests.
    fn test_context(exempt_turns: u32) -> CompactionContext {
        let mut config = CompactionLayerConfig::default();
        config.tools_compact_exempt_turns = exempt_turns;
        CompactionContext {
            token_count: 100_000,
            trigger_threshold: 80_000,
            current_turn: 10,
            config,
        }
    }

    /// Helper to create a User message with text.
    fn user_msg(text: &str) -> Message {
        Message::User {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    /// Helper to create an Assistant message with a ToolUse block.
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

    /// Helper to create a ToolResult message.
    fn tool_result(tool_use_id: &str, content: &str) -> Message {
        Message::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error: false,
        }
    }

    #[test]
    fn test_noop_when_no_tool_results() {
        let mut messages = vec![
            user_msg("Hello"),
            Message::Assistant {
                content: vec![ContentBlock::Text {
                    text: "Hi there!".to_string(),
                }],
                usage: None,
            },
        ];

        let ctx = test_context(5);
        let layer = ToolsCompactLayer;
        let result = layer.apply(&mut messages, &ctx);
        assert_eq!(result, LayerResult::Noop);
    }

    #[test]
    fn test_noop_when_all_results_in_exempt_zone() {
        // Only 3 user messages, exempt_turns = 5, so exempt_start_turn = 0
        // meaning nothing is old enough
        let mut messages = vec![
            user_msg("Turn 1"),
            assistant_tool_use("t1", "file_read"),
            tool_result("t1", "file contents here"),
            user_msg("Turn 2"),
            assistant_tool_use("t2", "shell"),
            tool_result("t2", "command output"),
            user_msg("Turn 3"),
        ];

        let ctx = test_context(5);
        let layer = ToolsCompactLayer;
        let result = layer.apply(&mut messages, &ctx);
        assert_eq!(result, LayerResult::Noop);
    }

    #[test]
    fn test_clears_old_compactable_results() {
        // 7 user messages, exempt_turns = 2, so exempt_start_turn = 5
        // Tool results in turns 1-5 are eligible for clearing
        let mut messages = vec![
            user_msg("Turn 1"),
            assistant_tool_use("t1", "file_read"),
            tool_result("t1", "long file contents here that take up space"),
            user_msg("Turn 2"),
            assistant_tool_use("t2", "shell"),
            tool_result("t2", "ls output with many files listed"),
            user_msg("Turn 3"),
            user_msg("Turn 4"),
            user_msg("Turn 5"),
            user_msg("Turn 6"),
            assistant_tool_use("t3", "grep"),
            tool_result("t3", "recent grep results"),
            user_msg("Turn 7"),
        ];

        let ctx = test_context(2);
        let layer = ToolsCompactLayer;
        let result = layer.apply(&mut messages, &ctx);

        // t1 and t2 should be cleared (turns 1 and 2, both <= 5)
        // t3 follows user_msg 6, so user_msg_count=6 > 5, not cleared
        match result {
            LayerResult::Applied(event) => {
                assert_eq!(event.stage, "tools_compact");
                assert_eq!(event.messages_affected, 2);
            }
            _ => panic!("Expected Applied, got {:?}", result),
        }

        // Verify the content was replaced
        if let Message::ToolResult { content, .. } = &messages[2] {
            assert_eq!(content, "[content cleared]");
        } else {
            panic!("Expected ToolResult at index 2");
        }
        if let Message::ToolResult { content, .. } = &messages[5] {
            assert_eq!(content, "[content cleared]");
        } else {
            panic!("Expected ToolResult at index 5");
        }
        // Recent result should be untouched
        if let Message::ToolResult { content, .. } = &messages[11] {
            assert_eq!(content, "recent grep results");
        } else {
            panic!("Expected ToolResult at index 11");
        }
    }

    #[test]
    fn test_preserves_tool_use_id_and_is_error() {
        let mut messages = vec![
            user_msg("Turn 1"),
            assistant_tool_use("my_tool_id", "file_read"),
            Message::ToolResult {
                tool_use_id: "my_tool_id".to_string(),
                content: "some content".to_string(),
                is_error: true,
            },
            user_msg("Turn 2"),
            user_msg("Turn 3"),
            user_msg("Turn 4"),
            user_msg("Turn 5"),
            user_msg("Turn 6"),
            user_msg("Turn 7"),
        ];

        let ctx = test_context(2);
        let layer = ToolsCompactLayer;
        layer.apply(&mut messages, &ctx);

        if let Message::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = &messages[2]
        {
            assert_eq!(tool_use_id, "my_tool_id");
            assert_eq!(content, "[content cleared]");
            assert_eq!(*is_error, true);
        } else {
            panic!("Expected ToolResult at index 2");
        }
    }

    #[test]
    fn test_skips_non_compactable_tools() {
        // "custom_tool" is not in the default compactable_tools list
        let mut messages = vec![
            user_msg("Turn 1"),
            assistant_tool_use("t1", "custom_tool"),
            tool_result("t1", "custom tool output"),
            user_msg("Turn 2"),
            user_msg("Turn 3"),
            user_msg("Turn 4"),
            user_msg("Turn 5"),
            user_msg("Turn 6"),
            user_msg("Turn 7"),
        ];

        let ctx = test_context(2);
        let layer = ToolsCompactLayer;
        let result = layer.apply(&mut messages, &ctx);
        assert_eq!(result, LayerResult::Noop);

        // Content should be unchanged
        if let Message::ToolResult { content, .. } = &messages[2] {
            assert_eq!(content, "custom tool output");
        }
    }

    #[test]
    fn test_skips_already_cleared_results() {
        let mut messages = vec![
            user_msg("Turn 1"),
            assistant_tool_use("t1", "file_read"),
            tool_result("t1", "[content cleared]"),
            user_msg("Turn 2"),
            user_msg("Turn 3"),
            user_msg("Turn 4"),
            user_msg("Turn 5"),
            user_msg("Turn 6"),
            user_msg("Turn 7"),
        ];

        let ctx = test_context(2);
        let layer = ToolsCompactLayer;
        let result = layer.apply(&mut messages, &ctx);
        assert_eq!(result, LayerResult::Noop);
    }

    #[test]
    fn test_name_returns_tools_compact() {
        let layer = ToolsCompactLayer;
        assert_eq!(layer.name(), "tools_compact");
    }

    #[test]
    fn test_tokens_before_and_after_in_event() {
        let long_content = "x".repeat(400); // 400 chars = 100 tokens
        let mut messages = vec![
            user_msg("Turn 1"),
            assistant_tool_use("t1", "file_read"),
            tool_result("t1", &long_content),
            user_msg("Turn 2"),
            user_msg("Turn 3"),
            user_msg("Turn 4"),
            user_msg("Turn 5"),
            user_msg("Turn 6"),
            user_msg("Turn 7"),
        ];

        let tokens_before = estimate_tokens(&messages);
        let ctx = test_context(2);
        let layer = ToolsCompactLayer;
        let result = layer.apply(&mut messages, &ctx);

        match result {
            LayerResult::Applied(event) => {
                assert_eq!(event.tokens_before, tokens_before);
                assert!(event.tokens_after < event.tokens_before);
            }
            _ => panic!("Expected Applied"),
        }
    }

    // ========================================================================
    // Property-based tests for ToolsCompactLayer
    // Validates: Requirements 2.1, 2.2, 2.3, 2.5
    // ========================================================================

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// The compactable tool names used in tests (same as default config).
        const COMPACTABLE_TOOLS: &[&str] =
            &["file_read", "shell", "grep", "glob", "web_fetch"];
        const NON_COMPACTABLE_TOOLS: &[&str] =
            &["custom_tool", "my_plugin", "special_op", "analyzer"];

        /// Strategy to generate a tool name, with ~60% chance of being compactable.
        fn arb_tool_name() -> impl Strategy<Value = String> {
            prop_oneof![
                6 => prop::sample::select(COMPACTABLE_TOOLS).prop_map(|s| s.to_string()),
                4 => prop::sample::select(NON_COMPACTABLE_TOOLS).prop_map(|s| s.to_string()),
            ]
        }

        /// Strategy to generate arbitrary tool result content (non-empty, varying length).
        fn arb_content() -> impl Strategy<Value = String> {
            "[a-zA-Z0-9 _./]{1,200}".prop_map(|s| s.to_string())
        }

        /// A single "turn" consists of:
        /// - A User message
        /// - Optionally, one or more tool use/result pairs from the assistant
        #[derive(Debug, Clone)]
        struct Turn {
            user_text: String,
            tool_calls: Vec<(String, String, String, bool)>, // (tool_id, tool_name, content, is_error)
        }

        /// Strategy to generate a single turn with 0-3 tool calls.
        fn arb_turn(turn_index: usize) -> impl Strategy<Value = Turn> {
            let tool_calls = prop::collection::vec(
                (
                    arb_tool_name(),
                    arb_content(),
                    proptest::bool::ANY,
                ),
                0..=3,
            )
            .prop_map(move |calls| {
                calls
                    .into_iter()
                    .enumerate()
                    .map(|(i, (name, content, is_error))| {
                        let id = format!("t_{}_{}", turn_index, i);
                        (id, name, content, is_error)
                    })
                    .collect::<Vec<_>>()
            });

            (Just(format!("User message turn {}", turn_index)), tool_calls).prop_map(
                |(user_text, tool_calls)| Turn {
                    user_text,
                    tool_calls,
                },
            )
        }

        /// Strategy to generate a conversation of 3-15 turns.
        fn arb_conversation() -> impl Strategy<Value = Vec<Turn>> {
            (3usize..=15).prop_flat_map(|num_turns| {
                let strategies: Vec<_> = (0..num_turns).map(|i| arb_turn(i)).collect();
                strategies
            })
        }

        /// Strategy for exempt_turns configuration (1-10).
        fn arb_exempt_turns() -> impl Strategy<Value = u32> {
            1u32..=10
        }

        /// Convert a Vec<Turn> into a Vec<Message>.
        fn turns_to_messages(turns: &[Turn]) -> Vec<Message> {
            let mut messages = Vec::new();
            for turn in turns {
                // User message
                messages.push(user_msg(&turn.user_text));
                // Assistant tool uses + tool results
                for (id, name, content, is_error) in &turn.tool_calls {
                    messages.push(assistant_tool_use(id, name));
                    messages.push(Message::ToolResult {
                        tool_use_id: id.clone(),
                        content: content.clone(),
                        is_error: *is_error,
                    });
                }
            }
            messages
        }

        // ====================================================================
        // **Property 2: ToolsCompact clears only eligible tool results**
        //
        // For any message history containing Tool_Result messages, the
        // ToolsCompact layer SHALL clear the content of a Tool_Result if and
        // only if: (a) the corresponding tool name is in the compactable tools
        // list, AND (b) the tool result is older than the N most recent turns.
        // If no eligible tool results exist, the layer SHALL return a no-op.
        //
        // **Validates: Requirements 2.1, 2.2, 2.3, 2.5**
        // ====================================================================
        proptest! {
            #[test]
            fn prop_tools_compact_clears_only_eligible_tool_results(
                turns in arb_conversation(),
                exempt_turns in arb_exempt_turns(),
            ) {
                let mut messages = turns_to_messages(&turns);
                let ctx = test_context(exempt_turns);

                // Snapshot: record which tool_use_ids should be cleared
                // A tool result is eligible iff:
                //   (a) its tool name is compactable
                //   (b) it occurs in a turn that is "old" (user_msg_count <= total_user_msgs - exempt_turns)
                let total_user_msgs = messages
                    .iter()
                    .filter(|m| matches!(m, Message::User { .. }))
                    .count() as u32;
                let exempt_start_turn = total_user_msgs.saturating_sub(exempt_turns);

                // Build tool_use_id -> name map
                let tool_use_names: std::collections::HashMap<String, String> = messages
                    .iter()
                    .filter_map(|msg| match msg {
                        Message::Assistant { content, .. } => Some(content),
                        _ => None,
                    })
                    .flatten()
                    .filter_map(|block| match block {
                        ContentBlock::ToolUse { block } => {
                            Some((block.id.clone(), block.name.clone()))
                        }
                        _ => None,
                    })
                    .collect();

                // Determine which tool_use_ids are eligible for clearing
                let mut user_msg_count: u32 = 0;
                let mut expected_cleared: std::collections::HashSet<String> = std::collections::HashSet::new();
                let mut expected_not_cleared: std::collections::HashSet<String> = std::collections::HashSet::new();

                for msg in messages.iter() {
                    if matches!(msg, Message::User { .. }) {
                        user_msg_count += 1;
                    }
                    if let Message::ToolResult { tool_use_id, content, .. } = msg {
                        let is_compactable = tool_use_names
                            .get(tool_use_id.as_str())
                            .map(|name| ctx.config.compactable_tools.contains(name))
                            .unwrap_or(false);
                        let is_old = user_msg_count <= exempt_start_turn;
                        let already_cleared = content == "[content cleared]";

                        if is_compactable && is_old && !already_cleared {
                            expected_cleared.insert(tool_use_id.clone());
                        } else {
                            expected_not_cleared.insert(tool_use_id.clone());
                        }
                    }
                }

                // Apply the layer
                let layer = ToolsCompactLayer;
                let result = layer.apply(&mut messages, &ctx);

                // If no eligible results, must be Noop
                if expected_cleared.is_empty() {
                    prop_assert_eq!(result, LayerResult::Noop);
                } else {
                    // Must be Applied
                    match &result {
                        LayerResult::Applied(event) => {
                            prop_assert_eq!(event.messages_affected, expected_cleared.len());
                        }
                        other => {
                            prop_assert!(false, "Expected Applied, got {:?}", other);
                        }
                    }
                }

                // Verify: cleared tool results have placeholder content
                for msg in &messages {
                    if let Message::ToolResult { tool_use_id, content, .. } = msg {
                        if expected_cleared.contains(tool_use_id) {
                            prop_assert_eq!(content.as_str(), "[content cleared]",
                                "Tool result {} should have been cleared", tool_use_id);
                        }
                    }
                }

                // Verify: non-eligible tool results are NOT cleared
                for msg in &messages {
                    if let Message::ToolResult { tool_use_id, content, .. } = msg {
                        if expected_not_cleared.contains(tool_use_id) {
                            prop_assert_ne!(content.as_str(), "[content cleared]",
                                "Tool result {} should NOT have been cleared", tool_use_id);
                        }
                    }
                }
            }
        }

        // ====================================================================
        // **Property 3: ToolsCompact preserves tool_use_id and is_error fields**
        //
        // For any Tool_Result message that is cleared by the ToolsCompact layer,
        // the `tool_use_id` and `is_error` fields SHALL be identical before and
        // after clearing — only the `content` field changes.
        //
        // **Validates: Requirements 2.1, 2.2, 2.3, 2.5**
        // ====================================================================
        proptest! {
            #[test]
            fn prop_tools_compact_preserves_tool_use_id_and_is_error(
                turns in arb_conversation(),
                exempt_turns in arb_exempt_turns(),
            ) {
                let mut messages = turns_to_messages(&turns);
                let ctx = test_context(exempt_turns);

                // Snapshot all ToolResult fields before applying
                #[derive(Debug, Clone)]
                struct ToolResultSnapshot {
                    tool_use_id: String,
                    is_error: bool,
                    index: usize,
                }

                let snapshots: Vec<ToolResultSnapshot> = messages
                    .iter()
                    .enumerate()
                    .filter_map(|(i, msg)| match msg {
                        Message::ToolResult {
                            tool_use_id,
                            is_error,
                            ..
                        } => Some(ToolResultSnapshot {
                            tool_use_id: tool_use_id.clone(),
                            is_error: *is_error,
                            index: i,
                        }),
                        _ => None,
                    })
                    .collect();

                // Apply the layer
                let layer = ToolsCompactLayer;
                layer.apply(&mut messages, &ctx);

                // Verify: tool_use_id and is_error are unchanged for ALL ToolResults
                // (the layer does not add/remove messages, only mutates content)
                let after_snapshots: Vec<ToolResultSnapshot> = messages
                    .iter()
                    .enumerate()
                    .filter_map(|(i, msg)| match msg {
                        Message::ToolResult {
                            tool_use_id,
                            is_error,
                            ..
                        } => Some(ToolResultSnapshot {
                            tool_use_id: tool_use_id.clone(),
                            is_error: *is_error,
                            index: i,
                        }),
                        _ => None,
                    })
                    .collect();

                // Same count of ToolResult messages (none added or removed)
                prop_assert_eq!(snapshots.len(), after_snapshots.len(),
                    "Number of ToolResult messages changed");

                // Each ToolResult preserves its tool_use_id and is_error
                for (before, after) in snapshots.iter().zip(after_snapshots.iter()) {
                    prop_assert_eq!(&before.tool_use_id, &after.tool_use_id,
                        "tool_use_id changed at index {}", before.index);
                    prop_assert_eq!(before.is_error, after.is_error,
                        "is_error changed at index {} for tool_use_id {}",
                        before.index, before.tool_use_id);
                    prop_assert_eq!(before.index, after.index,
                        "ToolResult position changed for {}", before.tool_use_id);
                }
            }
        }
    }
}
