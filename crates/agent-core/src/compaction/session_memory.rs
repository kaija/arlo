//! Session Memory layer implementation.
//!
//! Uses a session memory file as the summary source, removing old messages
//! and injecting the file content as a system message.

use crate::message::{ContentBlock, Message};

use super::layer::{CompactionContext, CompactionLayer, LayerResult};
use super::tokens::{estimate_single_message_tokens, estimate_tokens};
use super::CompactionEvent;

/// Layer 2: Inject session memory file as summary, removing old messages.
pub struct SessionMemoryLayer;

impl SessionMemoryLayer {
    /// Count messages that contain at least one text content block.
    /// Applies to User and Assistant messages with a Text variant in their content.
    fn count_text_block_messages(messages: &[Message]) -> usize {
        messages
            .iter()
            .filter(|msg| match msg {
                Message::User { content } => {
                    content.iter().any(|b| matches!(b, ContentBlock::Text { .. }))
                }
                Message::Assistant { content, .. } => {
                    content.iter().any(|b| matches!(b, ContentBlock::Text { .. }))
                }
                _ => false,
            })
            .count()
    }

    /// Compute the preserve boundary by working backwards from the newest messages,
    /// accumulating tokens until hitting the max preserved token limit.
    /// Returns the index where preservation starts (all messages from this index onward are kept).
    fn compute_preserve_boundary(messages: &[Message], max_tokens: usize) -> usize {
        let mut accumulated_tokens = 0;
        let mut preserve_from = messages.len();

        for i in (0..messages.len()).rev() {
            let msg_tokens = estimate_single_message_tokens(&messages[i]);
            if accumulated_tokens + msg_tokens > max_tokens {
                break;
            }
            accumulated_tokens += msg_tokens;
            preserve_from = i;
        }

        preserve_from
    }

    /// Find the insertion point: immediately after the last System message.
    /// If no System message exists, returns 0.
    fn find_insert_position(messages: &[Message]) -> usize {
        messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| matches!(msg, Message::System { .. }))
            .map(|(idx, _)| idx + 1)
            .last()
            .unwrap_or(0)
    }
}

impl CompactionLayer for SessionMemoryLayer {
    fn apply(&self, messages: &mut Vec<Message>, context: &CompactionContext) -> LayerResult {
        let config = &context.config;

        // Guard 1: session_memory_path must exist and point to a readable file.
        let memory_path = match &config.session_memory_path {
            Some(p) if p.exists() => p,
            _ => return LayerResult::Noop,
        };

        // Guard 2: minimum token count (default 10k).
        let tokens_before = estimate_tokens(messages);
        if tokens_before < config.session_memory_min_tokens {
            return LayerResult::Noop;
        }

        // Guard 3: minimum text-block messages (default 5).
        let text_block_count = Self::count_text_block_messages(messages);
        if text_block_count < config.session_memory_min_messages {
            return LayerResult::Noop;
        }

        // Read session memory content from disk.
        let memory_content = match std::fs::read_to_string(memory_path) {
            Ok(content) => content,
            Err(_) => return LayerResult::Failed("failed to read session memory file".into()),
        };

        // Compute preserve boundary: work backwards from newest messages,
        // accumulating tokens up to session_memory_max_preserved_tokens (40k default).
        let preserve_boundary =
            Self::compute_preserve_boundary(messages, config.session_memory_max_preserved_tokens);

        // Identify non-system messages before the preserve boundary to remove.
        let indices_to_remove: Vec<usize> = messages[..preserve_boundary]
            .iter()
            .enumerate()
            .filter(|(_, msg)| !matches!(msg, Message::System { .. }))
            .map(|(idx, _)| idx)
            .collect();

        if indices_to_remove.is_empty() {
            return LayerResult::Noop;
        }

        let messages_affected = indices_to_remove.len();

        // Remove old non-system messages (iterate from back to preserve indices).
        for &idx in indices_to_remove.iter().rev() {
            messages.remove(idx);
        }

        // Find insertion point: after the last existing System message.
        let insert_pos = Self::find_insert_position(messages);

        // Inject session memory as a system message.
        let summary_msg = Message::System {
            content: format!("[Session Memory Summary]\n\n{}", memory_content),
        };
        messages.insert(insert_pos, summary_msg);

        let tokens_after = estimate_tokens(messages);
        LayerResult::Applied(CompactionEvent {
            stage: "session_memory".to_string(),
            messages_affected,
            tokens_before,
            tokens_after,
        })
    }

    fn name(&self) -> &str {
        "session_memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::config::CompactionLayerConfig;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Helper to build a CompactionContext with configurable overrides.
    fn make_context(config: CompactionLayerConfig) -> CompactionContext {
        CompactionContext {
            token_count: 50_000,
            trigger_threshold: 167_000,
            current_turn: 10,
            config,
        }
    }

    /// Create a config with a valid session memory temp file.
    fn config_with_memory_file(content: &str) -> (CompactionLayerConfig, NamedTempFile) {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", content).unwrap();
        let mut config = CompactionLayerConfig::default();
        config.session_memory_path = Some(file.path().to_path_buf());
        (config, file)
    }

    /// Build a large User text message of approximately `token_target` tokens.
    fn large_user_message(token_target: usize) -> Message {
        let text = "x".repeat(token_target * 4);
        Message::User {
            content: vec![ContentBlock::Text { text }],
        }
    }

    /// Build a simple text User message.
    fn text_user_message(text: &str) -> Message {
        Message::User {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    /// Build a simple text Assistant message.
    fn text_assistant_message(text: &str) -> Message {
        Message::Assistant {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            usage: None,
        }
    }

    #[test]
    fn noop_when_no_session_memory_path() {
        let config = CompactionLayerConfig::default(); // session_memory_path is None
        let ctx = make_context(config);
        let layer = SessionMemoryLayer;
        let mut messages = vec![large_user_message(3000)];
        assert_eq!(layer.apply(&mut messages, &ctx), LayerResult::Noop);
    }

    #[test]
    fn noop_when_tokens_below_minimum() {
        let (mut config, _file) = config_with_memory_file("session summary");
        config.session_memory_min_tokens = 10_000;
        let ctx = make_context(config);
        let layer = SessionMemoryLayer;
        // Small message: way below 10k tokens
        let mut messages = vec![text_user_message("hello")];
        assert_eq!(layer.apply(&mut messages, &ctx), LayerResult::Noop);
    }

    #[test]
    fn noop_when_fewer_than_min_text_block_messages() {
        let (mut config, _file) = config_with_memory_file("session summary");
        config.session_memory_min_tokens = 1; // low threshold so token guard passes
        config.session_memory_min_messages = 5;
        let ctx = make_context(config);
        let layer = SessionMemoryLayer;
        // Only 2 text-block messages (below the 5 threshold)
        let mut messages = vec![
            large_user_message(1000),
            text_assistant_message("response"),
        ];
        assert_eq!(layer.apply(&mut messages, &ctx), LayerResult::Noop);
    }

    #[test]
    fn applies_when_all_guards_pass() {
        let memory_text = "This is the session memory content.";
        let (mut config, _file) = config_with_memory_file(memory_text);
        config.session_memory_min_tokens = 1;
        config.session_memory_min_messages = 2;
        // Preserve only ~2 tokens worth — forces older messages before the boundary
        config.session_memory_max_preserved_tokens = 2;
        let ctx = make_context(config);
        let layer = SessionMemoryLayer;

        let mut messages = vec![
            Message::System {
                content: "system instruction".to_string(),
            },
            large_user_message(500),  // old — should be removed
            text_assistant_message("old response one with enough text to be significant"),
            text_user_message("recent message"),
            text_assistant_message("recent response"),
        ];

        let result = layer.apply(&mut messages, &ctx);
        match result {
            LayerResult::Applied(event) => {
                assert_eq!(event.stage, "session_memory");
                assert!(event.messages_affected > 0);
                assert!(event.tokens_before > event.tokens_after || event.tokens_after > 0);
            }
            other => panic!("Expected Applied, got {:?}", other),
        }

        // Verify session memory was injected
        let has_session_memory = messages.iter().any(|m| match m {
            Message::System { content } => content.contains("[Session Memory Summary]"),
            _ => false,
        });
        assert!(has_session_memory);
    }

    #[test]
    fn preserves_system_messages() {
        let memory_text = "Summary content";
        let (mut config, _file) = config_with_memory_file(memory_text);
        config.session_memory_min_tokens = 1;
        config.session_memory_min_messages = 2;
        config.session_memory_max_preserved_tokens = 10; // very small preserve window
        let ctx = make_context(config);
        let layer = SessionMemoryLayer;

        let mut messages = vec![
            Message::System {
                content: "system instruction".to_string(),
            },
            text_user_message("old user message one with long enough text"),
            text_assistant_message("old assistant message one with long enough text"),
            text_user_message("another old user message with long enough text"),
            text_assistant_message("another old assistant with long enough text"),
            text_user_message("recent short"),
        ];

        layer.apply(&mut messages, &ctx);

        // The original system message should still be present
        let system_count = messages
            .iter()
            .filter(|m| match m {
                Message::System { content } => content == "system instruction",
                _ => false,
            })
            .count();
        assert_eq!(system_count, 1);
    }

    #[test]
    fn session_memory_inserted_after_last_system_message() {
        let memory_text = "Session memory data";
        let (mut config, _file) = config_with_memory_file(memory_text);
        config.session_memory_min_tokens = 1;
        config.session_memory_min_messages = 2;
        config.session_memory_max_preserved_tokens = 10;
        let ctx = make_context(config);
        let layer = SessionMemoryLayer;

        let mut messages = vec![
            Message::System {
                content: "first system".to_string(),
            },
            Message::System {
                content: "second system".to_string(),
            },
            text_user_message("old user msg with enough length to matter for tokens"),
            text_assistant_message("old assistant msg with enough length to matter"),
            text_user_message("recent msg"),
            text_assistant_message("recent response"),
        ];

        layer.apply(&mut messages, &ctx);

        // Find the session memory message
        let session_memory_idx = messages
            .iter()
            .position(|m| match m {
                Message::System { content } => content.contains("[Session Memory Summary]"),
                _ => false,
            })
            .expect("Session memory should be present");

        // Find the last original system message
        let last_original_system_idx = messages
            .iter()
            .position(|m| match m {
                Message::System { content } => content == "second system",
                _ => false,
            })
            .expect("second system should be present");

        // Session memory should come after the last original system message
        assert!(
            session_memory_idx > last_original_system_idx,
            "Session memory (idx {}) should be after last system message (idx {})",
            session_memory_idx,
            last_original_system_idx
        );
    }

    #[test]
    fn noop_when_session_memory_path_does_not_exist() {
        let mut config = CompactionLayerConfig::default();
        config.session_memory_path = Some("/nonexistent/path/to/memory.md".into());
        let ctx = make_context(config);
        let layer = SessionMemoryLayer;
        let mut messages = vec![large_user_message(5000)];
        assert_eq!(layer.apply(&mut messages, &ctx), LayerResult::Noop);
    }

    #[test]
    fn noop_when_nothing_to_remove_before_boundary() {
        let memory_text = "Some session memory";
        let (mut config, _file) = config_with_memory_file(memory_text);
        config.session_memory_min_tokens = 1;
        config.session_memory_min_messages = 2;
        // Very large preserve window — everything is preserved
        config.session_memory_max_preserved_tokens = 1_000_000;
        let ctx = make_context(config);
        let layer = SessionMemoryLayer;

        let mut messages = vec![
            Message::System {
                content: "system".to_string(),
            },
            text_user_message("msg one"),
            text_assistant_message("msg two"),
        ];

        // Since everything fits in the preserve window, nothing is before the boundary
        // and we get Noop
        assert_eq!(layer.apply(&mut messages, &ctx), LayerResult::Noop);
    }

    #[test]
    fn layer_name_is_session_memory() {
        let layer = SessionMemoryLayer;
        assert_eq!(layer.name(), "session_memory");
    }
}


#[cfg(test)]
mod proptests {
    use super::*;
    use crate::compaction::config::CompactionLayerConfig;
    use crate::compaction::tokens::estimate_tokens;
    use proptest::prelude::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// **Validates: Requirements 3.1, 3.2, 3.3, 3.5, 3.6**

    // --- Strategies ---

    /// Generate a text content block with a given size range (in chars).
    fn arb_text_block(min_chars: usize, max_chars: usize) -> impl Strategy<Value = ContentBlock> {
        proptest::collection::vec(prop::char::range('a', 'z'), min_chars..=max_chars).prop_map(
            |chars| ContentBlock::Text {
                text: chars.into_iter().collect(),
            },
        )
    }

    /// Generate a User message with a text block of given size range.
    fn arb_user_message(min_chars: usize, max_chars: usize) -> impl Strategy<Value = Message> {
        arb_text_block(min_chars, max_chars).prop_map(|block| Message::User {
            content: vec![block],
        })
    }

    /// Generate an Assistant message with a text block of given size range.
    fn arb_assistant_message(
        min_chars: usize,
        max_chars: usize,
    ) -> impl Strategy<Value = Message> {
        arb_text_block(min_chars, max_chars).prop_map(|block| Message::Assistant {
            content: vec![block],
            usage: None,
        })
    }

    /// Generate a conversation of alternating User/Assistant messages (pairs).
    /// Each message has text content of the given size range.
    fn arb_conversation(
        min_pairs: usize,
        max_pairs: usize,
        min_chars: usize,
        max_chars: usize,
    ) -> impl Strategy<Value = Vec<Message>> {
        proptest::collection::vec(
            (
                arb_user_message(min_chars, max_chars),
                arb_assistant_message(min_chars, max_chars),
            ),
            min_pairs..=max_pairs,
        )
        .prop_map(|pairs| {
            pairs
                .into_iter()
                .flat_map(|(u, a)| vec![u, a])
                .collect::<Vec<_>>()
        })
    }

    /// Helper to build a CompactionContext.
    fn make_context(config: CompactionLayerConfig) -> CompactionContext {
        CompactionContext {
            token_count: 50_000,
            trigger_threshold: 167_000,
            current_turn: 10,
            config,
        }
    }

    /// Create a config with a valid session memory temp file, returning the file handle to keep it alive.
    fn config_with_memory(
        content: &str,
        min_tokens: usize,
        min_messages: usize,
        max_preserved_tokens: usize,
    ) -> (CompactionLayerConfig, NamedTempFile) {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", content).unwrap();
        let mut config = CompactionLayerConfig::default();
        config.session_memory_path = Some(file.path().to_path_buf());
        config.session_memory_min_tokens = min_tokens;
        config.session_memory_min_messages = min_messages;
        config.session_memory_max_preserved_tokens = max_preserved_tokens;
        (config, file)
    }

    // --- Property 4: Session Memory preserves recent messages and injects summary ---
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        /// Property 4: For any message history where the Session Memory layer activates,
        /// all messages newer than the preservation boundary SHALL remain unmodified,
        /// and the session memory content SHALL be injected as a system message after
        /// the last system-instruction message.
        #[test]
        fn prop_preserves_recent_messages_and_injects_summary(
            conversation in arb_conversation(5, 15, 500, 2000),
            memory_content in "[a-z ]{10,200}",
        ) {
            // Set up: create messages that will pass all guards.
            // System message + conversation messages (at least 5 text-block messages).
            let mut messages = vec![
                Message::System { content: "system instruction".to_string() },
            ];
            messages.extend(conversation.clone());

            let total_tokens = estimate_tokens(&messages);
            // Set min_tokens low enough that our messages pass the guard.
            // Set max_preserved_tokens to something smaller than total to ensure
            // the layer actually removes some messages.
            let max_preserved = total_tokens / 3; // preserve about 1/3 of tokens

            let (config, _file) = config_with_memory(
                &memory_content,
                1,    // min_tokens = 1 (always passes)
                2,    // min_messages = 2 (always passes with 5+ pairs)
                max_preserved,
            );
            let ctx = make_context(config);
            let layer = SessionMemoryLayer;

            // Compute the preserve boundary before applying.
            let preserve_boundary =
                SessionMemoryLayer::compute_preserve_boundary(&messages, max_preserved);

            // Collect the messages that should be preserved (from preserve_boundary onward).
            let expected_preserved: Vec<Message> = messages[preserve_boundary..].to_vec();

            // Apply the layer.
            let result = layer.apply(&mut messages, &ctx);

            // If the layer applied (there were messages to remove before boundary):
            match result {
                LayerResult::Applied(_) => {
                    // 1. Verify session memory summary was injected.
                    let has_summary = messages.iter().any(|m| match m {
                        Message::System { content } => content.contains("[Session Memory Summary]")
                            && content.contains(&memory_content),
                        _ => false,
                    });
                    prop_assert!(has_summary, "Session memory summary should be injected");

                    // 2. Verify the summary appears after the last original system message.
                    // The summary is a system message too, so find its index.
                    let summary_idx = messages.iter().position(|m| match m {
                        Message::System { content } => content.contains("[Session Memory Summary]"),
                        _ => false,
                    }).unwrap();
                    // The original system message ("system instruction") should precede the summary.
                    let original_sys_idx = messages.iter().position(|m| match m {
                        Message::System { content } => content == "system instruction",
                        _ => false,
                    }).unwrap();
                    prop_assert!(summary_idx > original_sys_idx,
                        "Summary (idx {}) should appear after original system message (idx {})",
                        summary_idx, original_sys_idx);

                    // 3. Verify preserved messages are still present and unmodified.
                    // The preserved messages should appear at the end of the final message list.
                    // (They come after system messages + summary.)
                    for expected in &expected_preserved {
                        let found = messages.iter().any(|m| m == expected);
                        prop_assert!(found,
                            "Preserved message should still be present in output");
                    }
                }
                LayerResult::Noop => {
                    // If Noop, it means there was nothing to remove before the boundary.
                    // This can happen if max_preserved covers all non-system messages.
                    // That's fine — the property still holds vacuously.
                }
                LayerResult::Failed(reason) => {
                    prop_assert!(false, "Layer should not fail in this scenario: {}", reason);
                }
            }
        }
    }

    // --- Property 5: Session Memory guard conditions prevent premature activation ---
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        /// Property 5a: When no session memory file exists, the layer returns Noop.
        #[test]
        fn prop_noop_when_no_session_memory_file(
            conversation in arb_conversation(3, 10, 500, 2000),
        ) {
            let mut messages = vec![
                Message::System { content: "system".to_string() },
            ];
            messages.extend(conversation);

            // Config with no session_memory_path.
            let config = CompactionLayerConfig::default();
            let ctx = make_context(config);
            let layer = SessionMemoryLayer;

            let result = layer.apply(&mut messages, &ctx);
            prop_assert_eq!(result, LayerResult::Noop);
        }

        /// Property 5b: When total token count is below session_memory_min_tokens,
        /// the layer returns Noop.
        #[test]
        fn prop_noop_when_tokens_below_minimum(
            // Small messages that won't exceed 10k tokens.
            conversation in arb_conversation(3, 8, 10, 50),
            memory_content in "[a-z]{10,50}",
        ) {
            let mut messages = vec![
                Message::System { content: "sys".to_string() },
            ];
            messages.extend(conversation);

            // Ensure total tokens are below the min threshold.
            let total_tokens = estimate_tokens(&messages);

            // Set min_tokens higher than what we have.
            let min_tokens = total_tokens + 1000;

            let (config, _file) = config_with_memory(
                &memory_content,
                min_tokens,
                1,       // min_messages low so only token guard triggers
                40_000,
            );
            let ctx = make_context(config);
            let layer = SessionMemoryLayer;

            let result = layer.apply(&mut messages, &ctx);
            prop_assert_eq!(result, LayerResult::Noop);
        }

        /// Property 5c: When fewer than min text-block messages exist, the layer returns Noop.
        #[test]
        fn prop_noop_when_fewer_than_min_text_messages(
            // Generate only 1 or 2 text messages.
            conversation in arb_conversation(1, 2, 500, 2000),
            memory_content in "[a-z]{10,50}",
        ) {
            let mut messages = vec![
                Message::System { content: "sys".to_string() },
            ];
            messages.extend(conversation);

            // Set min_messages higher than what we generated (max 4 messages = 2 pairs).
            let (config, _file) = config_with_memory(
                &memory_content,
                1,       // min_tokens = 1 (passes)
                10,      // min_messages = 10 (we only have up to 4)
                40_000,
            );
            let ctx = make_context(config);
            let layer = SessionMemoryLayer;

            let result = layer.apply(&mut messages, &ctx);
            prop_assert_eq!(result, LayerResult::Noop);
        }
    }

    // --- Property 6: Session Memory respects 40k token preservation limit ---
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(30))]

        /// Property 6: For any message history processed by the Session Memory layer,
        /// the total tokens of preserved recent messages SHALL not exceed 40,000 tokens.
        #[test]
        fn prop_preserved_messages_respect_token_limit(
            conversation in arb_conversation(10, 25, 1000, 5000),
            memory_content in "[a-z ]{10,100}",
            max_preserved_tokens in 5000usize..=40_000usize,
        ) {
            let mut messages = vec![
                Message::System { content: "system instruction".to_string() },
            ];
            messages.extend(conversation);

            let (config, _file) = config_with_memory(
                &memory_content,
                1,    // min_tokens = 1 (passes)
                2,    // min_messages = 2 (passes)
                max_preserved_tokens,
            );
            let ctx = make_context(config);
            let layer = SessionMemoryLayer;

            // Compute the preserve boundary used by the layer.
            let preserve_boundary =
                SessionMemoryLayer::compute_preserve_boundary(&messages, max_preserved_tokens);

            // Verify the preserved messages' total tokens don't exceed the limit.
            let preserved_tokens = estimate_tokens(&messages[preserve_boundary..]);
            prop_assert!(
                preserved_tokens <= max_preserved_tokens,
                "Preserved tokens ({}) should not exceed max_preserved_tokens ({})",
                preserved_tokens, max_preserved_tokens
            );

            // Now also apply the layer and verify the same property holds on the result.
            let result = layer.apply(&mut messages, &ctx);
            if let LayerResult::Applied(_) = result {
                // After applying, find the non-system, non-summary messages.
                // These are the "preserved" recent messages.
                let preserved_after: Vec<&Message> = messages.iter()
                    .filter(|m| !matches!(m, Message::System { .. }))
                    .collect();

                let preserved_after_tokens: usize = preserved_after.iter()
                    .map(|m| estimate_single_message_tokens(m))
                    .sum();

                prop_assert!(
                    preserved_after_tokens <= max_preserved_tokens,
                    "After applying layer, preserved message tokens ({}) should not exceed limit ({})",
                    preserved_after_tokens, max_preserved_tokens
                );
            }
        }
    }
}
