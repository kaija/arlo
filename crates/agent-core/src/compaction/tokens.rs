//! Token counting utilities for the compaction system.
//!
//! Provides both a heuristic-based token estimator (chars / 4) and a
//! function that prefers actual Usage data when available.

use crate::message::{ContentBlock, Message, Usage};

/// Estimate tokens for a message list using the chars/4 heuristic.
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| estimate_single_message_tokens(m))
        .sum()
}

/// Estimate tokens for a single message using chars/4 heuristic.
pub fn estimate_single_message_tokens(msg: &Message) -> usize {
    let chars: usize = match msg {
        Message::System { content } => content.len(),
        Message::User { content } => content
            .iter()
            .map(|block| content_block_char_len(block))
            .sum(),
        Message::Assistant { content, .. } => content
            .iter()
            .map(|block| content_block_char_len(block))
            .sum(),
        Message::ToolResult { content, .. } => content.len(),
    };
    chars / 4
}

/// Compute the token count, preferring Usage data over the heuristic.
///
/// When `last_usage` is provided (from the most recent model response),
/// uses `input_tokens` as the authoritative count. Otherwise falls back
/// to the chars/4 estimate.
pub fn compute_token_count(messages: &[Message], last_usage: Option<&Usage>) -> usize {
    match last_usage {
        Some(usage) => usage.input_tokens as usize,
        None => estimate_tokens(messages),
    }
}

/// Character length of a single content block (used by the token estimator).
fn content_block_char_len(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::Image { data, .. } => data.len(),
        ContentBlock::ToolUse { block } => block.name.len() + block.input.to_string().len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolUseBlock;
    use serde_json::json;

    #[test]
    fn test_estimate_system_message() {
        // "Hello" = 5 chars => 5 / 4 = 1
        let msg = Message::System {
            content: "Hello".to_string(),
        };
        assert_eq!(estimate_single_message_tokens(&msg), 1);
    }

    #[test]
    fn test_estimate_user_message_text() {
        // 12 chars => 12 / 4 = 3
        let msg = Message::User {
            content: vec![ContentBlock::Text {
                text: "Hello world!".to_string(),
            }],
        };
        assert_eq!(estimate_single_message_tokens(&msg), 3);
    }

    #[test]
    fn test_estimate_user_message_multiple_blocks() {
        // "Hello" (5) + "World" (5) = 10 chars => 10 / 4 = 2
        let msg = Message::User {
            content: vec![
                ContentBlock::Text {
                    text: "Hello".to_string(),
                },
                ContentBlock::Text {
                    text: "World".to_string(),
                },
            ],
        };
        assert_eq!(estimate_single_message_tokens(&msg), 2);
    }

    #[test]
    fn test_estimate_assistant_message_with_tool_use() {
        let msg = Message::Assistant {
            content: vec![ContentBlock::ToolUse {
                block: ToolUseBlock {
                    id: "tool_1".to_string(),
                    name: "file_read".to_string(),
                    input: json!({"path": "/tmp/test.txt"}),
                },
            }],
            usage: None,
        };
        // name "file_read" (9 chars) + json input string repr
        let expected_chars = "file_read".len() + json!({"path": "/tmp/test.txt"}).to_string().len();
        assert_eq!(estimate_single_message_tokens(&msg), expected_chars / 4);
    }

    #[test]
    fn test_estimate_tool_result() {
        // 20 chars => 20 / 4 = 5
        let msg = Message::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "01234567890123456789".to_string(),
            is_error: false,
        };
        assert_eq!(estimate_single_message_tokens(&msg), 5);
    }

    #[test]
    fn test_estimate_empty_messages() {
        let messages: Vec<Message> = vec![];
        assert_eq!(estimate_tokens(&messages), 0);
    }

    #[test]
    fn test_estimate_tokens_sums_all_messages() {
        let messages = vec![
            Message::System {
                content: "abcd".to_string(), // 4 chars => 1 token
            },
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "abcdefgh".to_string(), // 8 chars => 2 tokens
                }],
            },
        ];
        assert_eq!(estimate_tokens(&messages), 3);
    }

    #[test]
    fn test_compute_token_count_prefers_usage() {
        let messages = vec![Message::System {
            content: "a]".repeat(1000), // Would estimate to 500 tokens
        }];
        let usage = Usage {
            input_tokens: 42,
            output_tokens: 10,
            cache_read_tokens: None,
        };
        assert_eq!(compute_token_count(&messages, Some(&usage)), 42);
    }

    #[test]
    fn test_compute_token_count_falls_back_to_heuristic() {
        let messages = vec![Message::System {
            content: "abcd".to_string(), // 4 chars => 1 token
        }];
        assert_eq!(compute_token_count(&messages, None), 1);
    }

    #[test]
    fn test_estimate_image_block() {
        let msg = Message::User {
            content: vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "a]".repeat(100), // 200 chars => 50 tokens
                source_type: "base64".to_string(),
            }],
        };
        assert_eq!(estimate_single_message_tokens(&msg), 50);
    }

    #[test]
    fn test_integer_division_truncation() {
        // 3 chars => 3 / 4 = 0 (integer division truncates)
        let msg = Message::System {
            content: "abc".to_string(),
        };
        assert_eq!(estimate_single_message_tokens(&msg), 0);
    }
}
