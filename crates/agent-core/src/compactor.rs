//! Multi-stage context compaction for keeping message history within token limits.
//!
//! The `ContextCompactor` executes a pipeline of `CompactionStage`s in order,
//! returning a `CompactionEvent` describing what changed on the first stage that
//! triggers, or `None` if all thresholds are satisfied.

use std::sync::Arc;

use crate::message::Message;

/// Event returned when compaction modifies the message history.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionEvent {
    /// Name of the stage that was applied (e.g., "snip", "truncate_tool_results", "auto_summarize").
    pub stage: String,
    /// Number of messages removed or affected.
    pub messages_affected: usize,
    /// Estimated token count before compaction.
    pub tokens_before: usize,
    /// Estimated token count after compaction.
    pub tokens_after: usize,
}

/// Trait for custom compaction logic.
pub trait CompactionFn: Send + Sync {
    /// Apply custom compaction to the message history.
    /// Returns `Some(CompactionEvent)` if the stage modified messages, `None` otherwise.
    fn apply(&self, messages: &mut Vec<Message>) -> Option<CompactionEvent>;

    /// Name of this custom compaction stage.
    fn name(&self) -> &str;
}

/// A single step in the multi-stage compaction pipeline.
#[derive(Clone)]
pub enum CompactionStage {
    /// Remove oldest non-system messages when token count exceeds the limit.
    /// Preserves all system messages and the most recent user message.
    Snip { max_history_tokens: usize },

    /// Truncate tool result content that exceeds `max_chars`, appending "[truncated]".
    TruncateToolResults { max_chars: usize },

    /// Replace old messages with a summary placeholder when token count exceeds threshold.
    /// Skips if no `summary_model` is configured in `CompactionConfig`.
    AutoSummarize {
        threshold_tokens: usize,
        preserve_recent: usize,
    },

    /// A custom compaction stage with user-defined logic.
    Custom(Arc<dyn CompactionFn>),
}

impl std::fmt::Debug for CompactionStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Snip { max_history_tokens } => f
                .debug_struct("Snip")
                .field("max_history_tokens", max_history_tokens)
                .finish(),
            Self::TruncateToolResults { max_chars } => f
                .debug_struct("TruncateToolResults")
                .field("max_chars", max_chars)
                .finish(),
            Self::AutoSummarize {
                threshold_tokens,
                preserve_recent,
            } => f
                .debug_struct("AutoSummarize")
                .field("threshold_tokens", threshold_tokens)
                .field("preserve_recent", preserve_recent)
                .finish(),
            Self::Custom(c) => f.debug_struct("Custom").field("name", &c.name()).finish(),
        }
    }
}

/// Configuration for the context compaction pipeline.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Ordered list of compaction stages to execute.
    pub stages: Vec<CompactionStage>,
    /// Optional model name used for AutoSummarize. If None, AutoSummarize is skipped.
    pub summary_model: Option<String>,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            stages: Vec::new(),
            summary_model: None,
        }
    }
}

/// The context compactor applies multi-stage compaction to message histories.
///
/// Stages execute sequentially in the order defined in the config.
/// The compactor returns on the first stage that triggers (modifies messages),
/// or returns `None` if no stage's threshold is met.
#[derive(Debug, Clone)]
pub struct ContextCompactor {
    config: CompactionConfig,
}

impl ContextCompactor {
    /// Create a new `ContextCompactor` with the given configuration.
    pub fn new(config: CompactionConfig) -> Self {
        Self { config }
    }

    /// Estimate token count using a simple heuristic: total characters / 4.
    fn estimate_tokens(messages: &[Message]) -> usize {
        messages
            .iter()
            .map(|msg| match msg {
                Message::System { content } => content.len(),
                Message::User { content } => content
                    .iter()
                    .map(|block| match block {
                        crate::message::ContentBlock::Text { text } => text.len(),
                        crate::message::ContentBlock::Image { data, .. } => data.len(),
                        crate::message::ContentBlock::ToolUse { block } => {
                            block.name.len() + block.input.to_string().len()
                        }
                    })
                    .sum::<usize>(),
                Message::Assistant { content, .. } => content
                    .iter()
                    .map(|block| match block {
                        crate::message::ContentBlock::Text { text } => text.len(),
                        crate::message::ContentBlock::Image { data, .. } => data.len(),
                        crate::message::ContentBlock::ToolUse { block } => {
                            block.name.len() + block.input.to_string().len()
                        }
                    })
                    .sum::<usize>(),
                Message::ToolResult { content, .. } => content.len(),
            })
            .sum::<usize>()
            / 4
    }

    /// Apply the compaction pipeline to the given message history.
    ///
    /// Executes stages sequentially. Returns `Some(CompactionEvent)` from the first
    /// stage that triggers, or `None` if no stage's threshold is met.
    pub fn compact(&self, messages: &mut Vec<Message>) -> Option<CompactionEvent> {
        for stage in &self.config.stages {
            let result = match stage {
                CompactionStage::Snip { max_history_tokens } => {
                    self.apply_snip(messages, *max_history_tokens)
                }
                CompactionStage::TruncateToolResults { max_chars } => {
                    self.apply_truncate_tool_results(messages, *max_chars)
                }
                CompactionStage::AutoSummarize {
                    threshold_tokens,
                    preserve_recent,
                } => self.apply_auto_summarize(messages, *threshold_tokens, *preserve_recent),
                CompactionStage::Custom(custom) => custom.apply(messages),
            };

            if result.is_some() {
                return result;
            }
        }

        None
    }

    /// Snip stage: remove oldest non-system messages until token count is within limit.
    /// Preserves all system messages and the most recent user message.
    fn apply_snip(
        &self,
        messages: &mut Vec<Message>,
        max_history_tokens: usize,
    ) -> Option<CompactionEvent> {
        let tokens_before = Self::estimate_tokens(messages);

        if tokens_before <= max_history_tokens {
            return None;
        }

        // Note: last user message index is recalculated dynamically in the loop
        // since the vec is being modified.

        let mut removed_count = 0;

        // Remove oldest non-system messages until we're under the threshold.
        // We iterate from the front (oldest), skipping system messages and the last user message.
        loop {
            let current_tokens = Self::estimate_tokens(messages);
            if current_tokens <= max_history_tokens {
                break;
            }

            // Find the first removable message (non-system, not the last user message)
            let removable_idx = messages.iter().enumerate().position(|(idx, msg)| {
                // Don't remove system messages
                if matches!(msg, Message::System { .. }) {
                    return false;
                }
                // Don't remove the most recent user message
                // (recalculate last_user_idx since we're modifying the vec)
                let current_last_user = messages
                    .iter()
                    .rposition(|m| matches!(m, Message::User { .. }));
                if let Some(last_u) = current_last_user {
                    if idx == last_u {
                        return false;
                    }
                }
                true
            });

            match removable_idx {
                Some(idx) => {
                    messages.remove(idx);
                    removed_count += 1;
                }
                None => break, // No more removable messages
            }
        }

        if removed_count == 0 {
            return None;
        }

        let tokens_after = Self::estimate_tokens(messages);

        Some(CompactionEvent {
            stage: "snip".to_string(),
            messages_affected: removed_count,
            tokens_before,
            tokens_after,
        })
    }

    /// TruncateToolResults stage: truncate tool results exceeding max_chars,
    /// appending "[truncated]".
    fn apply_truncate_tool_results(
        &self,
        messages: &mut Vec<Message>,
        max_chars: usize,
    ) -> Option<CompactionEvent> {
        let tokens_before = Self::estimate_tokens(messages);
        let mut truncated_count = 0;

        for msg in messages.iter_mut() {
            if let Message::ToolResult { content, .. } = msg {
                if content.len() > max_chars {
                    // Truncate and append suffix
                    let suffix = "[truncated]";
                    let truncate_at = max_chars.saturating_sub(suffix.len());
                    let mut new_content = content[..truncate_at].to_string();
                    new_content.push_str(suffix);
                    *content = new_content;
                    truncated_count += 1;
                }
            }
        }

        if truncated_count == 0 {
            return None;
        }

        let tokens_after = Self::estimate_tokens(messages);

        Some(CompactionEvent {
            stage: "truncate_tool_results".to_string(),
            messages_affected: truncated_count,
            tokens_before,
            tokens_after,
        })
    }

    /// AutoSummarize stage: replace old messages with a summary placeholder.
    /// Skips if no summary_model is configured.
    fn apply_auto_summarize(
        &self,
        messages: &mut Vec<Message>,
        threshold_tokens: usize,
        preserve_recent: usize,
    ) -> Option<CompactionEvent> {
        // Skip if no summary model configured
        if self.config.summary_model.is_none() {
            return None;
        }

        let tokens_before = Self::estimate_tokens(messages);

        if tokens_before <= threshold_tokens {
            return None;
        }

        // Separate system messages from non-system messages

        // We need to preserve:
        // 1. All system messages (anywhere in the history)
        // 2. The most recent `preserve_recent` non-system messages

        // Count non-system messages
        let non_system_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| !matches!(msg, Message::System { .. }))
            .map(|(idx, _)| idx)
            .collect();

        let non_system_count = non_system_indices.len();

        // If we don't have enough non-system messages to summarize, skip
        if non_system_count <= preserve_recent {
            return None;
        }

        // The messages to summarize are the oldest non-system messages
        // (everything except the last `preserve_recent` non-system messages)
        let summarize_count = non_system_count - preserve_recent;
        let indices_to_remove: Vec<usize> = non_system_indices[..summarize_count].to_vec();

        if indices_to_remove.is_empty() {
            return None;
        }

        // Create summary placeholder
        let summary_text = format!("[summary of {} messages]", indices_to_remove.len());

        // Remove messages to summarize (iterate from the back to preserve indices)
        for &idx in indices_to_remove.iter().rev() {
            messages.remove(idx);
        }

        // Insert summary message after the last system message (or at the start if none)
        let insert_pos = messages
            .iter()
            .rposition(|msg| matches!(msg, Message::System { .. }))
            .map(|idx| idx + 1)
            .unwrap_or(0);

        messages.insert(
            insert_pos,
            Message::System {
                content: summary_text,
            },
        );

        let tokens_after = Self::estimate_tokens(messages);

        Some(CompactionEvent {
            stage: "auto_summarize".to_string(),
            messages_affected: indices_to_remove.len(),
            tokens_before,
            tokens_after,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, Message};
    use proptest::prelude::*;

    /// Helper: create a system message with given content.
    fn system_msg(content: &str) -> Message {
        Message::System {
            content: content.to_string(),
        }
    }

    /// Helper: create a user message with text content.
    fn user_msg(text: &str) -> Message {
        Message::User {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    /// Helper: create an assistant message with text content.
    fn assistant_msg(text: &str) -> Message {
        Message::Assistant {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            usage: None,
        }
    }

    /// Helper: create a tool result message.
    fn tool_result_msg(id: &str, content: &str) -> Message {
        Message::ToolResult {
            tool_use_id: id.to_string(),
            content: content.to_string(),
            is_error: false,
        }
    }

    #[test]
    fn no_stages_returns_none() {
        let compactor = ContextCompactor::new(CompactionConfig::default());
        let mut messages = vec![user_msg("hello")];
        assert_eq!(compactor.compact(&mut messages), None);
    }

    #[test]
    fn snip_below_threshold_returns_none() {
        let config = CompactionConfig {
            stages: vec![CompactionStage::Snip {
                max_history_tokens: 1000,
            }],
            summary_model: None,
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![system_msg("be helpful"), user_msg("hi")];
        let original = messages.clone();
        assert_eq!(compactor.compact(&mut messages), None);
        assert_eq!(messages, original);
    }

    #[test]
    fn snip_removes_oldest_non_system_messages() {
        // Create messages that exceed the token limit
        // Each character ~0.25 tokens, so 100 chars = ~25 tokens
        let long_text = "a".repeat(400); // ~100 tokens
        let config = CompactionConfig {
            stages: vec![CompactionStage::Snip {
                max_history_tokens: 30, // very low threshold
            }],
            summary_model: None,
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![
            system_msg("system"),
            user_msg(&long_text),
            assistant_msg(&long_text),
            user_msg("latest question"),
        ];

        let event = compactor.compact(&mut messages);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.stage, "snip");
        assert!(event.messages_affected > 0);
        assert!(event.tokens_after <= event.tokens_before);

        // System message should be preserved
        assert!(messages
            .iter()
            .any(|m| matches!(m, Message::System { content } if content == "system")));

        // Most recent user message should be preserved
        assert!(messages
            .iter()
            .any(|m| matches!(m, Message::User { content } if content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text == "latest question")))));
    }

    #[test]
    fn snip_preserves_system_messages() {
        let long_text = "x".repeat(800);
        let config = CompactionConfig {
            stages: vec![CompactionStage::Snip {
                max_history_tokens: 10,
            }],
            summary_model: None,
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![
            system_msg("sys1"),
            system_msg("sys2"),
            user_msg(&long_text),
            assistant_msg(&long_text),
            user_msg("last user"),
        ];

        compactor.compact(&mut messages);

        // Both system messages must remain
        let system_count = messages
            .iter()
            .filter(|m| matches!(m, Message::System { .. }))
            .count();
        assert_eq!(system_count, 2);
    }

    #[test]
    fn truncate_tool_results_below_threshold() {
        let config = CompactionConfig {
            stages: vec![CompactionStage::TruncateToolResults { max_chars: 100 }],
            summary_model: None,
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![tool_result_msg("t1", "short result")];
        let original = messages.clone();
        assert_eq!(compactor.compact(&mut messages), None);
        assert_eq!(messages, original);
    }

    #[test]
    fn truncate_tool_results_truncates_long_content() {
        let config = CompactionConfig {
            stages: vec![CompactionStage::TruncateToolResults { max_chars: 50 }],
            summary_model: None,
        };
        let compactor = ContextCompactor::new(config);
        let long_content = "x".repeat(200);
        let mut messages = vec![tool_result_msg("t1", &long_content)];

        let event = compactor.compact(&mut messages);
        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.stage, "truncate_tool_results");
        assert_eq!(event.messages_affected, 1);

        // Verify the content was truncated
        if let Message::ToolResult { content, .. } = &messages[0] {
            assert!(content.len() <= 50);
            assert!(content.ends_with("[truncated]"));
        } else {
            panic!("expected ToolResult message");
        }
    }

    #[test]
    fn truncate_tool_results_multiple() {
        let config = CompactionConfig {
            stages: vec![CompactionStage::TruncateToolResults { max_chars: 20 }],
            summary_model: None,
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![
            tool_result_msg("t1", &"a".repeat(100)),
            user_msg("question"),
            tool_result_msg("t2", &"b".repeat(50)),
            tool_result_msg("t3", "short"),
        ];

        let event = compactor.compact(&mut messages).unwrap();
        assert_eq!(event.stage, "truncate_tool_results");
        assert_eq!(event.messages_affected, 2); // t1 and t2 exceeded max_chars

        // t3 should be unchanged
        if let Message::ToolResult { content, .. } = &messages[3] {
            assert_eq!(content, "short");
        }
    }

    #[test]
    fn auto_summarize_skipped_without_model() {
        let config = CompactionConfig {
            stages: vec![CompactionStage::AutoSummarize {
                threshold_tokens: 1, // very low threshold, should trigger
                preserve_recent: 1,
            }],
            summary_model: None, // no model configured
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![
            user_msg(&"x".repeat(100)),
            assistant_msg(&"y".repeat(100)),
            user_msg("latest"),
        ];
        let original = messages.clone();
        assert_eq!(compactor.compact(&mut messages), None);
        assert_eq!(messages, original);
    }

    #[test]
    fn auto_summarize_replaces_old_messages() {
        let long = "z".repeat(400);
        let config = CompactionConfig {
            stages: vec![CompactionStage::AutoSummarize {
                threshold_tokens: 10, // low threshold
                preserve_recent: 2,
            }],
            summary_model: Some("gpt-4".to_string()),
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![
            system_msg("instructions"),
            user_msg(&long),
            assistant_msg(&long),
            user_msg("recent1"),
            assistant_msg("recent2"),
        ];

        let event = compactor.compact(&mut messages).unwrap();
        assert_eq!(event.stage, "auto_summarize");
        assert!(event.messages_affected > 0);

        // System message preserved
        assert!(messages
            .iter()
            .any(|m| matches!(m, Message::System { content } if content == "instructions")));

        // Recent messages preserved
        assert!(messages.iter().any(
            |m| matches!(m, Message::User { content } if content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text == "recent1")))
        ));
        assert!(messages.iter().any(
            |m| matches!(m, Message::Assistant { content, .. } if content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text == "recent2")))
        ));

        // Summary placeholder present
        assert!(messages.iter().any(
            |m| matches!(m, Message::System { content } if content.starts_with("[summary of"))
        ));
    }

    #[test]
    fn auto_summarize_below_threshold_returns_none() {
        let config = CompactionConfig {
            stages: vec![CompactionStage::AutoSummarize {
                threshold_tokens: 10000,
                preserve_recent: 2,
            }],
            summary_model: Some("gpt-4".to_string()),
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![user_msg("hi"), assistant_msg("hello")];
        let original = messages.clone();
        assert_eq!(compactor.compact(&mut messages), None);
        assert_eq!(messages, original);
    }

    #[test]
    fn stages_execute_sequentially_first_wins() {
        // Both stages would trigger, but Snip runs first
        let long = "m".repeat(400);
        let config = CompactionConfig {
            stages: vec![
                CompactionStage::Snip {
                    max_history_tokens: 10,
                },
                CompactionStage::TruncateToolResults { max_chars: 5 },
            ],
            summary_model: None,
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![
            system_msg("sys"),
            user_msg(&long),
            tool_result_msg("t1", &long),
            user_msg("latest"),
        ];

        let event = compactor.compact(&mut messages).unwrap();
        // Snip should fire first
        assert_eq!(event.stage, "snip");
    }

    #[test]
    fn custom_stage_is_invoked() {
        struct DoubleSystem;
        impl CompactionFn for DoubleSystem {
            fn apply(&self, messages: &mut Vec<Message>) -> Option<CompactionEvent> {
                // Add a system message for testing purposes
                messages.push(Message::System {
                    content: "custom was here".to_string(),
                });
                Some(CompactionEvent {
                    stage: "double_system".to_string(),
                    messages_affected: 1,
                    tokens_before: 10,
                    tokens_after: 12,
                })
            }
            fn name(&self) -> &str {
                "double_system"
            }
        }

        let config = CompactionConfig {
            stages: vec![CompactionStage::Custom(Arc::new(DoubleSystem))],
            summary_model: None,
        };
        let compactor = ContextCompactor::new(config);
        let mut messages = vec![user_msg("hello")];
        let event = compactor.compact(&mut messages).unwrap();
        assert_eq!(event.stage, "double_system");
        assert!(messages
            .iter()
            .any(|m| matches!(m, Message::System { content } if content == "custom was here")));
    }

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(ContextCompactor::estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_tokens_basic() {
        let messages = vec![user_msg(&"a".repeat(100))]; // 100 chars / 4 = 25 tokens
        assert_eq!(ContextCompactor::estimate_tokens(&messages), 25);
    }

    #[test]
    fn compaction_event_fields() {
        let event = CompactionEvent {
            stage: "snip".to_string(),
            messages_affected: 3,
            tokens_before: 500,
            tokens_after: 100,
        };
        assert_eq!(event.stage, "snip");
        assert_eq!(event.messages_affected, 3);
        assert_eq!(event.tokens_before, 500);
        assert_eq!(event.tokens_after, 100);
    }

    // --- Property-based tests ---
    //
    // Property 6: Snip compaction preserves critical messages
    // **Validates: Requirements 11.4**

    /// Strategy to generate a non-empty text string of a given length range (in chars).
    fn arb_text(min_len: usize, max_len: usize) -> impl Strategy<Value = String> {
        prop::collection::vec(prop::char::range('a', 'z'), min_len..=max_len)
            .prop_map(|chars| chars.into_iter().collect::<String>())
    }

    /// Strategy to generate a system message with variable content.
    fn arb_system_msg() -> impl Strategy<Value = Message> {
        arb_text(4, 40).prop_map(|content| Message::System { content })
    }

    /// Strategy to generate a user message with text content of a given size range.
    fn arb_user_msg(min_chars: usize, max_chars: usize) -> impl Strategy<Value = Message> {
        arb_text(min_chars, max_chars).prop_map(|text| Message::User {
            content: vec![ContentBlock::Text { text }],
        })
    }

    /// Strategy to generate an assistant message.
    fn arb_assistant_msg(min_chars: usize, max_chars: usize) -> impl Strategy<Value = Message> {
        arb_text(min_chars, max_chars).prop_map(|text| Message::Assistant {
            content: vec![ContentBlock::Text { text }],
            usage: None,
        })
    }

    /// Strategy to generate a tool result message.
    fn arb_tool_result_msg(
        min_chars: usize,
        max_chars: usize,
    ) -> impl Strategy<Value = Message> {
        (arb_text(4, 20), arb_text(min_chars, max_chars)).prop_map(|(id, content)| {
            Message::ToolResult {
                tool_use_id: id,
                content,
                is_error: false,
            }
        })
    }

    /// Strategy to generate a non-system message (user, assistant, or tool result).
    fn arb_non_system_msg(min_chars: usize, max_chars: usize) -> impl Strategy<Value = Message> {
        prop_oneof![
            arb_user_msg(min_chars, max_chars),
            arb_assistant_msg(min_chars, max_chars),
            arb_tool_result_msg(min_chars, max_chars),
        ]
    }

    /// Strategy to generate a message history that:
    /// - Contains 1–4 system messages at various positions
    /// - Contains multiple user messages (at least 2)
    /// - Always exceeds a low token threshold (lots of content)
    /// - The last user message is identifiable
    fn arb_message_history_exceeding_threshold() -> impl Strategy<Value = (Vec<Message>, String)> {
        // Generate system messages (1-4)
        let system_msgs = prop::collection::vec(arb_system_msg(), 1..=4);
        // Generate non-system "filler" messages (5-15) with substantial content
        let filler_msgs = prop::collection::vec(arb_non_system_msg(100, 400), 5..=15);
        // Generate additional user messages to ensure at least 2 exist (2-4)
        let extra_user_msgs = prop::collection::vec(arb_user_msg(100, 300), 2..=4);
        // Generate the final user message with a unique marker
        let final_user_text = arb_text(20, 80);

        (system_msgs, filler_msgs, extra_user_msgs, final_user_text).prop_map(
            |(sys_msgs, filler, extra_users, final_text)| {
                let mut messages: Vec<Message> = Vec::new();

                // Interleave system messages among the beginning
                for (i, sys) in sys_msgs.iter().enumerate() {
                    // Place system messages at various positions
                    let pos = i.min(messages.len());
                    messages.insert(pos, sys.clone());
                }

                // Add extra user messages
                for user_msg in &extra_users {
                    messages.push(user_msg.clone());
                }

                // Add filler messages
                for msg in &filler {
                    messages.push(msg.clone());
                }

                // Append the final user message at the end (must be the LAST user message)
                let final_msg = Message::User {
                    content: vec![ContentBlock::Text {
                        text: final_text.clone(),
                    }],
                };
                messages.push(final_msg);

                (messages, final_text)
            },
        )
    }

    proptest! {
        /// Property 6: Snip compaction preserves critical messages.
        ///
        /// When message history exceeds max_history_tokens:
        /// - ALL system messages are preserved after compaction
        /// - The most recent user message (last in position) is preserved
        /// - Token count after compaction is <= max_history_tokens
        ///   (or as close as possible given preserved messages)
        #[test]
        fn prop_snip_preserves_critical_messages(
            (messages, final_user_text) in arb_message_history_exceeding_threshold()
        ) {
            // Use a low threshold so compaction always triggers.
            // The generated messages have at minimum (1 sys + 2 users + 5 filler) messages
            // with 100+ chars each, so tokens will far exceed 10.
            let max_history_tokens = 10;

            let config = CompactionConfig {
                stages: vec![CompactionStage::Snip { max_history_tokens }],
                summary_model: None,
            };
            let compactor = ContextCompactor::new(config);

            // Count system messages before compaction
            let system_contents_before: Vec<String> = messages
                .iter()
                .filter_map(|m| match m {
                    Message::System { content } => Some(content.clone()),
                    _ => None,
                })
                .collect();

            let tokens_before = ContextCompactor::estimate_tokens(&messages);
            // Confirm the generated history actually exceeds the threshold
            prop_assume!(tokens_before > max_history_tokens);

            let mut compacted = messages.clone();
            let event = compactor.compact(&mut compacted);

            // Compaction should have triggered
            prop_assert!(event.is_some(), "Expected compaction to trigger for {} tokens", tokens_before);
            let event = event.unwrap();
            prop_assert_eq!(&event.stage, "snip");

            // ASSERT 1: ALL system messages are preserved
            let system_contents_after: Vec<String> = compacted
                .iter()
                .filter_map(|m| match m {
                    Message::System { content } => Some(content.clone()),
                    _ => None,
                })
                .collect();

            for sys_content in &system_contents_before {
                prop_assert!(
                    system_contents_after.contains(sys_content),
                    "System message '{}' was removed during snip compaction",
                    sys_content
                );
            }
            prop_assert_eq!(
                system_contents_before.len(),
                system_contents_after.len(),
                "System message count changed: before={}, after={}",
                system_contents_before.len(),
                system_contents_after.len()
            );

            // ASSERT 2: The most recent user message (the one we appended last) is preserved
            let has_final_user = compacted.iter().any(|m| matches!(
                m,
                Message::User { content } if content.iter().any(|b| matches!(
                    b,
                    ContentBlock::Text { text } if text == &final_user_text
                ))
            ));
            prop_assert!(
                has_final_user,
                "The most recent user message with text '{}' was removed during snip compaction",
                &final_user_text
            );

            // ASSERT 3: Token count after compaction is <= max_history_tokens
            // OR only preserved messages remain (system + last user)
            let tokens_after = ContextCompactor::estimate_tokens(&compacted);

            // If we're still over the threshold, it must be because only
            // preserved messages (system + last user) remain and they alone exceed the limit.
            if tokens_after > max_history_tokens {
                // All remaining non-system messages should be only the final user message
                let non_system_msgs: Vec<&Message> = compacted
                    .iter()
                    .filter(|m| !matches!(m, Message::System { .. }))
                    .collect();
                prop_assert_eq!(
                    non_system_msgs.len(),
                    1,
                    "Token count still exceeds threshold ({} > {}) but {} non-system messages remain",
                    tokens_after,
                    max_history_tokens,
                    non_system_msgs.len()
                );
            }
        }

        /// Property 8: AutoSummarize preserves system and recent messages.
        /// **Validates: Requirements 11.6**
        ///
        /// When message history exceeds threshold_tokens with a summary_model configured:
        /// - ALL system messages are preserved after compaction
        /// - The most recent N non-system messages (where N = preserve_recent) are preserved
        /// - A summary placeholder message is inserted
        #[test]
        fn prop_auto_summarize_preserves_system_and_recent(
            // Generate 1-3 system messages
            system_msgs in prop::collection::vec(arb_system_msg(), 1..=3),
            // Generate many non-system messages with substantial content to exceed threshold
            non_system_msgs in prop::collection::vec(arb_non_system_msg(200, 500), 8..=15),
            // preserve_recent between 1 and 4
            preserve_recent in 1usize..=4,
        ) {
            // Build message history: system messages first, then non-system messages
            let mut messages: Vec<Message> = Vec::new();
            for sys in &system_msgs {
                messages.push(sys.clone());
            }
            for msg in &non_system_msgs {
                messages.push(msg.clone());
            }

            // Ensure we have more non-system messages than preserve_recent
            prop_assume!(non_system_msgs.len() > preserve_recent);

            // Use a very low threshold so compaction always triggers
            let threshold_tokens = 10;
            let tokens_before = ContextCompactor::estimate_tokens(&messages);
            prop_assume!(tokens_before > threshold_tokens);

            let config = CompactionConfig {
                stages: vec![CompactionStage::AutoSummarize {
                    threshold_tokens,
                    preserve_recent,
                }],
                summary_model: Some("test-summary-model".to_string()),
            };
            let compactor = ContextCompactor::new(config);

            // Track system message contents before compaction
            let system_contents_before: Vec<String> = messages
                .iter()
                .filter_map(|m| match m {
                    Message::System { content } => Some(content.clone()),
                    _ => None,
                })
                .collect();

            // Track the last `preserve_recent` non-system messages before compaction
            let recent_non_system: Vec<Message> = non_system_msgs
                .iter()
                .rev()
                .take(preserve_recent)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();

            let mut compacted = messages.clone();
            let event = compactor.compact(&mut compacted);

            // Compaction should have triggered
            prop_assert!(event.is_some(), "Expected auto_summarize to trigger for {} tokens", tokens_before);
            let event = event.unwrap();
            prop_assert_eq!(&event.stage, "auto_summarize");

            // ASSERT 1: ALL system messages are preserved
            let system_contents_after: Vec<String> = compacted
                .iter()
                .filter_map(|m| match m {
                    Message::System { content } => Some(content.clone()),
                    _ => None,
                })
                .collect();

            // Original system messages must still be present (the summary placeholder
            // is also a System message, so we check the originals are a subset)
            for sys_content in &system_contents_before {
                prop_assert!(
                    system_contents_after.contains(sys_content),
                    "System message '{}' was removed during auto_summarize compaction",
                    sys_content
                );
            }

            // ASSERT 2: The most recent N non-system messages are preserved
            for recent_msg in &recent_non_system {
                prop_assert!(
                    compacted.contains(recent_msg),
                    "A recent non-system message was removed during auto_summarize compaction"
                );
            }

            // ASSERT 3: A summary placeholder message was inserted
            let has_summary_placeholder = compacted.iter().any(|m| matches!(
                m,
                Message::System { content } if content.starts_with("[summary of")
            ));
            prop_assert!(
                has_summary_placeholder,
                "No summary placeholder message found after auto_summarize compaction"
            );
        }

        /// Property 7: Tool result truncation.
        ///
        /// **Validates: Requirements 11.5**
        ///
        /// For any tool result string exceeding max_chars, after the TruncateToolResults
        /// stage the result shall have length ≤ max_chars and end with "[truncated]".
        /// Results that were already under max_chars are unchanged.
        #[test]
        fn prop_tool_result_truncation(
            max_chars in 20usize..=200,
            // Generate tool results that EXCEED max_chars (use 201-500 char range)
            long_contents in prop::collection::vec(arb_text(201, 500), 1..=5),
            // Generate tool results that are short (under min max_chars of 20)
            short_contents in prop::collection::vec(arb_text(1, 19), 1..=3),
            // Generate unique tool IDs
            long_ids in prop::collection::vec(arb_text(4, 12), 5..=5),
            short_ids in prop::collection::vec(arb_text(4, 12), 3..=3),
        ) {
            let config = CompactionConfig {
                stages: vec![CompactionStage::TruncateToolResults { max_chars }],
                summary_model: None,
            };
            let compactor = ContextCompactor::new(config);

            // Build messages: mix of long tool results (exceeding max_chars) and short ones
            let mut messages: Vec<Message> = Vec::new();

            // Add long tool results that should be truncated
            for (i, content) in long_contents.iter().enumerate() {
                let id = long_ids.get(i).cloned().unwrap_or_else(|| format!("long_{}", i));
                // Ensure content actually exceeds max_chars
                let actual_content = if content.len() <= max_chars {
                    // Pad to exceed max_chars
                    let padding = "x".repeat(max_chars - content.len() + 1);
                    format!("{}{}", content, padding)
                } else {
                    content.clone()
                };
                messages.push(Message::ToolResult {
                    tool_use_id: id,
                    content: actual_content,
                    is_error: false,
                });
            }

            // Track where short messages start
            let short_messages_start_idx = messages.len();

            // Add short tool results that should remain unchanged
            for (i, content) in short_contents.iter().enumerate() {
                let id = short_ids.get(i).cloned().unwrap_or_else(|| format!("short_{}", i));
                // Ensure content is actually under max_chars
                let actual_content = if content.len() >= max_chars {
                    content[..max_chars - 1].to_string()
                } else {
                    content.clone()
                };
                messages.push(Message::ToolResult {
                    tool_use_id: id,
                    content: actual_content,
                    is_error: false,
                });
            }

            // Save original short contents for later comparison
            let original_short_contents: Vec<String> = messages[short_messages_start_idx..]
                .iter()
                .filter_map(|m| match m {
                    Message::ToolResult { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .collect();

            // Run compaction
            let event = compactor.compact(&mut messages);

            // Since we have long tool results exceeding max_chars, compaction should trigger
            prop_assert!(event.is_some(), "Expected truncation to trigger");
            let event = event.unwrap();
            prop_assert_eq!(&event.stage, "truncate_tool_results");

            // ASSERT 1: Every tool result content is now ≤ max_chars
            for msg in &messages {
                if let Message::ToolResult { content, .. } = msg {
                    prop_assert!(
                        content.len() <= max_chars,
                        "Tool result content length {} exceeds max_chars {}",
                        content.len(),
                        max_chars
                    );
                }
            }

            // ASSERT 2: Truncated results (those that were long) end with "[truncated]"
            for msg in &messages[..short_messages_start_idx] {
                if let Message::ToolResult { content, .. } = msg {
                    prop_assert!(
                        content.ends_with("[truncated]"),
                        "Truncated tool result should end with '[truncated]', got trailing: '{}'",
                        &content[content.len().saturating_sub(30)..]
                    );
                }
            }

            // ASSERT 3: Results that were already under max_chars are unchanged
            let current_short_contents: Vec<String> = messages[short_messages_start_idx..]
                .iter()
                .filter_map(|m| match m {
                    Message::ToolResult { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .collect();

            prop_assert_eq!(
                &original_short_contents,
                &current_short_contents,
                "Short tool results should be unchanged after truncation"
            );
        }

        /// Property 9: Compaction no-op below thresholds.
        ///
        /// **Validates: Requirements 11.9**
        ///
        /// For any message history where no compaction stage's activation threshold is met,
        /// the compactor shall return `None` and the message history shall remain unchanged.
        #[test]
        fn prop_compaction_noop_below_thresholds(
            messages in prop::collection::vec(
                arb_non_system_msg(1, 20),
                1..=5usize,
            )
        ) {
            // All messages have short content (1-20 chars each).
            // With 5 messages of max 20 chars each, total chars <= 100, tokens <= 25.
            // Configure all thresholds far above what these messages can reach.
            let config = CompactionConfig {
                stages: vec![
                    CompactionStage::Snip { max_history_tokens: 10_000 },
                    CompactionStage::TruncateToolResults { max_chars: 10_000 },
                    CompactionStage::AutoSummarize {
                        threshold_tokens: 10_000,
                        preserve_recent: 2,
                    },
                ],
                summary_model: Some("gpt-4".to_string()),
            };
            let compactor = ContextCompactor::new(config);

            // Clone messages before compaction to compare afterwards
            let original = messages.clone();
            let mut compacted = messages;

            // Run compaction
            let event = compactor.compact(&mut compacted);

            // Assert: compact() returns None (no stage triggered)
            prop_assert!(
                event.is_none(),
                "Expected compaction to return None for messages below all thresholds, got {:?}",
                event
            );

            // Assert: messages vector is identical to the original (unchanged)
            prop_assert_eq!(
                &compacted,
                &original,
                "Messages were modified despite no compaction stage triggering"
            );
        }
    }
}
