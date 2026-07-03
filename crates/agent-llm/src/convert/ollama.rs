//! Ollama message format converter.
//!
//! Converts canonical `Message` types to/from Ollama chat API wire format.
//! Ollama uses a simplified message format without multi-content-block support.
//!
//! ## Wire Format
//!
//! - System → `{"role": "system", "content": "..."}`
//! - User → `{"role": "user", "content": "..."}`
//! - Assistant → `{"role": "assistant", "content": "..."}`
//! - (No native tool support — tool use blocks are serialized as text)

use agent_core::message::{ContentBlock, Message};
use serde_json::{json, Value};

use super::ConvertError;

/// Convert a slice of canonical messages to Ollama wire format.
///
/// Ollama uses a simplified format where each message has a single
/// string `content` field. Multi-block content is flattened to text.
pub fn to_wire(messages: &[Message]) -> Vec<Value> {
    messages.iter().map(message_to_wire).collect()
}

/// Convert a single canonical message to Ollama wire format.
fn message_to_wire(msg: &Message) -> Value {
    match msg {
        Message::System { content } => {
            json!({
                "role": "system",
                "content": content
            })
        }
        Message::User { content } => {
            let text = flatten_content_blocks(content);
            json!({
                "role": "user",
                "content": text
            })
        }
        Message::Assistant { content, .. } => {
            let text = flatten_content_blocks(content);
            json!({
                "role": "assistant",
                "content": text
            })
        }
        Message::ToolResult { content, .. } => {
            // Ollama doesn't have native tool result support,
            // so we format it as a user message with context.
            json!({
                "role": "user",
                "content": content
            })
        }
    }
}

/// Flatten a vector of content blocks into a single string.
///
/// Text blocks are joined with newlines. Image blocks are represented
/// as "[image]" placeholders. Tool use blocks are formatted as text
/// representations.
fn flatten_content_blocks(blocks: &[ContentBlock]) -> String {
    let parts: Vec<String> = blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { .. } => "[image]".to_string(),
            ContentBlock::ToolUse { block: tool_use } => {
                format!("[tool_use: {}({})]", tool_use.name, tool_use.input)
            }
        })
        .collect();
    parts.join("\n")
}

/// Convert an Ollama wire-format response into canonical content blocks.
///
/// Expects a message object like:
/// ```json
/// {"role": "assistant", "content": "Hello!"}
/// ```
///
/// Since Ollama uses simple string content, the result is always a
/// single `ContentBlock::Text`.
pub fn from_wire(wire: &Value) -> Result<Vec<ContentBlock>, ConvertError> {
    // Accept either the full message object or just a content string
    let content_str = if let Some(content) = wire.get("content") {
        content.as_str().ok_or_else(|| ConvertError::InvalidValue {
            field: "content".to_string(),
            context: "Ollama response".to_string(),
            detail: "expected string".to_string(),
        })?
    } else if let Some(s) = wire.as_str() {
        s
    } else {
        return Err(ConvertError::MissingField {
            field: "content".to_string(),
            context: "Ollama response".to_string(),
        });
    };

    if content_str.is_empty() {
        return Ok(vec![]);
    }

    Ok(vec![ContentBlock::Text {
        text: content_str.to_string(),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::message::{ToolUseBlock, Usage};

    #[test]
    fn system_message_to_wire() {
        let msgs = vec![Message::System {
            content: "You are helpful.".to_string(),
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "system");
        assert_eq!(wire[0]["content"], "You are helpful.");
    }

    #[test]
    fn user_text_message_to_wire() {
        let msgs = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
            }],
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "user");
        assert_eq!(wire[0]["content"], "Hello");
    }

    #[test]
    fn user_multi_block_flattened() {
        let msgs = vec![Message::User {
            content: vec![
                ContentBlock::Text {
                    text: "Look at this:".to_string(),
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "abc".to_string(),
                    source_type: "base64".to_string(),
                },
            ],
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["content"], "Look at this:\n[image]");
    }

    #[test]
    fn assistant_message_to_wire() {
        let msgs = vec![Message::Assistant {
            content: vec![ContentBlock::Text {
                text: "Sure!".to_string(),
            }],
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: None,
            }),
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "assistant");
        assert_eq!(wire[0]["content"], "Sure!");
    }

    #[test]
    fn assistant_tool_use_flattened() {
        let msgs = vec![Message::Assistant {
            content: vec![ContentBlock::ToolUse {
                block: ToolUseBlock {
                    id: "tu_1".to_string(),
                    name: "shell".to_string(),
                    input: serde_json::json!({"cmd": "ls"}),
                },
            }],
            usage: None,
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "assistant");
        let content = wire[0]["content"].as_str().unwrap();
        assert!(content.contains("shell"));
        assert!(content.contains("cmd"));
    }

    #[test]
    fn tool_result_to_wire() {
        let msgs = vec![Message::ToolResult {
            tool_use_id: "tu_1".to_string(),
            content: "file contents here".to_string(),
            is_error: false,
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "user");
        assert_eq!(wire[0]["content"], "file contents here");
    }

    #[test]
    fn from_wire_simple_response() {
        let wire = json!({
            "role": "assistant",
            "content": "Hello there!"
        });
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            ContentBlock::Text {
                text: "Hello there!".to_string()
            }
        );
    }

    #[test]
    fn from_wire_empty_content() {
        let wire = json!({
            "role": "assistant",
            "content": ""
        });
        let blocks = from_wire(&wire).unwrap();
        assert!(blocks.is_empty());
    }

    #[test]
    fn from_wire_missing_content_returns_error() {
        let wire = json!({"role": "assistant"});
        let result = from_wire(&wire);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConvertError::MissingField { .. }
        ));
    }

    #[test]
    fn from_wire_non_string_content_returns_error() {
        let wire = json!({"role": "assistant", "content": 42});
        let result = from_wire(&wire);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConvertError::InvalidValue { .. }
        ));
    }

    #[test]
    fn roundtrip_simple_text() {
        let original_text = "Hello, world!";
        let msg = Message::Assistant {
            content: vec![ContentBlock::Text {
                text: original_text.to_string(),
            }],
            usage: None,
        };
        let wire = to_wire(&[msg]);
        let recovered = from_wire(&wire[0]).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0],
            ContentBlock::Text {
                text: original_text.to_string()
            }
        );
    }
}
