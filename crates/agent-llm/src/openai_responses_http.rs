//! OpenAI Responses API (`/v1/responses`) HTTP model implementation.
//!
//! Chat Completions rejects function tools combined with `reasoning_effort`
//! on newer reasoning models; the Responses API supports both. Requests are
//! stateless (`store: false`): the full conversation is re-sent every turn,
//! the same as the Chat Completions client.
//!
//! Reasoning items are not carried across turns (the canonical `Message` has
//! no slot for them). Function call items are sent without their server `id`
//! so the API does not require the matching reasoning item.

use std::collections::HashMap;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use agent_core::error::ModelError;
use agent_core::message::{ContentBlock, Message, Usage};
use agent_core::model::{Model, ModelRequest, ModelResponse, ModelStream};
use agent_core::stream::{StopReason, StreamChunk};

/// An OpenAI model served through the Responses API.
#[derive(Debug, Clone)]
pub struct OpenAIResponsesHttpModel {
    model_name: String,
    api_key: String,
    /// Base URL without trailing slash, e.g. "https://api.openai.com/v1".
    base_url: String,
    /// `reasoning.effort` ("none", "minimal", "low", "medium", "high", ...).
    reasoning_effort: Option<String>,
    /// `reasoning.summary` ("auto", "concise", "detailed"). Summaries stream
    /// as `ThinkingDelta`.
    reasoning_summary: Option<String>,
    client: Client,
}

impl OpenAIResponsesHttpModel {
    pub fn new(model_name: String, api_key: String, base_url: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_default();

        Self {
            model_name,
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            reasoning_effort: None,
            reasoning_summary: None,
            client,
        }
    }

    pub fn with_reasoning(mut self, effort: Option<String>, summary: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self.reasoning_summary = summary;
        self
    }

    /// Build the JSON request body for `/responses`.
    fn build_request_body(&self, request: &ModelRequest, stream: bool) -> Value {
        let mut body = json!({
            "model": self.model_name,
            "input": to_input_items(&request.messages),
            "stream": stream,
            "store": false,
            "max_output_tokens": request.max_tokens.unwrap_or(4096),
        });

        if !request.system.is_empty() {
            body["instructions"] = json!(request.system);
        }

        let mut reasoning = serde_json::Map::new();
        if let Some(effort) = &self.reasoning_effort {
            reasoning.insert("effort".into(), json!(effort));
        }
        if let Some(summary) = &self.reasoning_summary {
            reasoning.insert("summary".into(), json!(summary));
        }
        if !reasoning.is_empty() {
            body["reasoning"] = Value::Object(reasoning);
        }

        // Reasoning models reject `temperature` unless reasoning is off.
        if let Some(temp) = request.temperature {
            if self.reasoning_effort.as_deref() == Some("none") {
                body["temperature"] = json!(temp);
            }
        }

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect();
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }

        body
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response, ModelError> {
        let url = format!("{}/responses", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| ModelError::Connection(format!("Request failed: {}", e)))?;

        let status = response.status().as_u16();
        if status == 200 {
            return Ok(response);
        }
        if status == 429 {
            return Err(ModelError::RateLimited {
                retry_after_ms: 5000,
            });
        }
        let error_body = response
            .text()
            .await
            .unwrap_or_else(|_| "Could not read error body".to_string());
        Err(ModelError::Api {
            status,
            body: error_body,
        })
    }
}

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

/// Convert canonical messages to Responses API input items.
fn to_input_items(messages: &[Message]) -> Vec<Value> {
    let mut items = Vec::new();
    for msg in messages {
        match msg {
            Message::System { content } => {
                items.push(json!({ "role": "system", "content": content }));
            }
            Message::User { content } => {
                let parts: Vec<Value> = content.iter().map(user_part).collect();
                items.push(json!({ "role": "user", "content": parts }));
            }
            Message::Assistant { content, .. } => {
                let mut text = String::new();
                let mut calls = Vec::new();
                for block in content {
                    match block {
                        ContentBlock::Text { text: t } => text.push_str(t),
                        ContentBlock::ToolUse { block } => calls.push(json!({
                            "type": "function_call",
                            "call_id": block.id,
                            "name": block.name,
                            "arguments": block.input.to_string(),
                        })),
                        ContentBlock::Image { .. } => {}
                    }
                }
                if !text.is_empty() {
                    items.push(json!({ "role": "assistant", "content": text }));
                }
                items.extend(calls);
            }
            Message::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                }));
            }
        }
    }
    items
}

fn user_part(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({ "type": "input_text", "text": text }),
        ContentBlock::Image {
            media_type, data, ..
        } => json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", media_type, data),
        }),
        ContentBlock::ToolUse { block } => json!({
            "type": "input_text",
            "text": format!("[tool_use: {}({})]", block.name, block.input),
        }),
    }
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

fn parse_usage(response: &Value) -> Usage {
    let usage = response.get("usage");
    let get = |key: &str| {
        usage
            .and_then(|u| u.get(key))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    };
    Usage {
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_tokens: usage
            .and_then(|u| u.pointer("/input_tokens_details/cached_tokens"))
            .and_then(|v| v.as_u64()),
    }
}

/// Stop reason from a final response object. Tool calls take precedence.
fn stop_reason(response: &Value, saw_tool_call: bool) -> StopReason {
    if saw_tool_call {
        return StopReason::ToolUse;
    }
    match response
        .pointer("/incomplete_details/reason")
        .and_then(|v| v.as_str())
    {
        Some("max_output_tokens") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::ContentFilter,
        _ => StopReason::EndTurn,
    }
}

fn parse_arguments(args: Option<&Value>) -> Value {
    args.and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(json!({}))
}

/// Error carried by a `response.failed` / `error` event.
fn failure_error(err: Option<&Value>) -> ModelError {
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("response failed");
    let code = err
        .and_then(|e| e.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if code == "rate_limit_exceeded" {
        return ModelError::RateLimited {
            retry_after_ms: 5000,
        };
    }
    ModelError::Api {
        status: 0,
        body: format!("{} {}", code, message).trim().to_string(),
    }
}

/// Parse a non-streaming `/responses` body into a `ModelResponse`.
fn parse_response(body: &Value) -> Result<ModelResponse, ModelError> {
    if body.get("status").and_then(|v| v.as_str()) == Some("failed") {
        return Err(failure_error(body.get("error")));
    }
    let output = body
        .get("output")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ModelError::Api {
            status: 0,
            body: "No output in response".to_string(),
        })?;

    let mut content = Vec::new();
    let mut saw_tool_call = false;
    for item in output {
        match item.get("type").and_then(|v| v.as_str()) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    let text = match part.get("type").and_then(|v| v.as_str()) {
                        Some("output_text") => part.get("text"),
                        Some("refusal") => part.get("refusal"),
                        _ => None,
                    };
                    if let Some(text) = text.and_then(|v| v.as_str()) {
                        content.push(agent_core::model::ContentBlock::Text {
                            text: text.to_string(),
                        });
                    }
                }
            }
            Some("function_call") => {
                saw_tool_call = true;
                content.push(agent_core::model::ContentBlock::ToolUse {
                    id: item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    name: item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    input: parse_arguments(item.get("arguments")),
                });
            }
            _ => {}
        }
    }

    Ok(ModelResponse {
        content,
        usage: parse_usage(body),
        stop_reason: stop_reason(body, saw_tool_call),
    })
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// Translates Responses API SSE events into `StreamChunk`s.
#[derive(Default)]
struct ResponsesStreamParser {
    /// Undecoded bytes of an incomplete SSE line.
    buffer: Vec<u8>,
    /// Output item id (`fc_...`) → call_id (`call_...`), for argument deltas.
    call_ids: HashMap<String, String>,
    saw_tool_call: bool,
    /// Set once a terminal event (completed/incomplete/failed/error) is seen.
    finished: bool,
}

impl ResponsesStreamParser {
    /// Feed raw bytes; returns the chunks (or a terminal error) they produce.
    fn process_bytes(&mut self, bytes: &[u8]) -> Vec<Result<StreamChunk, ModelError>> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            let Some(data) = line.strip_prefix("data:") else {
                continue; // `event:` lines, comments, blank separators
            };
            let Ok(event) = serde_json::from_str::<Value>(data.trim_start()) else {
                continue;
            };
            out.extend(self.handle_event(&event));
        }
        out
    }

    fn handle_event(&mut self, event: &Value) -> Vec<Result<StreamChunk, ModelError>> {
        if self.finished {
            return Vec::new();
        }
        let str_field = |key: &str| event.get(key).and_then(|v| v.as_str());
        match str_field("type").unwrap_or_default() {
            "response.output_text.delta" | "response.refusal.delta" => match str_field("delta") {
                Some(d) if !d.is_empty() => vec![Ok(StreamChunk::TextDelta {
                    text: d.to_string(),
                })],
                _ => Vec::new(),
            },
            "response.reasoning_summary_text.delta" => match str_field("delta") {
                Some(d) if !d.is_empty() => vec![Ok(StreamChunk::ThinkingDelta {
                    text: d.to_string(),
                })],
                _ => Vec::new(),
            },
            "response.output_item.added" => {
                let item = &event["item"];
                if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
                    return Vec::new();
                }
                let call_id = item["call_id"].as_str().unwrap_or_default().to_string();
                if let Some(item_id) = item.get("id").and_then(|v| v.as_str()) {
                    self.call_ids.insert(item_id.to_string(), call_id.clone());
                }
                self.saw_tool_call = true;
                vec![Ok(StreamChunk::ToolUseStart {
                    id: call_id,
                    name: item["name"].as_str().unwrap_or_default().to_string(),
                })]
            }
            "response.function_call_arguments.delta" => {
                let call_id = str_field("item_id").and_then(|id| self.call_ids.get(id));
                match (call_id, str_field("delta")) {
                    (Some(id), Some(d)) => vec![Ok(StreamChunk::ToolUseInputDelta {
                        id: id.clone(),
                        delta: d.to_string(),
                    })],
                    _ => Vec::new(),
                }
            }
            "response.output_item.done" => {
                let item = &event["item"];
                if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
                    return Vec::new();
                }
                vec![Ok(StreamChunk::ToolUseEnd {
                    id: item["call_id"].as_str().unwrap_or_default().to_string(),
                    input: parse_arguments(item.get("arguments")),
                })]
            }
            "response.completed" | "response.incomplete" => {
                self.finished = true;
                let response = &event["response"];
                vec![Ok(StreamChunk::MessageStop {
                    stop_reason: stop_reason(response, self.saw_tool_call),
                    usage: parse_usage(response),
                })]
            }
            "response.failed" => {
                self.finished = true;
                vec![Err(failure_error(event.pointer("/response/error")))]
            }
            "error" => {
                self.finished = true;
                vec![Err(failure_error(Some(event)))]
            }
            _ => Vec::new(),
        }
    }
}

#[async_trait]
impl Model for OpenAIResponsesHttpModel {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
        let body = self.build_request_body(&request, true);
        let response = self.post(&body).await?;

        let (tx, rx) = mpsc::channel::<Result<StreamChunk, ModelError>>(64);
        tokio::spawn(async move {
            let mut parser = ResponsesStreamParser::default();
            let mut bytes_stream = response.bytes_stream();
            while let Some(chunk) = bytes_stream.next().await {
                let bytes = match chunk {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx
                            .send(Err(ModelError::StreamInterrupted(e.to_string())))
                            .await;
                        return;
                    }
                };
                for item in parser.process_bytes(&bytes) {
                    if tx.send(item).await.is_err() {
                        return; // receiver dropped
                    }
                }
                if parser.finished {
                    return;
                }
            }
            let _ = tx
                .send(Err(ModelError::StreamInterrupted(
                    "stream ended before response.completed".to_string(),
                )))
                .await;
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let body = self.build_request_body(&request, false);
        let response_body: Value = self
            .post(&body)
            .await?
            .json()
            .await
            .map_err(|e| ModelError::Connection(format!("Failed to parse JSON: {}", e)))?;
        parse_response(&response_body)
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::message::ToolUseBlock;
    use agent_core::model::ToolDefinition;

    fn model() -> OpenAIResponsesHttpModel {
        OpenAIResponsesHttpModel::new(
            "gpt-test".into(),
            "sk".into(),
            "https://api.example.com/v1/".into(),
        )
    }

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            system: "be helpful".into(),
            messages,
            tools: vec![ToolDefinition {
                name: "shell".into(),
                description: "run".into(),
                parameters: json!({"type": "object"}),
            }],
            max_tokens: Some(1000),
            temperature: Some(0.0),
            output_schema: None,
        }
    }

    #[test]
    fn body_has_instructions_flat_tools_and_reasoning() {
        let m = model().with_reasoning(Some("high".into()), Some("auto".into()));
        let body = m.build_request_body(&request(vec![]), true);
        assert_eq!(body["instructions"], "be helpful");
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 1000);
        assert_eq!(
            body["reasoning"],
            json!({"effort": "high", "summary": "auto"})
        );
        assert_eq!(body["tools"][0]["name"], "shell");
        assert_eq!(body["tools"][0]["type"], "function");
        assert!(body["tools"][0].get("function").is_none());
        assert!(body.get("temperature").is_none());
        assert_eq!(m.base_url, "https://api.example.com/v1");
    }

    #[test]
    fn temperature_only_sent_when_reasoning_disabled() {
        let m = model().with_reasoning(Some("none".into()), None);
        let body = m.build_request_body(&request(vec![]), false);
        assert_eq!(body["temperature"], 0.0);
        assert!(model()
            .build_request_body(&request(vec![]), false)
            .get("reasoning")
            .is_none());
    }

    #[test]
    fn input_items_cover_tool_round_trip() {
        let items = to_input_items(&[
            Message::User {
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
            Message::Assistant {
                content: vec![
                    ContentBlock::Text {
                        text: "running".into(),
                    },
                    ContentBlock::ToolUse {
                        block: ToolUseBlock {
                            id: "call_1".into(),
                            name: "shell".into(),
                            input: json!({"cmd": "ls"}),
                        },
                    },
                ],
                usage: None,
            },
            Message::ToolResult {
                tool_use_id: "call_1".into(),
                content: "a.txt".into(),
                is_error: false,
            },
        ]);
        assert_eq!(
            items,
            vec![
                json!({"role": "user", "content": [{"type": "input_text", "text": "hi"}]}),
                json!({"role": "assistant", "content": "running"}),
                json!({"type": "function_call", "call_id": "call_1", "name": "shell",
                       "arguments": "{\"cmd\":\"ls\"}"}),
                json!({"type": "function_call_output", "call_id": "call_1", "output": "a.txt"}),
            ]
        );
    }

    #[test]
    fn parse_non_streaming_response() {
        let body = json!({
            "status": "completed",
            "output": [
                {"type": "reasoning", "summary": []},
                {"type": "message", "role": "assistant",
                 "content": [{"type": "output_text", "text": "listing"}]},
                {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                 "name": "shell", "arguments": "{\"cmd\":\"ls\"}"}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5,
                      "input_tokens_details": {"cached_tokens": 4}}
        });
        let resp = parse_response(&body).unwrap();
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.cache_read_tokens, Some(4));
        assert_eq!(resp.content.len(), 2);
        match &resp.content[1] {
            agent_core::model::ContentBlock::ToolUse { id, input, .. } => {
                assert_eq!(id, "call_1");
                assert_eq!(input, &json!({"cmd": "ls"}));
            }
            other => panic!("expected tool use, got {:?}", other),
        }
    }

    #[test]
    fn parse_incomplete_max_tokens() {
        let body = json!({
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": []
        });
        assert_eq!(
            parse_response(&body).unwrap().stop_reason,
            StopReason::MaxTokens
        );
    }

    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
            .collect()
    }

    #[test]
    fn stream_text_reasoning_and_tool_call() {
        let raw = sse(&[
            json!({"type": "response.created", "response": {}}),
            json!({"type": "response.reasoning_summary_text.delta", "delta": "thinking"}),
            json!({"type": "response.output_text.delta", "delta": "Hel"}),
            json!({"type": "response.output_text.delta", "delta": "lo"}),
            json!({"type": "response.output_item.added", "output_index": 1,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                            "name": "shell", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "delta": "{\"cmd\":"}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1",
                   "delta": "\"ls\"}"}),
            json!({"type": "response.output_item.done", "output_index": 1,
                   "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                            "name": "shell", "arguments": "{\"cmd\":\"ls\"}"}}),
            json!({"type": "response.completed",
                   "response": {"status": "completed",
                                "usage": {"input_tokens": 7, "output_tokens": 3}}}),
        ]);

        // Split mid-line (and mid-UTF-8 is harmless: bytes are buffered).
        let mut parser = ResponsesStreamParser::default();
        let (a, b) = raw.as_bytes().split_at(raw.len() / 2);
        let mut chunks: Vec<StreamChunk> = parser
            .process_bytes(a)
            .into_iter()
            .chain(parser.process_bytes(b))
            .map(|r| r.unwrap())
            .collect();

        assert!(parser.finished);
        let stop = chunks.pop().unwrap();
        assert_eq!(
            chunks,
            vec![
                StreamChunk::ThinkingDelta {
                    text: "thinking".into()
                },
                StreamChunk::TextDelta { text: "Hel".into() },
                StreamChunk::TextDelta { text: "lo".into() },
                StreamChunk::ToolUseStart {
                    id: "call_1".into(),
                    name: "shell".into()
                },
                StreamChunk::ToolUseInputDelta {
                    id: "call_1".into(),
                    delta: "{\"cmd\":".into()
                },
                StreamChunk::ToolUseInputDelta {
                    id: "call_1".into(),
                    delta: "\"ls\"}".into()
                },
                StreamChunk::ToolUseEnd {
                    id: "call_1".into(),
                    input: json!({"cmd": "ls"})
                },
            ]
        );
        match stop {
            StreamChunk::MessageStop { stop_reason, usage } => {
                assert_eq!(stop_reason, StopReason::ToolUse);
                assert_eq!(usage.input_tokens, 7);
                assert_eq!(usage.output_tokens, 3);
            }
            other => panic!("expected MessageStop, got {:?}", other),
        }
    }

    #[test]
    fn stream_failed_event_yields_error() {
        let raw = sse(&[json!({"type": "response.failed",
            "response": {"error": {"code": "server_error", "message": "boom"}}})]);
        let mut parser = ResponsesStreamParser::default();
        let out = parser.process_bytes(raw.as_bytes());
        assert!(parser.finished);
        match &out[..] {
            [Err(ModelError::Api { body, .. })] => assert!(body.contains("boom")),
            other => panic!("unexpected: {:?}", other),
        }
    }
}
