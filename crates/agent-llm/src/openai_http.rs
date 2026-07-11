//! Real OpenAI-compatible HTTP model implementation.
//!
//! Makes actual HTTP calls to OpenAI-compatible endpoints (including
//! custom base URLs for proxy services like Trend Micro RDSEC).

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use agent_core::error::ModelError;
use agent_core::message::{ContentBlock, Usage};
use agent_core::model::{Model, ModelRequest, ModelResponse, ModelStream};
use agent_core::stream::{StopReason, StreamChunk};

use crate::convert::openai as openai_convert;

/// An OpenAI-compatible model that makes real HTTP calls.
#[derive(Debug, Clone)]
pub struct OpenAIHttpModel {
    /// The model name (e.g., "claude-4.6-sonnet-aws", "gpt-4o")
    model_name: String,
    /// API key for Authorization: Bearer header
    api_key: String,
    /// Base URL for the API (e.g., "https://api.openai.com/v1")
    base_url: String,
    /// HTTP client (reusable across requests)
    client: Client,
}

impl OpenAIHttpModel {
    /// Create a new OpenAI HTTP model.
    ///
    /// # Arguments
    /// * `model_name` - The model identifier to pass in requests
    /// * `api_key` - Bearer token for authentication
    /// * `base_url` - Base URL (without trailing slash), e.g. "https://api.openai.com/v1"
    pub fn new(model_name: String, api_key: String, base_url: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_default();

        Self {
            model_name,
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        }
    }

    /// Build the JSON request body for chat completions.
    fn build_request_body(&self, request: &ModelRequest, stream: bool) -> Value {
        let messages = self.build_messages(request);
        let mut body = json!({
            "model": self.model_name,
            "messages": messages,
            "stream": stream,
        });

        // Request usage stats in streaming mode
        if stream {
            body["stream_options"] = json!({"include_usage": true});
        }

        // Use max_completion_tokens (the current OpenAI standard).
        // The legacy "max_tokens" param is deprecated and rejected by newer models.
        let token_limit = request.max_tokens.unwrap_or(4096);
        body["max_completion_tokens"] = json!(token_limit);

        // Reasoning models (o-series) don't support temperature — skip it for those.
        if let Some(temp) = request.temperature {
            let name = self.model_name.to_lowercase();
            let is_reasoning =
                name.starts_with("o1") || name.starts_with("o3") || name.starts_with("o4");
            if !is_reasoning {
                body["temperature"] = json!(temp);
            }
        }

        // Add tools if present
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters
                        }
                    })
                })
                .collect();
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        body
    }

    /// Build the messages array including system prompt.
    fn build_messages(&self, request: &ModelRequest) -> Vec<Value> {
        let mut msgs = Vec::new();

        // System message
        if !request.system.is_empty() {
            msgs.push(json!({
                "role": "system",
                "content": request.system
            }));
        }

        // Convert canonical messages to OpenAI wire format
        let wire_msgs = openai_convert::to_wire(&request.messages);
        msgs.extend(wire_msgs);

        msgs
    }

    /// Parse a non-streaming response into ModelResponse.
    fn parse_response(&self, body: &Value) -> Result<ModelResponse, ModelError> {
        let choice = body
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| ModelError::Api {
                status: 0,
                body: "No choices in response".to_string(),
            })?;

        let message = choice.get("message").ok_or_else(|| ModelError::Api {
            status: 0,
            body: "No message in choice".to_string(),
        })?;

        let finish_reason = choice
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("stop");

        let stop_reason = match finish_reason {
            "stop" => StopReason::EndTurn,
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            "content_filter" => StopReason::ContentFilter,
            _ => StopReason::EndTurn,
        };

        // Parse content blocks using the converter
        let content_blocks = openai_convert::from_wire(message).map_err(|e| ModelError::Api {
            status: 0,
            body: format!("Failed to parse response: {}", e),
        })?;

        // Convert to model::ContentBlock format
        let content = content_blocks
            .into_iter()
            .map(|b| match b {
                ContentBlock::Text { text } => agent_core::model::ContentBlock::Text { text },
                ContentBlock::ToolUse { block } => agent_core::model::ContentBlock::ToolUse {
                    id: block.id,
                    name: block.name,
                    input: block.input,
                },
                _ => agent_core::model::ContentBlock::Text {
                    text: String::new(),
                },
            })
            .collect();

        // Parse usage
        let usage = self.parse_usage(body);

        Ok(ModelResponse {
            content,
            usage,
            stop_reason,
        })
    }

    /// Parse usage from response body.
    fn parse_usage(&self, body: &Value) -> Usage {
        if let Some(usage) = body.get("usage") {
            Usage {
                input_tokens: usage
                    .get("prompt_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                output_tokens: usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                cache_read_tokens: None,
            }
        } else {
            Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: None,
            }
        }
    }
}

#[async_trait]
impl Model for OpenAIHttpModel {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
        let body = self.build_request_body(&request, true);
        let url = format!("{}/chat/completions", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Connection(format!("Request failed: {}", e)))?;

        let status = response.status().as_u16();
        if status != 200 {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Could not read error body".to_string());

            if status == 429 {
                return Err(ModelError::RateLimited {
                    retry_after_ms: 5000,
                });
            }
            return Err(ModelError::Api {
                status,
                body: error_body,
            });
        }

        // Stream SSE response
        let bytes_stream = response.bytes_stream();
        let (tx, rx) = mpsc::channel::<Result<StreamChunk, ModelError>>(64);

        // Spawn a task to process the SSE stream
        tokio::spawn(async move {
            use futures::TryStreamExt;
            let mut buffer = String::new();
            let mut current_tool_id: Option<String> = None;
            let mut _current_tool_name: Option<String> = None;
            let mut current_tool_args = String::new();
            let mut total_input_tokens: u64 = 0;
            let mut total_output_tokens: u64 = 0;
            let mut final_stop_reason = StopReason::EndTurn;

            let mut stream = bytes_stream;
            while let Ok(Some(chunk)) = stream.try_next().await {
                let text = String::from_utf8_lossy(&chunk);
                buffer.push_str(&text);

                // Process complete SSE lines
                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim_end_matches('\r').to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }

                    if let Some(data) = line.strip_prefix("data: ") {
                        if data.trim() == "[DONE]" {
                            // Flush any pending tool use
                            if let Some(tool_id) = current_tool_id.take() {
                                let input: Value =
                                    serde_json::from_str(&current_tool_args).unwrap_or(json!({}));
                                let _ = tx
                                    .send(Ok(StreamChunk::ToolUseEnd { id: tool_id, input }))
                                    .await;
                                current_tool_args.clear();
                                _current_tool_name = None;
                            }
                            // Send final MessageStop
                            let _ = tx
                                .send(Ok(StreamChunk::MessageStop {
                                    stop_reason: final_stop_reason,
                                    usage: Usage {
                                        input_tokens: total_input_tokens,
                                        output_tokens: total_output_tokens,
                                        cache_read_tokens: None,
                                    },
                                }))
                                .await;
                            return;
                        }

                        let parsed: Value = match serde_json::from_str(data) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        // Extract usage if present
                        if let Some(usage) = parsed.get("usage") {
                            total_input_tokens = usage
                                .get("prompt_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(total_input_tokens);
                            total_output_tokens = usage
                                .get("completion_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(total_output_tokens);
                        }

                        let choice = match parsed.get("choices").and_then(|c| c.get(0)) {
                            Some(c) => c,
                            None => continue,
                        };

                        // Check finish_reason
                        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                            final_stop_reason = match fr {
                                "stop" => StopReason::EndTurn,
                                "tool_calls" => StopReason::ToolUse,
                                "length" => StopReason::MaxTokens,
                                "content_filter" => StopReason::ContentFilter,
                                _ => StopReason::EndTurn,
                            };
                        }

                        let delta = match choice.get("delta") {
                            Some(d) => d,
                            None => continue,
                        };

                        // Handle text content delta
                        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                            if !content.is_empty() {
                                let _ = tx
                                    .send(Ok(StreamChunk::TextDelta {
                                        text: content.to_string(),
                                    }))
                                    .await;
                            }
                        }

                        // Handle tool_calls delta
                        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array())
                        {
                            for tc in tool_calls {
                                let tc_id = tc.get("id").and_then(|v| v.as_str());
                                let function = tc.get("function");

                                // If we get a new tool call id, it's a new tool
                                if let Some(id) = tc_id {
                                    // Flush previous tool if any
                                    if let Some(prev_id) = current_tool_id.take() {
                                        let input: Value = serde_json::from_str(&current_tool_args)
                                            .unwrap_or(json!({}));
                                        let _ = tx
                                            .send(Ok(StreamChunk::ToolUseEnd {
                                                id: prev_id,
                                                input,
                                            }))
                                            .await;
                                        current_tool_args.clear();
                                    }

                                    let name = function
                                        .and_then(|f| f.get("name"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();

                                    let _ = tx
                                        .send(Ok(StreamChunk::ToolUseStart {
                                            id: id.to_string(),
                                            name: name.clone(),
                                        }))
                                        .await;

                                    current_tool_id = Some(id.to_string());
                                    _current_tool_name = Some(name);
                                }

                                // Accumulate arguments delta
                                if let Some(args_delta) = function
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|v| v.as_str())
                                {
                                    current_tool_args.push_str(args_delta);
                                    if let Some(id) = &current_tool_id {
                                        let _ = tx
                                            .send(Ok(StreamChunk::ToolUseInputDelta {
                                                id: id.clone(),
                                                delta: args_delta.to_string(),
                                            }))
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // If stream ended without [DONE], send MessageStop anyway
            if let Some(tool_id) = current_tool_id.take() {
                let input: Value = serde_json::from_str(&current_tool_args).unwrap_or(json!({}));
                let _ = tx
                    .send(Ok(StreamChunk::ToolUseEnd { id: tool_id, input }))
                    .await;
            }
            let _ = tx
                .send(Ok(StreamChunk::MessageStop {
                    stop_reason: final_stop_reason,
                    usage: Usage {
                        input_tokens: total_input_tokens,
                        output_tokens: total_output_tokens,
                        cache_read_tokens: None,
                    },
                }))
                .await;
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let body = self.build_request_body(&request, false);
        let url = format!("{}/chat/completions", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Connection(format!("Request failed: {}", e)))?;

        let status = response.status().as_u16();
        if status != 200 {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Could not read error body".to_string());

            if status == 429 {
                return Err(ModelError::RateLimited {
                    retry_after_ms: 5000,
                });
            }
            return Err(ModelError::Api {
                status,
                body: error_body,
            });
        }

        let response_body: Value = response
            .json()
            .await
            .map_err(|e| ModelError::Connection(format!("Failed to parse JSON: {}", e)))?;

        self.parse_response(&response_body)
    }

    fn name(&self) -> &str {
        &self.model_name
    }

    fn provider(&self) -> &str {
        "openai"
    }

    fn context_window(&self) -> usize {
        128_000
    }

    fn max_output_tokens(&self) -> usize {
        16_384
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn input_cost_per_million(&self) -> f64 {
        5.0
    }

    fn output_cost_per_million(&self) -> f64 {
        15.0
    }
}
