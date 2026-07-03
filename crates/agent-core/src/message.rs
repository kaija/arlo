//! Core message types representing conversation history.
//!
//! All components in the framework share these canonical message types
//! for representing the conversation between user, assistant, and tools.

use serde::{Deserialize, Serialize};

/// A single message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    /// A system message providing instructions to the model.
    System { content: String },
    /// A user message containing one or more content blocks.
    User { content: Vec<ContentBlock> },
    /// An assistant message containing one or more content blocks, with optional usage info.
    Assistant {
        content: Vec<ContentBlock>,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    /// A tool result message returned after tool execution.
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

/// A block of content within a User or Assistant message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text { text: String },
    /// An image encoded as base64 data.
    Image {
        media_type: String,
        data: String,
        source_type: String,
    },
    /// A tool use request from the assistant.
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(flatten)]
        block: ToolUseBlock,
    },
}

/// A tool use block representing a request to execute a tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolUseBlock {
    /// Unique identifier for this tool use invocation.
    pub id: String,
    /// The name of the tool to execute.
    pub name: String,
    /// The JSON input arguments for the tool.
    pub input: serde_json::Value,
}

/// Token usage statistics for a model response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Usage {
    /// Number of input tokens consumed.
    pub input_tokens: u64,
    /// Number of output tokens generated.
    pub output_tokens: u64,
    /// Number of tokens read from cache, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_system_message_roundtrip() {
        let msg = Message::System {
            content: "You are a helpful assistant.".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_user_message_with_text() {
        let msg = Message::User {
            content: vec![ContentBlock::Text {
                text: "Hello, world!".to_string(),
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_user_message_with_image() {
        let msg = Message::User {
            content: vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "base64data==".to_string(),
                source_type: "base64".to_string(),
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_assistant_message_with_tool_use() {
        let msg = Message::Assistant {
            content: vec![ContentBlock::ToolUse {
                block: ToolUseBlock {
                    id: "tool_123".to_string(),
                    name: "read_file".to_string(),
                    input: json!({"path": "/tmp/test.txt"}),
                },
            }],
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: Some(20),
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_assistant_message_without_usage() {
        let msg = Message::Assistant {
            content: vec![ContentBlock::Text {
                text: "Here is my response.".to_string(),
            }],
            usage: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_tool_result_message() {
        let msg = Message::ToolResult {
            tool_use_id: "tool_123".to_string(),
            content: "File contents here".to_string(),
            is_error: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_tool_result_error() {
        let msg = Message::ToolResult {
            tool_use_id: "tool_456".to_string(),
            content: "Permission denied".to_string(),
            is_error: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_usage_default() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, None);
    }

    #[test]
    fn test_content_block_text_roundtrip() {
        let block = ContentBlock::Text {
            text: "hello".to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deserialized);
    }

    #[test]
    fn test_tool_use_block_with_complex_input() {
        let block = ToolUseBlock {
            id: "tu_001".to_string(),
            name: "shell".to_string(),
            input: json!({
                "command": "ls -la",
                "working_dir": "/home/user",
                "timeout": 30
            }),
        };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ToolUseBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(block, deserialized);
    }

    #[test]
    fn test_multiple_content_blocks() {
        let msg = Message::Assistant {
            content: vec![
                ContentBlock::Text {
                    text: "Let me read that file.".to_string(),
                },
                ContentBlock::ToolUse {
                    block: ToolUseBlock {
                        id: "tool_789".to_string(),
                        name: "file_read".to_string(),
                        input: json!({"path": "src/main.rs"}),
                    },
                },
            ],
            usage: Some(Usage {
                input_tokens: 200,
                output_tokens: 75,
                cache_read_tokens: None,
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }
}
