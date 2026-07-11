//! OpenAI message format converter.
//!
//! Converts canonical `Message` types to/from OpenAI Chat Completions
//! wire format.
//!
//! ## Wire Format
//!
//! - System → `{"role": "system", "content": "..."}`
//! - User → `{"role": "user", "content": [{"type": "text", "text": "..."}, ...]}`
//! - Assistant → `{"role": "assistant", "content": "..." or [...], "tool_calls": [...]}`
//! - ToolResult → `{"role": "tool", "tool_call_id": "...", "content": "..."}`

use agent_core::message::{ContentBlock, Message, ToolUseBlock};
use serde_json::{json, Value};

use super::ConvertError;

/// Convert a slice of canonical messages to OpenAI wire format.
pub fn to_wire(messages: &[Message]) -> Vec<Value> {
    messages.iter().map(message_to_wire).collect()
}

/// Convert a single canonical message to OpenAI wire format.
fn message_to_wire(msg: &Message) -> Value {
    match msg {
        Message::System { content } => {
            json!({
                "role": "system",
                "content": content
            })
        }
        Message::User { content } => {
            let blocks: Vec<Value> = content.iter().map(content_block_to_wire).collect();
            json!({
                "role": "user",
                "content": blocks
            })
        }
        Message::Assistant { content, .. } => {
            let mut obj = json!({ "role": "assistant" });

            // Separate text/image blocks from tool_use blocks
            let mut text_blocks: Vec<Value> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();

            for block in content {
                match block {
                    ContentBlock::ToolUse { block: tool_use } => {
                        tool_calls.push(json!({
                            "id": tool_use.id,
                            "type": "function",
                            "function": {
                                "name": tool_use.name,
                                "arguments": tool_use.input.to_string()
                            }
                        }));
                    }
                    _ => {
                        text_blocks.push(content_block_to_wire(block));
                    }
                }
            }

            // If only one text block and no images, use simple string content
            if text_blocks.len() == 1
                && matches!(&content[0], ContentBlock::Text { .. })
                && tool_calls.is_empty()
            {
                if let ContentBlock::Text { text } = &content[0] {
                    obj["content"] = json!(text);
                } else {
                    obj["content"] = json!(text_blocks);
                }
            } else if !text_blocks.is_empty() {
                obj["content"] = json!(text_blocks);
            }

            if !tool_calls.is_empty() {
                obj["tool_calls"] = json!(tool_calls);
            }

            obj
        }
        Message::ToolResult {
            tool_use_id,
            content,
            ..
        } => {
            json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content
            })
        }
    }
}

/// Convert a single content block to OpenAI wire format.
fn content_block_to_wire(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => {
            json!({
                "type": "text",
                "text": text
            })
        }
        ContentBlock::Image {
            media_type, data, ..
        } => {
            json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", media_type, data)
                }
            })
        }
        ContentBlock::ToolUse { block: tool_use } => {
            // Tool use blocks shouldn't appear here in normal flow (they're
            // handled as tool_calls), but handle gracefully.
            json!({
                "type": "text",
                "text": format!("[tool_use: {}({})]", tool_use.name, tool_use.input)
            })
        }
    }
}

/// Convert an OpenAI wire-format response into canonical content blocks.
///
/// Expects the response JSON to be a single assistant message object, e.g.:
/// ```json
/// {
///   "role": "assistant",
///   "content": "Hello!",
///   "tool_calls": [...]
/// }
/// ```
pub fn from_wire(wire: &Value) -> Result<Vec<ContentBlock>, ConvertError> {
    let mut blocks = Vec::new();

    // Parse content field (can be string or array)
    if let Some(content) = wire.get("content") {
        match content {
            Value::String(text) => {
                if !text.is_empty() {
                    blocks.push(ContentBlock::Text { text: text.clone() });
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    blocks.push(parse_content_block(item)?);
                }
            }
            Value::Null => {}
            _ => {
                return Err(ConvertError::InvalidValue {
                    field: "content".to_string(),
                    context: "OpenAI response".to_string(),
                    detail: "expected string, array, or null".to_string(),
                });
            }
        }
    }

    // Parse tool_calls array
    if let Some(Value::Array(tool_calls)) = wire.get("tool_calls") {
        for tc in tool_calls {
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField {
                    field: "id".to_string(),
                    context: "OpenAI tool_call".to_string(),
                })?
                .to_string();

            let function = tc
                .get("function")
                .ok_or_else(|| ConvertError::MissingField {
                    field: "function".to_string(),
                    context: "OpenAI tool_call".to_string(),
                })?;

            let name = function
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ConvertError::MissingField {
                    field: "function.name".to_string(),
                    context: "OpenAI tool_call".to_string(),
                })?
                .to_string();

            let arguments_str = function
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");

            let input: Value = serde_json::from_str(arguments_str).unwrap_or(json!({}));

            blocks.push(ContentBlock::ToolUse {
                block: ToolUseBlock { id, name, input },
            });
        }
    }

    Ok(blocks)
}

/// Parse a single content block from the OpenAI content array.
fn parse_content_block(item: &Value) -> Result<ContentBlock, ConvertError> {
    let block_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("text");

    match block_type {
        "text" => {
            let text = item
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(ContentBlock::Text { text })
        }
        "image_url" => {
            // Parse data URL: "data:image/png;base64,..."
            let url = item
                .get("image_url")
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let (media_type, data) = parse_data_url(url);
            Ok(ContentBlock::Image {
                media_type,
                data,
                source_type: "base64".to_string(),
            })
        }
        other => Err(ConvertError::UnknownBlockType(other.to_string())),
    }
}

/// Parse a data URL into (media_type, base64_data).
fn parse_data_url(url: &str) -> (String, String) {
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some(semicolon) = rest.find(';') {
            let media_type = &rest[..semicolon];
            let after_semi = &rest[semicolon + 1..];
            if let Some(data) = after_semi.strip_prefix("base64,") {
                return (media_type.to_string(), data.to_string());
            }
        }
    }
    // Fallback: treat entire URL as data
    ("application/octet-stream".to_string(), url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::message::Usage;
    use serde_json::json;

    #[test]
    fn system_message_to_wire() {
        let msgs = vec![Message::System {
            content: "You are helpful.".to_string(),
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["role"], "system");
        assert_eq!(wire[0]["content"], "You are helpful.");
    }

    #[test]
    fn user_message_with_text_to_wire() {
        let msgs = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
            }],
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "user");
        assert_eq!(wire[0]["content"][0]["type"], "text");
        assert_eq!(wire[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn user_message_with_image_to_wire() {
        let msgs = vec![Message::User {
            content: vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "abc123".to_string(),
                source_type: "base64".to_string(),
            }],
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["content"][0]["type"], "image_url");
        assert_eq!(
            wire[0]["content"][0]["image_url"]["url"],
            "data:image/png;base64,abc123"
        );
    }

    #[test]
    fn assistant_simple_text_to_wire() {
        let msgs = vec![Message::Assistant {
            content: vec![ContentBlock::Text {
                text: "Hi there".to_string(),
            }],
            usage: None,
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "assistant");
        assert_eq!(wire[0]["content"], "Hi there");
    }

    #[test]
    fn assistant_with_tool_calls_to_wire() {
        let msgs = vec![Message::Assistant {
            content: vec![
                ContentBlock::Text {
                    text: "Let me check.".to_string(),
                },
                ContentBlock::ToolUse {
                    block: ToolUseBlock {
                        id: "call_1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "/tmp/test.txt"}),
                    },
                },
            ],
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: None,
            }),
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "assistant");
        assert_eq!(wire[0]["content"][0]["type"], "text");
        assert_eq!(wire[0]["content"][0]["text"], "Let me check.");
        assert_eq!(wire[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(wire[0]["tool_calls"][0]["type"], "function");
        assert_eq!(wire[0]["tool_calls"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn tool_result_to_wire() {
        let msgs = vec![Message::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: "File contents here".to_string(),
            is_error: false,
        }];
        let wire = to_wire(&msgs);
        assert_eq!(wire[0]["role"], "tool");
        assert_eq!(wire[0]["tool_call_id"], "call_1");
        assert_eq!(wire[0]["content"], "File contents here");
    }

    #[test]
    fn from_wire_simple_text() {
        let wire = json!({
            "role": "assistant",
            "content": "Hello!"
        });
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
    fn from_wire_with_tool_calls() {
        let wire = json!({
            "role": "assistant",
            "content": "Let me help.",
            "tool_calls": [{
                "id": "call_abc",
                "type": "function",
                "function": {
                    "name": "shell",
                    "arguments": "{\"command\":\"ls\"}"
                }
            }]
        });
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0],
            ContentBlock::Text {
                text: "Let me help.".to_string()
            }
        );
        match &blocks[1] {
            ContentBlock::ToolUse { block } => {
                assert_eq!(block.id, "call_abc");
                assert_eq!(block.name, "shell");
                assert_eq!(block.input, json!({"command": "ls"}));
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn from_wire_null_content() {
        let wire = json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "read_file",
                    "arguments": "{}"
                }
            }]
        });
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolUse { block } => {
                assert_eq!(block.id, "call_1");
                assert_eq!(block.name, "read_file");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn from_wire_array_content() {
        let wire = json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Here you go."}
            ]
        });
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            ContentBlock::Text {
                text: "Here you go.".to_string()
            }
        );
    }

    #[test]
    fn from_wire_image_url() {
        let wire = json!({
            "role": "assistant",
            "content": [
                {
                    "type": "image_url",
                    "image_url": {
                        "url": "data:image/jpeg;base64,/9j/abc"
                    }
                }
            ]
        });
        let blocks = from_wire(&wire).unwrap();
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::Image {
                media_type, data, ..
            } => {
                assert_eq!(media_type, "image/jpeg");
                assert_eq!(data, "/9j/abc");
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn from_wire_unknown_block_type_returns_error() {
        let wire = json!({
            "content": [{"type": "audio", "data": "..."}]
        });
        let result = from_wire(&wire);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConvertError::UnknownBlockType(_)
        ));
    }

    #[test]
    fn from_wire_missing_tool_call_id_returns_error() {
        let wire = json!({
            "content": null,
            "tool_calls": [{"type": "function", "function": {"name": "x", "arguments": "{}"}}]
        });
        let result = from_wire(&wire);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ConvertError::MissingField { .. }
        ));
    }

    #[test]
    fn roundtrip_text_message() {
        let original = vec![ContentBlock::Text {
            text: "Hello world".to_string(),
        }];
        let msg = Message::Assistant {
            content: original.clone(),
            usage: None,
        };
        let wire = to_wire(&[msg]);
        let recovered = from_wire(&wire[0]).unwrap();
        assert_eq!(recovered, original);
    }

    #[test]
    fn roundtrip_tool_use_message() {
        let original = vec![
            ContentBlock::Text {
                text: "Running tool.".to_string(),
            },
            ContentBlock::ToolUse {
                block: ToolUseBlock {
                    id: "tc_1".to_string(),
                    name: "grep".to_string(),
                    input: json!({"pattern": "foo", "path": "."}),
                },
            },
        ];
        let msg = Message::Assistant {
            content: original.clone(),
            usage: None,
        };
        let wire = to_wire(&[msg]);
        let recovered = from_wire(&wire[0]).unwrap();
        assert_eq!(recovered, original);
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
                "[a-zA-Z0-9 _-]{0,50}".prop_map(serde_json::Value::String),
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
                "[a-zA-Z0-9_-]{1,20}",         // id
                "[a-zA-Z_][a-zA-Z0-9_]{0,20}", // name
                arb_json_value(),              // input
            )
                .prop_map(|(id, name, input)| ToolUseBlock { id, name, input })
        }

        /// Strategy for non-empty text (OpenAI's from_wire skips empty strings).
        fn arb_nonempty_text() -> impl Strategy<Value = String> {
            "[^\x00]{1,100}"
        }

        proptest! {
            /// **Validates: Requirements 16.7**
            ///
            /// For any non-empty text string, an Assistant message with a single
            /// text block round-trips through OpenAI wire format.
            #[test]
            fn prop_openai_single_text_roundtrip(
                text in arb_nonempty_text()
            ) {
                let original = vec![ContentBlock::Text { text }];
                let msg = Message::Assistant {
                    content: original.clone(),
                    usage: None,
                };
                let wire = to_wire(&[msg]);
                let recovered = from_wire(&wire[0]).unwrap();
                prop_assert_eq!(recovered, original);
            }

            /// **Validates: Requirements 16.7**
            ///
            /// For any combination of text and tool_use blocks, OpenAI round-trip
            /// preserves all content blocks.
            #[test]
            fn prop_openai_text_and_tool_calls_roundtrip(
                text in arb_nonempty_text(),
                tool_uses in prop::collection::vec(arb_tool_use_block(), 1..4)
            ) {
                let mut original = vec![ContentBlock::Text { text }];
                for tu in tool_uses {
                    original.push(ContentBlock::ToolUse { block: tu });
                }

                let msg = Message::Assistant {
                    content: original.clone(),
                    usage: None,
                };
                let wire = to_wire(&[msg]);
                let recovered = from_wire(&wire[0]).unwrap();
                prop_assert_eq!(recovered, original);
            }

            /// **Validates: Requirements 16.7**
            ///
            /// For any tool_use blocks without text, the round-trip preserves them.
            #[test]
            fn prop_openai_tool_calls_only_roundtrip(
                tool_uses in prop::collection::vec(arb_tool_use_block(), 1..4)
            ) {
                let original: Vec<ContentBlock> = tool_uses
                    .into_iter()
                    .map(|tu| ContentBlock::ToolUse { block: tu })
                    .collect();

                let msg = Message::Assistant {
                    content: original.clone(),
                    usage: None,
                };
                let wire = to_wire(&[msg]);
                let recovered = from_wire(&wire[0]).unwrap();
                prop_assert_eq!(recovered, original);
            }
        }
    }
}
