//! Anthropic message format converter.
//!
//! Converts canonical `Message` types to/from Anthropic Messages API
//! wire format.
//!
//! ## Wire Format
//!
//! - System message is separate (not in messages array) — extracted via `extract_system()`
//! - User → `{"role": "user", "content": [...]}`
//! - Assistant → `{"role": "assistant", "content": [...]}`
//! - ToolResult → `{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "...", "content": "..."}]}`

use agent_core::message::{ContentBlock, Message, ToolUseBlock};
use serde_json::{json, Value};

use super::ConvertError;

/// Result of converting canonical messages to Anthropic wire format.
///
/// The Anthropic API separates the system prompt from the messages array,
/// so this struct holds both pieces.
#[derive(Debug, Clone)]
pub struct AnthropicWireMessages {
    /// The system prompt (extracted from System messages).
    pub system: Option<String>,
    /// The messages array for the API request.
    pub messages: Vec<Value>,
}

/// Convert a slice of canonical messages to Anthropic wire format.
///
/// System messages are extracted and concatenated into a separate system string.
/// The remaining messages are converted to the Anthropic messages array format.
pub fn to_wire(messages: &[Message]) -> AnthropicWireMessages {
    let mut system_parts: Vec<String> = Vec::new();
    let mut wire_messages: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::System { content } => {
                system_parts.push(content.clone());
            }
            Message::User { content } => {
                let blocks: Vec<Value> = content.iter().map(content_block_to_wire).collect();
                wire_messages.push(json!({
                    "role": "user",
                    "content": blocks
                }));
            }
            Message::Assistant { content, .. } => {
                let blocks: Vec<Value> = content.iter().map(content_block_to_wire).collect();
                wire_messages.push(json!({
                    "role": "assistant",
                    "content": blocks
                }));
            }
            Message::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut result_block = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content
                });
                if *is_error {
                    result_block["is_error"] = json!(true);
                }
                wire_messages.push(json!({
                    "role": "user",
                    "content": [result_block]
                }));
            }
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n"))
    };

    AnthropicWireMessages {
        system,
        messages: wire_messages,
    }
}

/// Convert a single content block to Anthropic wire format.
fn content_block_to_wire(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => {
            json!({
                "type": "text",
                "text": text
            })
        }
        ContentBlock::Image {
            media_type,
            data,
            source_type,
        } => {
            json!({
                "type": "image",
                "source": {
                    "type": source_type,
                    "media_type": media_type,
                    "data": data
                }
            })
        }
        ContentBlock::ToolUse { block: tool_use } => {
            json!({
                "type": "tool_use",
                "id": tool_use.id,
                "name": tool_use.name,
                "input": tool_use.input
            })
        }
    }
}

/// Convert an Anthropic wire-format response into canonical content blocks.
///
/// Expects the response content array from an Anthropic message, e.g.:
/// ```json
/// [
///   {"type": "text", "text": "Hello"},
///   {"type": "tool_use", "id": "tu_1", "name": "shell", "input": {...}}
/// ]
/// ```
pub fn from_wire(wire: &Value) -> Result<Vec<ContentBlock>, ConvertError> {
    let content_array = match wire {
        Value::Array(arr) => arr,
        Value::Object(obj) => {
            // Accept a full message object with a "content" field
            if let Some(Value::Array(arr)) = obj.get("content") {
                arr
            } else {
                return Err(ConvertError::MissingField {
                    field: "content".to_string(),
                    context: "Anthropic response".to_string(),
                });
            }
        }
        _ => {
            return Err(ConvertError::InvalidValue {
                field: "response".to_string(),
                context: "Anthropic response".to_string(),
                detail: "expected array or object".to_string(),
            });
        }
    };

    let mut blocks = Vec::new();
    for item in content_array {
        blocks.push(parse_content_block(item)?);
    }
    Ok(blocks)
}

/// Parse a single content block from an Anthropic content array.
fn parse_content_block(item: &Value) -> Result<ContentBlock, ConvertError> {
    let block_type = item
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ConvertError::MissingField {
            field: "type".to_string(),
            context: "Anthropic content block".to_string(),
        })?;

    match block_type {
        "text" => {
            let text = item
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ContentBlock::Text { text })
        }
        "image" => {
            let source = item.get("source").ok_or_else(|| ConvertError::MissingField {
                field: "source".to_string(),
                context: "Anthropic image block".to_string(),
            })?;
            let media_type = source
                .get("media_type")
                .and_then(|v| v.as_str())
                .unwrap_or("application/octet-stream")
                .to_string();
            let data = source
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let source_type = source
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("base64")
                .to_string();
            Ok(ContentBlock::Image {
                media_type,
                data,
                source_type,
            })
        }
        "tool_use" => {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField {
                    field: "id".to_string(),
                    context: "Anthropic tool_use block".to_string(),
                })?
                .to_string();
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField {
                    field: "name".to_string(),
                    context: "Anthropic tool_use block".to_string(),
                })?
                .to_string();
            let input = item.get("input").cloned().unwrap_or(json!({}));
            Ok(ContentBlock::ToolUse {
                block: ToolUseBlock { id, name, input },
            })
        }
        other => Err(ConvertError::UnknownBlockType(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::message::Usage;

    #[test]
    fn system_message_extracted_separately() {
        let msgs = vec![
            Message::System {
                content: "You are helpful.".to_string(),
            },
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "Hi".to_string(),
                }],
            },
        ];
        let result = to_wire(&msgs);
        assert_eq!(result.system, Some("You are helpful.".to_string()));
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0]["role"], "user");
    }

    #[test]
    fn multiple_system_messages_concatenated() {
        let msgs = vec![
            Message::System {
                content: "Part 1.".to_string(),
            },
            Message::System {
                content: "Part 2.".to_string(),
            },
        ];
        let result = to_wire(&msgs);
        assert_eq!(result.system, Some("Part 1.\nPart 2.".to_string()));
        assert!(result.messages.is_empty());
    }

    #[test]
    fn user_message_to_wire() {
        let msgs = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
            }],
        }];
        let result = to_wire(&msgs);
        assert_eq!(result.messages[0]["role"], "user");
        assert_eq!(result.messages[0]["content"][0]["type"], "text");
        assert_eq!(result.messages[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn assistant_message_with_tool_use_to_wire() {
        let msgs = vec![Message::Assistant {
            content: vec![
                ContentBlock::Text {
                    text: "Let me check.".to_string(),
                },
                ContentBlock::ToolUse {
                    block: ToolUseBlock {
                        id: "tu_1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "/tmp/x"}),
                    },
                },
            ],
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: None,
            }),
        }];
        let result = to_wire(&msgs);
        assert_eq!(result.messages[0]["role"], "assistant");
        assert_eq!(result.messages[0]["content"][0]["type"], "text");
        assert_eq!(result.messages[0]["content"][1]["type"], "tool_use");
        assert_eq!(result.messages[0]["content"][1]["id"], "tu_1");
        assert_eq!(result.messages[0]["content"][1]["name"], "read_file");
        assert_eq!(result.messages[0]["content"][1]["input"], json!({"path": "/tmp/x"}));
    }

    #[test]
    fn tool_result_to_wire() {
        let msgs = vec![Message::ToolResult {
            tool_use_id: "tu_1".to_string(),
            content: "file contents".to_string(),
            is_error: false,
        }];
        let result = to_wire(&msgs);
        assert_eq!(result.messages[0]["role"], "user");
        assert_eq!(result.messages[0]["content"][0]["type"], "tool_result");
        assert_eq!(result.messages[0]["content"][0]["tool_use_id"], "tu_1");
        assert_eq!(result.messages[0]["content"][0]["content"], "file contents");
        assert!(result.messages[0]["content"][0].get("is_error").is_none());
    }

    #[test]
    fn tool_result_error_to_wire() {
        let msgs = vec![Message::ToolResult {
            tool_use_id: "tu_2".to_string(),
            content: "permission denied".to_string(),
            is_error: true,
        }];
        let result = to_wire(&msgs);
        assert_eq!(result.messages[0]["content"][0]["is_error"], true);
    }

    #[test]
    fn image_to_wire() {
        let msgs = vec![Message::User {
            content: vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "base64data".to_string(),
                source_type: "base64".to_string(),
            }],
        }];
        let result = to_wire(&msgs);
        let img = &result.messages[0]["content"][0];
        assert_eq!(img["type"], "image");
        assert_eq!(img["source"]["type"], "base64");
        assert_eq!(img["source"]["media_type"], "image/png");
        assert_eq!(img["source"]["data"], "base64data");
    }

    #[test]
    fn from_wire_text_block() {
        let wire = json!([
            {"type": "text", "text": "Hello!"}
        ]);
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            ContentBlock::Text {
                text: "Hello!".to_string()
            }
        );
    }

    #[test]
    fn from_wire_tool_use_block() {
        let wire = json!([
            {
                "type": "tool_use",
                "id": "tu_abc",
                "name": "shell",
                "input": {"command": "ls"}
            }
        ]);
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolUse { block } => {
                assert_eq!(block.id, "tu_abc");
                assert_eq!(block.name, "shell");
                assert_eq!(block.input, json!({"command": "ls"}));
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn from_wire_image_block() {
        let wire = json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": "abc123"
                }
            }
        ]);
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Image {
                media_type, data, source_type
            } => {
                assert_eq!(media_type, "image/jpeg");
                assert_eq!(data, "abc123");
                assert_eq!(source_type, "base64");
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn from_wire_message_object() {
        let wire = json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Done."}
            ]
        });
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            ContentBlock::Text {
                text: "Done.".to_string()
            }
        );
    }

    #[test]
    fn from_wire_unknown_type_returns_error() {
        let wire = json!([{"type": "audio", "data": "..."}]);
        let result = from_wire(&wire);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConvertError::UnknownBlockType(_)));
    }

    #[test]
    fn from_wire_missing_tool_use_id_returns_error() {
        let wire = json!([{"type": "tool_use", "name": "x", "input": {}}]);
        let result = from_wire(&wire);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ConvertError::MissingField { .. }));
    }

    #[test]
    fn roundtrip_text_and_tool_use() {
        let original = vec![
            ContentBlock::Text {
                text: "Checking...".to_string(),
            },
            ContentBlock::ToolUse {
                block: ToolUseBlock {
                    id: "tu_42".to_string(),
                    name: "grep".to_string(),
                    input: json!({"pattern": "TODO"}),
                },
            },
        ];
        let msg = Message::Assistant {
            content: original.clone(),
            usage: None,
        };
        let wire_result = to_wire(&[msg]);
        // The assistant message content array
        let wire_content = &wire_result.messages[0]["content"];
        let recovered = from_wire(wire_content).unwrap();
        assert_eq!(recovered, original);
    }

    #[test]
    fn no_system_messages_yields_none() {
        let msgs = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "Hi".to_string(),
            }],
        }];
        let result = to_wire(&msgs);
        assert_eq!(result.system, None);
    }

    // --- Property-based tests ---
    // Property 15: Message format conversion round-trip
    // Validates: Requirements 16.7

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        /// Strategy for generating arbitrary JSON values (limited depth).
        fn arb_json_value() -> impl Strategy<Value = serde_json::Value> {
            let leaf = prop_oneof![
                Just(serde_json::Value::Null),
                any::<bool>().prop_map(serde_json::Value::Bool),
                any::<i64>().prop_map(|n| serde_json::Value::Number(serde_json::Number::from(n))),
                "[a-zA-Z0-9 _-]{0,50}".prop_map(|s| serde_json::Value::String(s)),
            ];

            leaf.prop_recursive(
                2,  // depth
                32, // max nodes
                5,  // items per collection
                |inner| {
                    prop_oneof![
                        prop::collection::vec(inner.clone(), 0..3)
                            .prop_map(serde_json::Value::Array),
                        prop::collection::hash_map("[a-zA-Z_][a-zA-Z0-9_]{0,10}", inner, 0..3)
                            .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
                    ]
                },
            )
        }

        /// Strategy for generating arbitrary ToolUseBlock values.
        fn arb_tool_use_block() -> impl Strategy<Value = ToolUseBlock> {
            (
                "[a-zA-Z0-9_-]{1,20}",            // id
                "[a-zA-Z_][a-zA-Z0-9_]{0,20}",    // name
                arb_json_value(),                  // input
            )
                .prop_map(|(id, name, input)| ToolUseBlock { id, name, input })
        }

        /// Strategy for generating content blocks that round-trip through Anthropic.
        /// Anthropic supports Text, Image, and ToolUse in assistant content.
        fn arb_anthropic_content_block() -> impl Strategy<Value = ContentBlock> {
            prop_oneof![
                "[^\x00]{0,100}".prop_map(|text| ContentBlock::Text { text }),
                (
                    "(image/png|image/jpeg|image/gif|image/webp)",
                    "[a-zA-Z0-9+/=]{0,50}",
                    Just("base64".to_string()),
                )
                    .prop_map(|(media_type, data, source_type)| ContentBlock::Image {
                        media_type,
                        data,
                        source_type,
                    }),
                arb_tool_use_block().prop_map(|block| ContentBlock::ToolUse { block }),
            ]
        }

        /// Strategy for generating Assistant content blocks that round-trip.
        fn arb_assistant_content() -> impl Strategy<Value = Vec<ContentBlock>> {
            prop::collection::vec(arb_anthropic_content_block(), 1..6)
        }

        proptest! {
            /// **Validates: Requirements 16.7**
            ///
            /// For any valid Assistant message content blocks, converting to
            /// Anthropic wire format and back preserves all content blocks.
            #[test]
            fn prop_anthropic_assistant_roundtrip(
                content in arb_assistant_content()
            ) {
                let msg = Message::Assistant {
                    content: content.clone(),
                    usage: None,
                };
                let wire_result = to_wire(&[msg]);
                // The assistant message content array is at messages[0]["content"]
                let wire_content = &wire_result.messages[0]["content"];
                let recovered = from_wire(wire_content).unwrap();
                prop_assert_eq!(recovered, content);
            }

            /// For any single Text content block, the Anthropic round-trip preserves it.
            #[test]
            fn prop_anthropic_text_only_roundtrip(
                text in "[^\x00]{0,200}"
            ) {
                let content = vec![ContentBlock::Text { text: text.clone() }];
                let msg = Message::Assistant {
                    content: content.clone(),
                    usage: None,
                };
                let wire_result = to_wire(&[msg]);
                let wire_content = &wire_result.messages[0]["content"];
                let recovered = from_wire(wire_content).unwrap();
                prop_assert_eq!(recovered, content);
            }

            /// For any combination of text and tool_use blocks, Anthropic round-trips correctly.
            #[test]
            fn prop_anthropic_text_and_tool_use_roundtrip(
                text in "[^\x00]{1,100}",
                tool_use in arb_tool_use_block()
            ) {
                let content = vec![
                    ContentBlock::Text { text },
                    ContentBlock::ToolUse { block: tool_use },
                ];
                let msg = Message::Assistant {
                    content: content.clone(),
                    usage: None,
                };
                let wire_result = to_wire(&[msg]);
                let wire_content = &wire_result.messages[0]["content"];
                let recovered = from_wire(wire_content).unwrap();
                prop_assert_eq!(recovered, content);
            }
        }
    }
}
