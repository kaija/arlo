//! Integration tests for the full compaction pipeline.
//!
//! **Validates: Requirements 8.1, 8.3, 10.2, 11.1, 11.2, 11.3, 11.4**
//!
//! Properties tested:
//! - Property 12: Successful compaction produces a complete event and updates state
//! - Property 13: Compaction preserves relative message ordering
//! - Property 14: System messages are never removed by compaction
//! - Property 15: Summary insertion position is after last system-instruction message
//! - Property 16: Tool-use/tool-result pairs are preserved intact in retained messages

use proptest::prelude::*;
use serde_json::json;

use agent_core::{
    CompactionLayerConfig, CompactionPipeline, CompactionState, ContentBlock, Message,
    ToolUseBlock,
};
use agent_core::compaction::tokens::estimate_tokens;

// ============================================================================
// Helpers
// ============================================================================

fn system_msg(content: &str) -> Message {
    Message::System {
        content: content.to_string(),
    }
}

fn user_msg(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
    }
}

fn assistant_msg(text: &str) -> Message {
    Message::Assistant {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        usage: None,
    }
}

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

fn tool_result_msg(tool_use_id: &str, content: &str) -> Message {
    Message::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: content.to_string(),
        is_error: false,
    }
}

/// Create messages with old tool results that tools_compact can clear.
/// Each old turn has a user msg + assistant tool_use + tool_result with big content.
fn messages_with_clearable_tool_results(
    num_old_turns: u32,
    content_size: usize,
) -> Vec<Message> {
    let big_content = "x".repeat(content_size);
    let mut messages = Vec::new();

    // System instruction at the top
    messages.push(system_msg("You are a helpful assistant."));

    // Old turns with compactable tool results
    for i in 0..num_old_turns {
        messages.push(user_msg(&format!("Old turn {}", i)));
        let tid = format!("t_{}", i);
        messages.push(assistant_tool_use(&tid, "file_read"));
        messages.push(tool_result_msg(&tid, &big_content));
    }

    // Recent exempt turns (6 user messages to stay above default 5 exempt)
    for i in 0..6 {
        messages.push(user_msg(&format!("Recent turn {}", i)));
        messages.push(assistant_msg(&format!("Recent response {}", i)));
    }

    messages
}

/// Helper to compute pipeline parameters so that the threshold is below current tokens.
/// Returns (context_window, max_output_tokens) such that the pipeline will trigger.
fn trigger_params(token_count: usize) -> (usize, usize) {
    // threshold = context_window - max(max_output_tokens, 20000) - 13000
    // We want threshold < token_count.
    // Pick threshold = token_count - 1000
    let target_threshold = token_count.saturating_sub(1000);
    let context_window = target_threshold + 20_000 + 13_000;
    let max_output_tokens = 8192;
    (context_window, max_output_tokens)
}

// ============================================================================
// Property 12: Successful compaction produces a complete event and updates state
//
// For any successful compaction operation, the returned CompactionEvent SHALL
// contain a non-empty layer name, a positive messages_affected count, and valid
// tokens_before/tokens_after values. The CompactionState SHALL have
// total_compactions incremented by 1, messages_removed incremented by
// messages_affected, and last_compaction_turn set to the current turn.
//
// **Validates: Requirements 8.1, 8.3, 10.2**
// ============================================================================

#[tokio::test]
async fn test_property12_successful_compaction_event_and_state() {
    let mut messages = messages_with_clearable_tool_results(10, 2000);
    let mut state = CompactionState::default();
    let config = CompactionLayerConfig::default();
    let mut pipeline = CompactionPipeline::new(config);

    let token_count = estimate_tokens(&messages);
    let (context_window, max_output_tokens) = trigger_params(token_count);
    let current_turn = 15u32;

    let result = pipeline
        .compact(
            &mut messages,
            &mut state,
            token_count,
            context_window,
            max_output_tokens,
            current_turn,
            None,
        )
        .await;

    let event = result.expect("Compaction should succeed");

    // Event completeness checks
    assert!(!event.stage.is_empty(), "stage name must be non-empty");
    assert!(event.messages_affected > 0, "messages_affected must be positive");
    assert!(event.tokens_before > 0, "tokens_before must be positive");
    assert!(event.tokens_after < event.tokens_before, "tokens_after < tokens_before");

    // State update checks
    assert_eq!(state.total_compactions, 1);
    assert_eq!(state.messages_removed, event.messages_affected);
    assert_eq!(state.last_compaction_turn, Some(current_turn));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property 12 (proptest): For any number of old tool-result turns (3-20),
    /// successful tools_compact produces a valid event and updates state correctly.
    #[test]
    fn prop_successful_compaction_event_and_state(
        num_old_turns in 6u32..=20,
        content_size in 500usize..=3000,
        current_turn in 10u32..=100,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut messages = messages_with_clearable_tool_results(num_old_turns, content_size);
            let mut state = CompactionState::default();
            let config = CompactionLayerConfig::default();
            let mut pipeline = CompactionPipeline::new(config);

            let token_count = estimate_tokens(&messages);
            let (context_window, max_output_tokens) = trigger_params(token_count);

            let result = pipeline
                .compact(
                    &mut messages,
                    &mut state,
                    token_count,
                    context_window,
                    max_output_tokens,
                    current_turn,
                    None,
                )
                .await;

            let event = result.expect("Compaction should succeed with clearable results");

            // Property: event fields are valid
            prop_assert!(!event.stage.is_empty());
            prop_assert!(event.messages_affected > 0);
            prop_assert!(event.tokens_before > 0);
            prop_assert!(event.tokens_after <= event.tokens_before);

            // Property: state is updated
            prop_assert_eq!(state.total_compactions, 1u32);
            prop_assert_eq!(state.messages_removed, event.messages_affected);
            prop_assert_eq!(state.last_compaction_turn, Some(current_turn));

            Ok(())
        })?;
    }
}

// ============================================================================
// Property 13: Compaction preserves relative message ordering
//
// For any message history before and after compaction, the subset of messages
// that exist in both (not removed or replaced) SHALL appear in the same
// relative order.
//
// **Validates: Requirements 11.1**
// ============================================================================

#[tokio::test]
async fn test_property13_relative_ordering_preserved() {
    let mut messages = messages_with_clearable_tool_results(10, 2000);
    let messages_before = messages.clone();

    let mut state = CompactionState::default();
    let config = CompactionLayerConfig::default();
    let mut pipeline = CompactionPipeline::new(config);

    let token_count = estimate_tokens(&messages);
    let (context_window, max_output_tokens) = trigger_params(token_count);

    pipeline
        .compact(
            &mut messages,
            &mut state,
            token_count,
            context_window,
            max_output_tokens,
            10,
            None,
        )
        .await;

    // Find messages that exist in both before and after (by equality).
    // Their relative order must be preserved.
    let common_after: Vec<&Message> = messages
        .iter()
        .filter(|m| messages_before.contains(m))
        .collect();

    // Verify that for every pair in common_after, their order in messages_before
    // matches their order in common_after.
    for i in 0..common_after.len() {
        for j in (i + 1)..common_after.len() {
            let pos_i = messages_before
                .iter()
                .position(|m| m == common_after[i])
                .unwrap();
            let pos_j = messages_before
                .iter()
                .position(|m| m == common_after[j])
                .unwrap();
            assert!(
                pos_i < pos_j,
                "Relative order violated: message at after-index {} was at before-pos {}, \
                 message at after-index {} was at before-pos {}",
                i, pos_i, j, pos_j
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property 13 (proptest): For any conversation with clearable tool results,
    /// messages retained after compaction appear in the same relative order as before.
    #[test]
    fn prop_relative_ordering_preserved(
        num_old_turns in 6u32..=15,
        content_size in 500usize..=2000,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut messages = messages_with_clearable_tool_results(num_old_turns, content_size);
            let messages_before = messages.clone();
            let mut state = CompactionState::default();
            let config = CompactionLayerConfig::default();
            let mut pipeline = CompactionPipeline::new(config);

            let token_count = estimate_tokens(&messages);
            let (context_window, max_output_tokens) = trigger_params(token_count);

            pipeline
                .compact(
                    &mut messages,
                    &mut state,
                    token_count,
                    context_window,
                    max_output_tokens,
                    10,
                    None,
                )
                .await;

            // Messages in 'after' that also exist in 'before' must maintain order.
            // For tools_compact, messages are not removed—only content is cleared—
            // so message count stays the same, and all messages are "common".
            // Their positional order must be identical.
            prop_assert_eq!(
                messages.len(),
                messages_before.len(),
                "Tools Compact should not add or remove messages"
            );

            // Verify ordering: index i in after corresponds to index i in before
            // (since tools_compact only mutates content in-place).
            for (i, (before, after)) in messages_before.iter().zip(messages.iter()).enumerate() {
                // For non-ToolResult messages, they should be identical.
                match (before, after) {
                    (Message::ToolResult { tool_use_id: id_b, .. },
                     Message::ToolResult { tool_use_id: id_a, .. }) => {
                        prop_assert_eq!(id_b, id_a,
                            "ToolResult at position {} changed tool_use_id", i);
                    }
                    _ => {
                        prop_assert_eq!(before, after,
                            "Non-ToolResult message at position {} changed", i);
                    }
                }
            }

            Ok(())
        })?;
    }
}

// ============================================================================
// Property 14: System messages are never removed by compaction
//
// For any compaction operation (across all layers), every System message present
// before compaction SHALL still be present after compaction. Compaction may add
// new system messages (summaries) but SHALL NOT remove existing ones.
//
// **Validates: Requirements 11.2**
// ============================================================================

#[tokio::test]
async fn test_property14_system_messages_preserved_tools_compact() {
    let mut messages = vec![
        system_msg("System instruction 1"),
        system_msg("System instruction 2"),
    ];
    // Add old tool results that can be cleared
    for i in 0..10u32 {
        messages.push(user_msg(&format!("Turn {}", i)));
        let tid = format!("t_{}", i);
        messages.push(assistant_tool_use(&tid, "shell"));
        messages.push(tool_result_msg(&tid, &"y".repeat(2000)));
    }
    // Recent turns
    for i in 0..6 {
        messages.push(user_msg(&format!("Recent {}", i)));
        messages.push(assistant_msg(&format!("Response {}", i)));
    }

    let system_msgs_before: Vec<String> = messages
        .iter()
        .filter_map(|m| match m {
            Message::System { content } => Some(content.clone()),
            _ => None,
        })
        .collect();

    let mut state = CompactionState::default();
    let config = CompactionLayerConfig::default();
    let mut pipeline = CompactionPipeline::new(config);

    let token_count = estimate_tokens(&messages);
    let (context_window, max_output_tokens) = trigger_params(token_count);

    pipeline
        .compact(
            &mut messages,
            &mut state,
            token_count,
            context_window,
            max_output_tokens,
            15,
            None,
        )
        .await;

    // Every system message from before must still be present
    let system_msgs_after: Vec<String> = messages
        .iter()
        .filter_map(|m| match m {
            Message::System { content } => Some(content.clone()),
            _ => None,
        })
        .collect();

    for sys_msg in &system_msgs_before {
        assert!(
            system_msgs_after.contains(sys_msg),
            "System message was removed by compaction: {:?}",
            &sys_msg[..sys_msg.len().min(50)]
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property 14 (proptest): For any number of system messages scattered
    /// throughout the history, none are removed after compaction.
    #[test]
    fn prop_system_messages_never_removed(
        num_system_msgs in 1usize..=5,
        num_old_turns in 6u32..=12,
        content_size in 500usize..=2000,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut messages = Vec::new();

            // Add system messages at the beginning
            for i in 0..num_system_msgs {
                messages.push(system_msg(&format!("System instruction {}", i)));
            }

            // Add old turns with clearable tool results
            for i in 0..num_old_turns {
                messages.push(user_msg(&format!("Old turn {}", i)));
                let tid = format!("t_{}", i);
                messages.push(assistant_tool_use(&tid, "grep"));
                messages.push(tool_result_msg(&tid, &"a".repeat(content_size)));
            }

            // Add recent turns
            for i in 0..6 {
                messages.push(user_msg(&format!("Recent {}", i)));
                messages.push(assistant_msg(&format!("Resp {}", i)));
            }

            let system_msgs_before: Vec<String> = messages
                .iter()
                .filter_map(|m| match m {
                    Message::System { content } => Some(content.clone()),
                    _ => None,
                })
                .collect();

            let mut state = CompactionState::default();
            let config = CompactionLayerConfig::default();
            let mut pipeline = CompactionPipeline::new(config);

            let token_count = estimate_tokens(&messages);
            let (context_window, max_output_tokens) = trigger_params(token_count);

            pipeline
                .compact(
                    &mut messages,
                    &mut state,
                    token_count,
                    context_window,
                    max_output_tokens,
                    20,
                    None,
                )
                .await;

            let system_msgs_after: Vec<String> = messages
                .iter()
                .filter_map(|m| match m {
                    Message::System { content } => Some(content.clone()),
                    _ => None,
                })
                .collect();

            for original in &system_msgs_before {
                prop_assert!(
                    system_msgs_after.contains(original),
                    "System message removed: {}",
                    &original[..original.len().min(40)]
                );
            }

            Ok(())
        })?;
    }
}

// ============================================================================
// Property 15: Summary insertion position is after last system-instruction message
//
// For any compaction that replaces messages with a summary, the summary system
// message SHALL be inserted immediately after the last original system message.
//
// **Validates: Requirements 11.3**
// ============================================================================

#[tokio::test]
async fn test_property15_summary_insertion_after_last_system_msg() {
    // Use session_memory layer which inserts a summary system message.
    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut memory_file = NamedTempFile::new().unwrap();
    write!(memory_file, "This is the session memory summary content.").unwrap();

    let mut config = CompactionLayerConfig::default();
    config.session_memory_path = Some(memory_file.path().to_path_buf());
    config.session_memory_min_tokens = 1;
    config.session_memory_min_messages = 2;
    config.session_memory_max_preserved_tokens = 100; // very small to force removal

    let mut messages = vec![
        system_msg("First system instruction"),
        system_msg("Second system instruction"),
    ];
    // Add enough messages to pass guards and have messages to remove
    for i in 0..10 {
        messages.push(user_msg(&format!("User message {} with some padding text", i)));
        messages.push(assistant_msg(&format!("Assistant response {} with text", i)));
    }

    let mut state = CompactionState::default();
    let mut pipeline = CompactionPipeline::new(config);

    let token_count = estimate_tokens(&messages);
    let (context_window, max_output_tokens) = trigger_params(token_count);

    let result = pipeline
        .compact(
            &mut messages,
            &mut state,
            token_count,
            context_window,
            max_output_tokens,
            10,
            None,
        )
        .await;

    assert!(result.is_some(), "Compaction should succeed via session_memory");
    let event = result.unwrap();
    assert_eq!(event.stage, "session_memory");

    // Find the summary message
    let summary_idx = messages
        .iter()
        .position(|m| match m {
            Message::System { content } => content.contains("[Session Memory Summary]"),
            _ => false,
        })
        .expect("Summary system message should be present");

    // Find the last original system message (not the summary)
    let last_original_sys_idx = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| match m {
            Message::System { content } => !content.contains("[Session Memory Summary]"),
            _ => false,
        })
        .map(|(idx, _)| idx)
        .last()
        .expect("Original system messages should exist");

    // Summary must be immediately after the last original system message
    assert_eq!(
        summary_idx,
        last_original_sys_idx + 1,
        "Summary (idx {}) should be immediately after last system msg (idx {})",
        summary_idx,
        last_original_sys_idx
    );
}

// ============================================================================
// Property 16: Tool-use/tool-result pairs are preserved intact in retained messages
//
// For any message set preserved after compaction (the "recent" messages kept
// verbatim), if a ToolResult message is present, the Assistant message
// containing the corresponding ToolUse block SHALL also be present.
//
// **Validates: Requirements 11.4**
// ============================================================================

#[tokio::test]
async fn test_property16_tool_pairs_preserved_in_retained_messages() {
    // After tools_compact, all messages remain (just content cleared).
    // Tool-use/tool-result pairs should both still be present.
    let mut messages = vec![
        system_msg("instruction"),
    ];
    for i in 0..10u32 {
        messages.push(user_msg(&format!("Turn {}", i)));
        let tid = format!("tool_{}", i);
        messages.push(assistant_tool_use(&tid, "file_read"));
        messages.push(tool_result_msg(&tid, &"content".repeat(100)));
    }
    // Recent exempt turns
    for i in 0..6 {
        messages.push(user_msg(&format!("Recent {}", i)));
        let tid = format!("recent_tool_{}", i);
        messages.push(assistant_tool_use(&tid, "shell"));
        messages.push(tool_result_msg(&tid, "recent output"));
    }

    let mut state = CompactionState::default();
    let config = CompactionLayerConfig::default();
    let mut pipeline = CompactionPipeline::new(config);

    let token_count = estimate_tokens(&messages);
    let (context_window, max_output_tokens) = trigger_params(token_count);

    pipeline
        .compact(
            &mut messages,
            &mut state,
            token_count,
            context_window,
            max_output_tokens,
            20,
            None,
        )
        .await;

    // For every ToolResult in the final messages, verify the corresponding
    // ToolUse (by tool_use_id) exists in an Assistant message.
    let tool_use_ids_in_assistants: Vec<String> = messages
        .iter()
        .filter_map(|m| match m {
            Message::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { block } => Some(block.id.clone()),
            _ => None,
        })
        .collect();

    for msg in &messages {
        if let Message::ToolResult { tool_use_id, .. } = msg {
            assert!(
                tool_use_ids_in_assistants.contains(tool_use_id),
                "ToolResult with id '{}' has no corresponding ToolUse in any Assistant message",
                tool_use_id
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property 16 (proptest): For any conversation with tool-use/tool-result pairs,
    /// after tools_compact, all pairs remain intact (both ToolUse and ToolResult present).
    #[test]
    fn prop_tool_pairs_preserved_after_compaction(
        num_tool_turns in 6u32..=15,
        content_size in 200usize..=1500,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut messages = vec![system_msg("instruction")];

            // Build tool-use/tool-result pairs in old turns
            for i in 0..num_tool_turns {
                messages.push(user_msg(&format!("Turn {}", i)));
                let tid = format!("tool_{}", i);
                messages.push(assistant_tool_use(&tid, "file_read"));
                messages.push(tool_result_msg(&tid, &"x".repeat(content_size)));
            }
            // Recent exempt turns
            for i in 0..6 {
                messages.push(user_msg(&format!("Recent {}", i)));
                messages.push(assistant_msg(&format!("Resp {}", i)));
            }

            let mut state = CompactionState::default();
            let config = CompactionLayerConfig::default();
            let mut pipeline = CompactionPipeline::new(config);

            let token_count = estimate_tokens(&messages);
            let (context_window, max_output_tokens) = trigger_params(token_count);

            pipeline
                .compact(
                    &mut messages,
                    &mut state,
                    token_count,
                    context_window,
                    max_output_tokens,
                    20,
                    None,
                )
                .await;

            // Collect all ToolUse IDs from Assistant messages
            let tool_use_ids: std::collections::HashSet<String> = messages
                .iter()
                .filter_map(|m| match m {
                    Message::Assistant { content, .. } => Some(content),
                    _ => None,
                })
                .flatten()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { block } => Some(block.id.clone()),
                    _ => None,
                })
                .collect();

            // Every ToolResult must have a matching ToolUse
            for msg in &messages {
                if let Message::ToolResult { tool_use_id, .. } = msg {
                    prop_assert!(
                        tool_use_ids.contains(tool_use_id),
                        "ToolResult '{}' has no matching ToolUse",
                        tool_use_id
                    );
                }
            }

            Ok(())
        })?;
    }
}
