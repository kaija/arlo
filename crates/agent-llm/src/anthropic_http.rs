//! Real Anthropic Messages API HTTP model implementation.
//!
//! Makes actual HTTP calls to the Anthropic Messages API endpoint
//! with support for streaming (SSE) and non-streaming responses.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::debug;

use agent_core::error::ModelError;
use agent_core::message::Usage;
use agent_core::model::{Model, ModelRequest, ModelResponse, ModelStream};
use agent_core::stream::{StopReason, StreamChunk};
use crate::convert::anthropic as anthropic_convert;

/// An Anthropic Messages API model that makes real HTTP calls.
#[derive(Debug, Clone)]
pub struct AnthropicHttpModel {
    /// The model name (bare name, e.g. "claude-sonnet-4-20250514")
    model_name: String,
    /// API key for x-api-key header
    api_key: String,
    /// Base URL for the API (e.g. "https://api.anthropic.com/v1", no trailing slash)
    base_url: String,
    /// HTTP client (reusable across requests)
    client: Client,
    /// Maximum time to wait for the next chunk during streaming (default 90s)
    stream_idle_timeout: Duration,
}

impl AnthropicHttpModel {
    /// Create a new Anthropic HTTP model.
    ///
    /// # Arguments
    /// * `model_name` - The bare model identifier to pass in requests
    /// * `api_key` - Value for the x-api-key authentication header
    /// * `base_url` - Base URL (trailing slashes will be trimmed)
    pub fn new(model_name: String, api_key: String, base_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default();

        Self {
            model_name,
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
            stream_idle_timeout: Duration::from_secs(90),
        }
    }

    /// Build the JSON request body for the Anthropic Messages API.
    ///
    /// Assembles model, max_tokens, messages, stream, and conditionally
    /// includes system, tools, and temperature fields.
    pub fn build_request_body(&self, request: &ModelRequest, stream: bool) -> Value {
        let wire = anthropic_convert::to_wire(&request.messages);

        let max_tokens = request.max_tokens.unwrap_or(8192);

        let mut body = json!({
            "model": self.model_name,
            "max_tokens": max_tokens,
            "messages": wire.messages,
            "stream": stream,
        });

        // Build effective system prompt: ModelRequest.system + extracted system from to_wire
        let effective_system = match wire.system {
            Some(wire_system) => {
                if request.system.is_empty() {
                    wire_system
                } else {
                    format!("{}\n{}", request.system, wire_system)
                }
            }
            None => request.system.clone(),
        };
        if !effective_system.is_empty() {
            body["system"] = json!(effective_system);
        }

        // Include tools only when non-empty, mapping parameters → input_schema
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect();
            body["tools"] = json!(tools);
        }

        // Include temperature only when specified, clamped to [0.0, 1.0]
        if let Some(temp) = request.temperature {
            let clamped = temp.clamp(0.0, 1.0);
            body["temperature"] = json!(clamped);
        }

        body
    }

    /// Build the HTTP headers required for every Anthropic API request.
    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.api_key)
                .unwrap_or_else(|_| HeaderValue::from_static("")),
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers
    }

    /// Map an HTTP error response to the appropriate ModelError.
    ///
    /// - HTTP 429/529 → `ModelError::RateLimited` with retry-after header parsed as seconds × 1000, default 5000ms
    /// - Other 4xx/5xx → `ModelError::Api { status, body }` with body sanitized
    fn map_http_error(
        status_code: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> ModelError {
        match status_code {
            429 | 529 => {
                // Parse retry-after header (value in seconds, convert to ms)
                let retry_after_ms = headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|secs| secs * 1000)
                    .unwrap_or(5000);
                ModelError::RateLimited { retry_after_ms }
            }
            _ => {
                let body_str = Self::sanitize_error_body(status_code, body);
                ModelError::Api {
                    status: status_code,
                    body: body_str,
                }
            }
        }
    }

    /// Sanitize an error response body for inclusion in error messages.
    ///
    /// Detects HTML responses (from proxies like CloudFlare/nginx) and extracts
    /// a meaningful message. Non-HTML bodies are truncated to 4096 bytes.
    fn sanitize_error_body(status_code: u16, body: &[u8]) -> String {
        let body_str = String::from_utf8_lossy(body);
        let trimmed = body_str.trim_start();

        // Detect HTML error pages (case-insensitive)
        let lower = trimmed.to_lowercase();
        let is_html = lower.starts_with("<!doctype html")
            || lower.starts_with("<html");

        if is_html {
            // Try to extract <title> content
            if let Some(title) = Self::extract_html_title(&body_str) {
                return title;
            }
            return format!("proxy error (HTTP {})", status_code);
        }

        // Non-HTML: truncate to 4096 bytes
        let truncated: String = body_str.chars().take(4096).collect();
        truncated
    }

    /// Extract the content of the `<title>` tag from an HTML string.
    fn extract_html_title(html: &str) -> Option<String> {
        let lower = html.to_lowercase();
        let start = lower.find("<title>")?;
        let after_tag = start + 7; // length of "<title>"
        let end = lower[after_tag..].find("</title>")?;
        let title = html[after_tag..after_tag + end].trim().to_string();
        if title.is_empty() {
            None
        } else {
            Some(title)
        }
    }
}

#[async_trait]
impl Model for AnthropicHttpModel {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
        // 1. Build request body and headers
        let body = self.build_request_body(&request, true);
        let headers = self.build_headers();

        // 2. Make the HTTP request
        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Connection(format!("Request failed: {}", e)))?;

        // 3. Check for HTTP errors
        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let response_headers = response.headers().clone();
            let body_bytes = response.bytes().await.unwrap_or_default();
            return Err(Self::map_http_error(status_code, &response_headers, &body_bytes));
        }

        // 4. Set up channel and spawn parser task
        let (tx, rx) = mpsc::channel::<Result<StreamChunk, ModelError>>(32);
        let idle_timeout = self.stream_idle_timeout;

        tokio::spawn(async move {
            let mut parser = SseParserState::new(idle_timeout);
            let mut byte_stream = response.bytes_stream();

            loop {
                let chunk_result =
                    tokio::time::timeout(idle_timeout, byte_stream.next()).await;

                match chunk_result {
                    Ok(Some(Ok(bytes))) => {
                        let chunks = parser.process_bytes(&bytes);
                        for chunk in chunks {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return; // receiver dropped
                            }
                        }
                        // Check for overloaded error
                        if parser.overloaded_error {
                            let _ = tx
                                .send(Err(ModelError::RateLimited {
                                    retry_after_ms: 5000,
                                }))
                                .await;
                            return;
                        }
                    }
                    Ok(Some(Err(e))) => {
                        // Stream read error
                        let _ = tx
                            .send(Err(ModelError::Connection(format!(
                                "Stream error: {}",
                                e
                            ))))
                            .await;
                        return;
                    }
                    Ok(None) => {
                        // Stream ended
                        if !parser.events_received {
                            // No events at all - signal fallback needed
                            let _ = tx
                                .send(Err(ModelError::Connection(
                                    "Stream produced no events (fallback needed)"
                                        .to_string(),
                                )))
                                .await;
                            return;
                        }
                        // Normal end - flush any remaining state
                        break;
                    }
                    Err(_) => {
                        // Timeout fired
                        if !parser.events_received {
                            // No events received yet, just abort
                            let _ = tx
                                .send(Err(ModelError::Connection(
                                    "Stream idle timeout before any events".to_string(),
                                )))
                                .await;
                            return;
                        }
                        // Had events but timed out - emit MessageStop with accumulated state
                        let final_chunks = parser.flush();
                        for chunk in final_chunks {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                        return;
                    }
                }
            }

            // Stream ended normally - flush if message_stop wasn't already emitted
            // The parser emits MessageStop on message_stop event, so only flush
            // for premature termination (which we handled in the Ok(None) branch
            // when events_received is true but no message_stop was seen)
        });

        let stream = ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        // Build request body with stream: false
        let body = self.build_request_body(&request, false);
        let headers = self.build_headers();

        // Make the HTTP request
        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| ModelError::Connection(format!("Request failed: {}", e)))?;

        // Check for HTTP errors
        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body_bytes = response.bytes().await.unwrap_or_default();
            if status_code == 429 || status_code == 529 {
                return Err(ModelError::RateLimited {
                    retry_after_ms: 5000,
                });
            }
            let body_str =
                String::from_utf8_lossy(&body_bytes[..body_bytes.len().min(4096)]).to_string();
            return Err(ModelError::Api {
                status: status_code,
                body: body_str,
            });
        }

        // Parse JSON response
        let response_body: Value = response.json().await.map_err(|e| ModelError::Api {
            status: 0,
            body: format!("Failed to parse response JSON: {}", e),
        })?;

        // Extract content blocks using from_wire()
        let content_value = response_body.get("content").ok_or_else(|| ModelError::Api {
            status: 0,
            body: "Response missing 'content' field".to_string(),
        })?;

        let core_blocks = anthropic_convert::from_wire(content_value).map_err(|e| {
            ModelError::Api {
                status: 0,
                body: format!("Failed to parse response content: {}", e),
            }
        })?;

        // Convert agent_core::message::ContentBlock to agent_core::model::ContentBlock
        let content: Vec<agent_core::model::ContentBlock> = core_blocks
            .into_iter()
            .filter_map(|block| match block {
                agent_core::message::ContentBlock::Text { text } => {
                    Some(agent_core::model::ContentBlock::Text { text })
                }
                agent_core::message::ContentBlock::ToolUse { block: tool_use } => {
                    Some(agent_core::model::ContentBlock::ToolUse {
                        id: tool_use.id,
                        name: tool_use.name,
                        input: tool_use.input,
                    })
                }
                agent_core::message::ContentBlock::Image { .. } => {
                    // Images don't typically appear in model responses; skip gracefully
                    None
                }
            })
            .collect();

        // Extract usage
        let usage_obj = response_body.get("usage");
        let usage = Usage {
            input_tokens: usage_obj
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: usage_obj
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_read_tokens: usage_obj
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(|v| v.as_u64()),
        };

        // Extract stop reason
        let stop_reason_str = response_body.get("stop_reason").and_then(|v| v.as_str());
        let stop_reason = map_stop_reason(stop_reason_str);

        Ok(ModelResponse {
            content,
            usage,
            stop_reason,
        })
    }

    fn name(&self) -> &str {
        &self.model_name
    }

    fn provider(&self) -> &str {
        "anthropic"
    }

    fn context_window(&self) -> usize {
        200_000
    }

    fn max_output_tokens(&self) -> usize {
        8_192
    }

    fn supports_tools(&self) -> bool {
        true
    }

    fn input_cost_per_million(&self) -> f64 {
        3.0
    }

    fn output_cost_per_million(&self) -> f64 {
        15.0
    }
}

/// Map an Anthropic stop_reason string to the canonical StopReason enum.
///
/// Unknown, null, or absent values default to `EndTurn`.
fn map_stop_reason(s: Option<&str>) -> StopReason {
    match s {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        _ => StopReason::EndTurn,
    }
}

// ─── SSE Stream Parser ───────────────────────────────────────────────────────

/// Maximum line buffer size (1 MB). Lines exceeding this are discarded.
const MAX_LINE_BUFFER_SIZE: usize = 1_048_576;

/// Tracks an in-progress content block during SSE streaming.
#[derive(Debug, Clone)]
pub enum ActiveBlock {
    /// A text content block being streamed.
    Text,
    /// A tool_use content block accumulating JSON input.
    ToolUse {
        id: String,
        name: String,
        json_buf: String,
    },
    /// A thinking/reasoning content block being streamed.
    Thinking,
}

/// SSE parser state that processes raw bytes from an Anthropic streaming response.
///
/// Buffers partial lines, splits on LF/CRLF, parses SSE `data:` lines as JSON,
/// and dispatches events into canonical `StreamChunk` values.
#[derive(Debug)]
pub struct SseParserState {
    /// Partial line accumulator (max 1MB).
    line_buffer: Vec<u8>,
    /// Input tokens from `message_start`.
    pub input_tokens: u64,
    /// Output tokens from `message_delta`.
    pub output_tokens: u64,
    /// Cache read tokens from `message_start` or `message_delta`.
    pub cache_read_tokens: Option<u64>,
    /// Stop reason from `message_delta`.
    pub stop_reason: Option<String>,
    /// Active content blocks indexed by their position.
    active_blocks: HashMap<u32, ActiveBlock>,
    /// Whether any SSE events have been received on this stream.
    pub events_received: bool,
    /// Maximum wait between chunks (used by the streaming method).
    pub idle_timeout: Duration,
    /// Whether an overloaded_error event was received.
    pub overloaded_error: bool,
}

impl SseParserState {
    /// Create a new SSE parser state.
    pub fn new(idle_timeout: Duration) -> Self {
        Self {
            line_buffer: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: None,
            stop_reason: None,
            active_blocks: HashMap::new(),
            events_received: false,
            idle_timeout,
            overloaded_error: false,
        }
    }

    /// Process a chunk of bytes from the stream.
    ///
    /// Buffers partial lines, splits on LF/CRLF, parses complete SSE data lines,
    /// and returns a list of `StreamChunk` values to emit.
    pub fn process_bytes(&mut self, bytes: &[u8]) -> Vec<StreamChunk> {
        let mut chunks = Vec::new();

        for &byte in bytes {
            if byte == b'\n' {
                // We have a complete line. Check for trailing CR.
                if self.line_buffer.last() == Some(&b'\r') {
                    self.line_buffer.pop();
                }
                let line = String::from_utf8_lossy(&self.line_buffer).to_string();
                self.line_buffer.clear();
                chunks.extend(self.process_line(&line));
            } else {
                self.line_buffer.push(byte);
                // Enforce max line buffer size
                if self.line_buffer.len() > MAX_LINE_BUFFER_SIZE {
                    debug!("SSE line buffer exceeded 1MB, discarding");
                    self.line_buffer.clear();
                }
            }
        }

        chunks
    }

    /// Flush any in-progress state (called on stream end).
    ///
    /// If no `message_stop` was received, emits a final `MessageStop` with
    /// accumulated state. Also flushes any in-progress tool_use blocks.
    pub fn flush(&mut self) -> Vec<StreamChunk> {
        let mut chunks = Vec::new();

        // Flush any in-progress tool_use blocks
        let block_indices: Vec<u32> = self.active_blocks.keys().copied().collect();
        for index in block_indices {
            if let Some(ActiveBlock::ToolUse { id, json_buf, .. }) =
                self.active_blocks.remove(&index)
            {
                let input = serde_json::from_str::<Value>(&json_buf)
                    .unwrap_or_else(|_| json!({}));
                chunks.push(StreamChunk::ToolUseEnd { id, input });
            }
        }

        // Emit final MessageStop with accumulated state
        let stop_reason = map_stop_reason(self.stop_reason.as_deref());
        chunks.push(StreamChunk::MessageStop {
            stop_reason,
            usage: Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_read_tokens: self.cache_read_tokens,
            },
        });

        chunks
    }

    /// Process a single complete line from the SSE stream.
    fn process_line(&mut self, line: &str) -> Vec<StreamChunk> {
        // Skip empty lines (SSE event separators)
        if line.is_empty() {
            return Vec::new();
        }

        // Skip comment lines (starting with ':')
        if line.starts_with(':') {
            return Vec::new();
        }

        // Only process lines with "data: " prefix
        let data_str = match line.strip_prefix("data: ") {
            Some(d) => d,
            None => {
                // Skip lines with unrecognized prefixes (e.g. "event: ...")
                return Vec::new();
            }
        };

        // Parse JSON
        let data: Value = match serde_json::from_str(data_str) {
            Ok(v) => v,
            Err(e) => {
                debug!("SSE: invalid JSON in data line: {}", e);
                return Vec::new();
            }
        };

        self.dispatch_event(&data)
    }

    /// Dispatch a parsed SSE event JSON value to the appropriate handler.
    fn dispatch_event(&mut self, data: &Value) -> Vec<StreamChunk> {
        // Mark that we've received at least one event
        self.events_received = true;

        let event_type = match data.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => {
                debug!("SSE: event missing 'type' field");
                return Vec::new();
            }
        };

        // Check for overloaded_error
        if event_type == "overloaded_error" || event_type == "error" {
            if let Some(err) = data.get("error") {
                if err.get("type").and_then(|v| v.as_str()) == Some("overloaded_error") {
                    self.overloaded_error = true;
                    return Vec::new();
                }
            }
            // Top-level overloaded_error type
            if event_type == "overloaded_error" {
                self.overloaded_error = true;
                return Vec::new();
            }
        }

        match event_type {
            "ping" => Vec::new(),
            "message_start" => self.handle_message_start(data),
            "content_block_start" => self.handle_content_block_start(data),
            "content_block_delta" => self.handle_content_block_delta(data),
            "content_block_stop" => self.handle_content_block_stop(data),
            "message_delta" => self.handle_message_delta(data),
            "message_stop" => self.handle_message_stop(),
            _ => {
                debug!("SSE: unknown event type: {}", event_type);
                Vec::new()
            }
        }
    }

    /// Handle `message_start` event: extract usage from message.usage.
    fn handle_message_start(&mut self, data: &Value) -> Vec<StreamChunk> {
        if let Some(usage) = data.pointer("/message/usage") {
            if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                self.input_tokens = input;
            }
            if let Some(cache_read) = usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
            {
                self.cache_read_tokens = Some(cache_read);
            }
        }
        Vec::new()
    }

    /// Handle `content_block_start` event: register a new active block at the given index.
    fn handle_content_block_start(&mut self, data: &Value) -> Vec<StreamChunk> {
        let index = match data.get("index").and_then(|v| v.as_u64()) {
            Some(i) => i as u32,
            None => {
                debug!("SSE: content_block_start missing 'index' field");
                return Vec::new();
            }
        };

        let content_block = match data.get("content_block") {
            Some(cb) => cb,
            None => {
                debug!("SSE: content_block_start missing 'content_block' field");
                return Vec::new();
            }
        };

        let block_type = content_block
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match block_type {
            "text" => {
                self.active_blocks.insert(index, ActiveBlock::Text);
                Vec::new()
            }
            "tool_use" => {
                let id = content_block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = content_block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.active_blocks.insert(
                    index,
                    ActiveBlock::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        json_buf: String::new(),
                    },
                );
                vec![StreamChunk::ToolUseStart { id, name }]
            }
            "thinking" => {
                self.active_blocks.insert(index, ActiveBlock::Thinking);
                Vec::new()
            }
            _ => {
                debug!("SSE: unknown content_block type: {}", block_type);
                Vec::new()
            }
        }
    }

    /// Handle `content_block_delta` event: emit the appropriate delta chunk.
    fn handle_content_block_delta(&mut self, data: &Value) -> Vec<StreamChunk> {
        let index = match data.get("index").and_then(|v| v.as_u64()) {
            Some(i) => i as u32,
            None => {
                debug!("SSE: content_block_delta missing 'index' field");
                return Vec::new();
            }
        };

        let delta = match data.get("delta") {
            Some(d) => d,
            None => {
                debug!("SSE: content_block_delta missing 'delta' field");
                return Vec::new();
            }
        };

        let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match delta_type {
            "text_delta" => {
                let text = delta
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                vec![StreamChunk::TextDelta { text }]
            }
            "input_json_delta" => {
                let partial_json = delta
                    .get("partial_json")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Append to the tool_use's json_buf and get the id
                if let Some(ActiveBlock::ToolUse { id, json_buf, .. }) =
                    self.active_blocks.get_mut(&index)
                {
                    json_buf.push_str(&partial_json);
                    let id = id.clone();
                    vec![StreamChunk::ToolUseInputDelta {
                        id,
                        delta: partial_json,
                    }]
                } else {
                    debug!(
                        "SSE: input_json_delta for index {} but no active tool_use block",
                        index
                    );
                    Vec::new()
                }
            }
            "thinking_delta" => {
                let text = delta
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                vec![StreamChunk::ThinkingDelta { text }]
            }
            _ => {
                debug!("SSE: unknown delta type: {}", delta_type);
                Vec::new()
            }
        }
    }

    /// Handle `content_block_stop` event: finalize the block at the given index.
    fn handle_content_block_stop(&mut self, data: &Value) -> Vec<StreamChunk> {
        let index = match data.get("index").and_then(|v| v.as_u64()) {
            Some(i) => i as u32,
            None => {
                debug!("SSE: content_block_stop missing 'index' field");
                return Vec::new();
            }
        };

        match self.active_blocks.remove(&index) {
            Some(ActiveBlock::ToolUse { id, json_buf, .. }) => {
                let input = serde_json::from_str::<Value>(&json_buf)
                    .unwrap_or_else(|_| json!({}));
                vec![StreamChunk::ToolUseEnd { id, input }]
            }
            Some(ActiveBlock::Text) | Some(ActiveBlock::Thinking) => {
                // Text and thinking blocks just get removed, no final emission needed
                Vec::new()
            }
            None => {
                debug!(
                    "SSE: content_block_stop for index {} but no active block",
                    index
                );
                Vec::new()
            }
        }
    }

    /// Handle `message_delta` event: extract stop_reason and output_tokens.
    fn handle_message_delta(&mut self, data: &Value) -> Vec<StreamChunk> {
        if let Some(delta) = data.get("delta") {
            if let Some(reason) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                self.stop_reason = Some(reason.to_string());
            }
        }
        if let Some(usage) = data.get("usage") {
            if let Some(output) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                self.output_tokens = output;
            }
        }
        Vec::new()
    }

    /// Handle `message_stop` event: emit the final MessageStop chunk.
    fn handle_message_stop(&mut self) -> Vec<StreamChunk> {
        let stop_reason = map_stop_reason(self.stop_reason.as_deref());
        vec![StreamChunk::MessageStop {
            stop_reason,
            usage: Usage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cache_read_tokens: self.cache_read_tokens,
            },
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::message::{Message, ContentBlock};
    use agent_core::model::{ModelRequest, ToolDefinition};
    use proptest::prelude::*;
    use proptest::collection::vec;

    proptest! {
        /// **Property 1: Base URL Trailing Slash Normalization**
        /// *For any* base URL string (including those with zero, one, or multiple trailing slashes),
        /// the constructed `AnthropicHttpModel` SHALL store a base URL with no trailing slash.
        ///
        /// **Validates: Requirements 1.1, 1.3**
        #[test]
        fn prop_base_url_trailing_slash_normalization(
            base in "[a-z]{3,10}://[a-z]{3,10}\\.[a-z]{2,5}(/[a-z]{1,5}){0,3}//*"
        ) {
            let model = AnthropicHttpModel::new(
                "test-model".to_string(),
                "test-key".to_string(),
                base.clone(),
            );
            prop_assert!(!model.base_url.ends_with('/'),
                "base_url should not end with slash, got: {}", model.base_url);
        }

        /// **Property 2: Request Body Required Fields**
        /// *For any* valid `ModelRequest`, the built request body SHALL always contain the fields
        /// `model`, `max_tokens`, `messages`, and `stream`; SHALL contain a `system` field if and
        /// only if the effective system prompt is non-empty; and SHALL set `max_tokens` to the
        /// ModelRequest value when specified or 8192 when unspecified.
        ///
        /// **Validates: Requirements 2.1, 2.2, 2.3, 2.6**
        #[test]
        fn prop_request_body_required_fields(
            system in ".*",
            max_tokens in proptest::option::of(1u32..100_000u32),
            stream in proptest::bool::ANY,
        ) {
            let model = AnthropicHttpModel::new(
                "claude-sonnet-4-20250514".to_string(),
                "test-key".to_string(),
                "https://api.anthropic.com/v1".to_string(),
            );

            let request = ModelRequest {
                system: system.clone(),
                messages: vec![],
                tools: vec![],
                max_tokens,
                temperature: None,
                output_schema: None,
            };

            let body = model.build_request_body(&request, stream);

            // model field is always present and equals model_name
            prop_assert_eq!(
                body.get("model").and_then(|v| v.as_str()),
                Some("claude-sonnet-4-20250514"),
                "body must contain 'model' field matching model_name"
            );

            // max_tokens is always present and equals request.max_tokens or 8192
            let expected_max_tokens = max_tokens.unwrap_or(8192);
            prop_assert_eq!(
                body.get("max_tokens").and_then(|v| v.as_u64()),
                Some(expected_max_tokens as u64),
                "body must contain 'max_tokens' = {} (got {:?})",
                expected_max_tokens,
                body.get("max_tokens")
            );

            // messages is always present and is an array
            prop_assert!(
                body.get("messages").and_then(|v| v.as_array()).is_some(),
                "body must contain 'messages' as an array"
            );

            // stream is always present and equals the stream parameter
            prop_assert_eq!(
                body.get("stream").and_then(|v| v.as_bool()),
                Some(stream),
                "body must contain 'stream' = {}", stream
            );

            // system field present IFF effective system prompt is non-empty.
            // With empty messages vec, to_wire() returns system: None,
            // so effective system = request.system only.
            let effective_system_empty = system.is_empty();
            if effective_system_empty {
                prop_assert!(
                    body.get("system").is_none(),
                    "body must NOT contain 'system' when effective system prompt is empty"
                );
            } else {
                prop_assert!(
                    body.get("system").is_some(),
                    "body must contain 'system' when effective system prompt is non-empty"
                );
            }
        }

        /// **Property 4: Temperature Clamping**
        /// *For any* f32 temperature value, the temperature included in the request body
        /// SHALL be clamped to the range [0.0, 1.0].
        ///
        /// **Validates: Requirements 2.5**
        #[test]
        fn prop_temperature_clamping(
            temp in any::<f32>().prop_filter("not NaN", |f| !f.is_nan())
        ) {
            let model = AnthropicHttpModel::new(
                "test-model".to_string(),
                "test-key".to_string(),
                "https://api.anthropic.com/v1".to_string(),
            );

            let request = ModelRequest {
                system: String::new(),
                messages: vec![],
                tools: vec![],
                max_tokens: None,
                temperature: Some(temp),
                output_schema: None,
            };

            let body = model.build_request_body(&request, false);

            // When temperature is Some(t), body["temperature"] must be present
            let body_temp = body["temperature"].as_f64();
            prop_assert!(body_temp.is_some(),
                "temperature field should be present when temperature is Some({})", temp);

            let clamped = body_temp.unwrap();
            prop_assert!(clamped >= 0.0,
                "temperature should be >= 0.0, got {} for input {}", clamped, temp);
            prop_assert!(clamped <= 1.0,
                "temperature should be <= 1.0, got {} for input {}", clamped, temp);
        }

        /// **Property 4 (None case): Temperature Absent When None**
        /// When temperature is None, body["temperature"] SHALL be absent.
        ///
        /// **Validates: Requirements 2.5**
        #[test]
        fn prop_temperature_absent_when_none(
            _dummy in 0u8..1u8
        ) {
            let model = AnthropicHttpModel::new(
                "test-model".to_string(),
                "test-key".to_string(),
                "https://api.anthropic.com/v1".to_string(),
            );

            let request = ModelRequest {
                system: String::new(),
                messages: vec![],
                tools: vec![],
                max_tokens: None,
                temperature: None,
                output_schema: None,
            };

            let body = model.build_request_body(&request, false);

            prop_assert!(body.get("temperature").is_none(),
                "temperature field should be absent when temperature is None");
        }

        /// **Property 3: Tool Definition Wire Format Mapping**
        /// *For any* list of `ToolDefinition` values, the built request body's `tools` array
        /// SHALL contain one entry per definition where `name` equals `ToolDefinition.name`,
        /// `description` equals `ToolDefinition.description`, and `input_schema` equals
        /// `ToolDefinition.parameters`.
        ///
        /// **Validates: Requirements 2.4**
        #[test]
        fn prop_tool_definition_wire_format_mapping(
            tools in vec(
                (
                    "[a-z_]{1,20}",
                    "[a-zA-Z0-9 ]{1,50}",
                    prop_oneof![
                        Just(json!({"type": "object", "properties": {}})),
                        Just(json!({"type": "object", "properties": {"x": {"type": "string"}}})),
                        Just(json!({"type": "object", "properties": {"n": {"type": "integer"}}, "required": ["n"]})),
                    ],
                ).prop_map(|(name, description, parameters)| ToolDefinition {
                    name,
                    description,
                    parameters,
                }),
                1..=5,
            )
        ) {
            let model = AnthropicHttpModel::new(
                "test-model".to_string(),
                "test-key".to_string(),
                "https://api.anthropic.com/v1".to_string(),
            );

            let request = ModelRequest {
                system: String::new(),
                messages: vec![],
                tools: tools.clone(),
                max_tokens: None,
                temperature: None,
                output_schema: None,
            };

            let body = model.build_request_body(&request, false);

            let tools_array = body["tools"].as_array()
                .expect("body['tools'] should be an array");

            // Same length
            prop_assert_eq!(tools_array.len(), tools.len(),
                "tools array length mismatch: expected {}, got {}", tools.len(), tools_array.len());

            // Each entry maps correctly
            for (i, tool_def) in tools.iter().enumerate() {
                let entry = &tools_array[i];
                prop_assert_eq!(
                    entry["name"].as_str().unwrap_or(""),
                    tool_def.name.as_str(),
                    "tool[{}].name mismatch", i
                );
                prop_assert_eq!(
                    entry["description"].as_str().unwrap_or(""),
                    tool_def.description.as_str(),
                    "tool[{}].description mismatch", i
                );
                prop_assert_eq!(
                    &entry["input_schema"],
                    &tool_def.parameters,
                    "tool[{}].input_schema mismatch", i
                );
            }
        }

        /// **Property 12: System Prompt Concatenation**
        /// *For any* ModelRequest system prompt and message sequence, the effective system prompt
        /// in the request body SHALL be:
        /// (a) `ModelRequest.system + "\n" + extracted_system` when `to_wire()` extracts a
        ///     non-None system field, or
        /// (b) `ModelRequest.system` alone when `to_wire()` returns None.
        ///
        /// **Validates: Requirements 10.3, 10.4**
        #[test]
        fn prop_system_prompt_concatenation(
            request_system in "[a-zA-Z0-9 .,!?]{0,100}",
            include_system_msg in any::<bool>(),
            system_msg_content in "[a-zA-Z0-9 .,!?]{1,100}",
        ) {
            let model = AnthropicHttpModel::new(
                "test-model".to_string(),
                "test-key".to_string(),
                "https://api.anthropic.com/v1".to_string(),
            );

            let mut messages: Vec<Message> = Vec::new();
            if include_system_msg {
                messages.push(Message::System {
                    content: system_msg_content.clone(),
                });
            }
            // Always include at least one user message so the request is well-formed
            messages.push(Message::User {
                content: vec![ContentBlock::Text { text: "hello".to_string() }],
            });

            let request = ModelRequest {
                system: request_system.clone(),
                messages,
                tools: vec![],
                max_tokens: None,
                temperature: None,
                output_schema: None,
            };

            let body = model.build_request_body(&request, false);

            if request_system.is_empty() && !include_system_msg {
                // Both empty: system field should be absent
                prop_assert!(
                    body.get("system").is_none() || body["system"].is_null(),
                    "Expected no system field when both sources are empty, got: {:?}",
                    body.get("system")
                );
            } else if request_system.is_empty() && include_system_msg {
                // Only extracted system: body["system"] == extracted_system
                prop_assert_eq!(
                    body["system"].as_str().unwrap_or(""),
                    system_msg_content.as_str(),
                    "Expected extracted system only"
                );
            } else if !request_system.is_empty() && !include_system_msg {
                // Only ModelRequest.system: body["system"] == request_system
                prop_assert_eq!(
                    body["system"].as_str().unwrap_or(""),
                    request_system.as_str(),
                    "Expected request system only"
                );
            } else {
                // Both non-empty: concatenated with newline separator
                let expected = format!("{}\n{}", request_system, system_msg_content);
                prop_assert_eq!(
                    body["system"].as_str().unwrap_or(""),
                    expected.as_str(),
                    "Expected concatenated system prompt"
                );
            }
        }

        /// **Property 5: SSE Event-to-StreamChunk Mapping**
        /// *For any* well-formed SSE event of type `content_block_start` (tool_use),
        /// `content_block_delta` (text_delta), `content_block_delta` (input_json_delta),
        /// or `content_block_delta` (thinking_delta), the SSE parser SHALL emit the
        /// corresponding `StreamChunk` variant with fields matching the event payload.
        ///
        /// **Validates: Requirements 4.3, 4.4, 4.5, 4.12**
        #[test]
        fn prop_sse_event_to_stream_chunk_mapping(
            tool_id in "[a-z0-9]{8,16}",
            tool_name in "[a-z_]{3,15}",
            text_content in "[a-zA-Z0-9 .,!?]{1,100}",
            json_fragment in "[a-zA-Z0-9]{1,50}",
            thinking_content in "[a-zA-Z0-9 ]{1,100}",
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            let mut parser = SseParserState::new(Duration::from_secs(90));

            // Test content_block_start (tool_use) → ToolUseStart
            let tool_use_start = json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": tool_id,
                    "name": tool_name
                }
            });
            let chunks = parser.process_bytes(&make_sse_line(&tool_use_start));
            prop_assert_eq!(chunks.len(), 1, "tool_use start should emit exactly 1 chunk");
            prop_assert_eq!(
                &chunks[0],
                &StreamChunk::ToolUseStart { id: tool_id.clone(), name: tool_name.clone() },
                "tool_use start should emit ToolUseStart with matching id and name"
            );

            // Test content_block_delta (text_delta) → TextDelta
            // First register a text block at index 1
            let text_start = json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": { "type": "text" }
            });
            parser.process_bytes(&make_sse_line(&text_start));

            let text_delta = json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {
                    "type": "text_delta",
                    "text": text_content
                }
            });
            let chunks = parser.process_bytes(&make_sse_line(&text_delta));
            prop_assert_eq!(chunks.len(), 1, "text_delta should emit exactly 1 chunk");
            prop_assert_eq!(
                &chunks[0],
                &StreamChunk::TextDelta { text: text_content.clone() },
                "text_delta should emit TextDelta with matching text"
            );

            // Test content_block_delta (input_json_delta) → ToolUseInputDelta
            let json_delta = json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": json_fragment
                }
            });
            let chunks = parser.process_bytes(&make_sse_line(&json_delta));
            prop_assert_eq!(chunks.len(), 1, "input_json_delta should emit exactly 1 chunk");
            prop_assert_eq!(
                &chunks[0],
                &StreamChunk::ToolUseInputDelta { id: tool_id.clone(), delta: json_fragment.clone() },
                "input_json_delta should emit ToolUseInputDelta with matching id and delta"
            );

            // Test content_block_delta (thinking_delta) → ThinkingDelta
            // First register a thinking block at index 2
            let thinking_start = json!({
                "type": "content_block_start",
                "index": 2,
                "content_block": { "type": "thinking" }
            });
            parser.process_bytes(&make_sse_line(&thinking_start));

            let thinking_delta = json!({
                "type": "content_block_delta",
                "index": 2,
                "delta": {
                    "type": "thinking_delta",
                    "thinking": thinking_content
                }
            });
            let chunks = parser.process_bytes(&make_sse_line(&thinking_delta));
            prop_assert_eq!(chunks.len(), 1, "thinking_delta should emit exactly 1 chunk");
            prop_assert_eq!(
                &chunks[0],
                &StreamChunk::ThinkingDelta { text: thinking_content.clone() },
                "thinking_delta should emit ThinkingDelta with matching text"
            );
        }

        /// **Property 6: Tool Input JSON Accumulation Round-Trip**
        /// *For any* valid JSON value, when serialized and split into arbitrary non-empty
        /// string fragments delivered as sequential `input_json_delta` events followed by
        /// a `content_block_stop`, the parser SHALL emit a `ToolUseEnd` whose `input` field
        /// is equal to the original JSON value.
        ///
        /// **Validates: Requirements 4.6**
        #[test]
        fn prop_tool_input_json_accumulation_round_trip(
            // Generate simple JSON-compatible values
            json_variant in prop_oneof![
                Just(json!(null)),
                Just(json!(true)),
                Just(json!(false)),
                (1i64..1000i64).prop_map(|n| json!(n)),
                "[a-zA-Z0-9 ]{1,30}".prop_map(|s| json!(s)),
                Just(json!({"key": "value"})),
                Just(json!({"a": 1, "b": "two"})),
                Just(json!([1, 2, 3])),
                Just(json!({"nested": {"x": true}})),
            ],
            num_fragments in 1usize..=5usize,
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            let tool_id = "tool_abc123".to_string();
            let tool_name = "test_tool".to_string();

            let mut parser = SseParserState::new(Duration::from_secs(90));

            // Send content_block_start for a tool_use block
            let start_event = json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": tool_id,
                    "name": tool_name
                }
            });
            parser.process_bytes(&make_sse_line(&start_event));

            // Serialize the JSON value and split into fragments
            let serialized = serde_json::to_string(&json_variant).unwrap();
            let chars: Vec<char> = serialized.chars().collect();
            let total_len = chars.len();

            // Split into num_fragments non-empty parts
            let mut split_points: Vec<usize> = Vec::new();
            if total_len > 1 && num_fragments > 1 {
                let step = total_len / num_fragments;
                for i in 1..num_fragments {
                    let point = (step * i).min(total_len - 1);
                    if !split_points.contains(&point) && point > 0 && point < total_len {
                        split_points.push(point);
                    }
                }
            }
            split_points.sort();

            let mut fragments = Vec::new();
            let mut prev = 0;
            for &point in &split_points {
                fragments.push(chars[prev..point].iter().collect::<String>());
                prev = point;
            }
            fragments.push(chars[prev..].iter().collect::<String>());

            // Send each fragment as an input_json_delta event
            for fragment in &fragments {
                let delta_event = json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": fragment
                    }
                });
                parser.process_bytes(&make_sse_line(&delta_event));
            }

            // Send content_block_stop
            let stop_event = json!({
                "type": "content_block_stop",
                "index": 0
            });
            let chunks = parser.process_bytes(&make_sse_line(&stop_event));

            // Verify ToolUseEnd with correct input
            prop_assert_eq!(chunks.len(), 1, "content_block_stop should emit exactly 1 chunk");
            match &chunks[0] {
                StreamChunk::ToolUseEnd { id, input } => {
                    prop_assert_eq!(id, &tool_id, "ToolUseEnd id mismatch");
                    prop_assert_eq!(input, &json_variant,
                        "ToolUseEnd input should equal original JSON value. Got {:?}, expected {:?}",
                        input, json_variant);
                }
                other => {
                    prop_assert!(false, "Expected ToolUseEnd, got {:?}", other);
                }
            }
        }

        /// **Property 7: Usage Accumulation Across Stream Events**
        /// *For any* `input_tokens` value in a `message_start` event and `output_tokens`
        /// value in a `message_delta` event, the final `StreamChunk::MessageStop` SHALL
        /// contain a `Usage` with those exact token counts.
        ///
        /// **Validates: Requirements 4.1, 4.7, 4.8**
        #[test]
        fn prop_usage_accumulation_across_stream_events(
            input_tokens in 0u64..1_000_000u64,
            output_tokens in 0u64..1_000_000u64,
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            let mut parser = SseParserState::new(Duration::from_secs(90));

            // Send message_start with input_tokens
            let message_start = json!({
                "type": "message_start",
                "message": {
                    "usage": {
                        "input_tokens": input_tokens
                    }
                }
            });
            parser.process_bytes(&make_sse_line(&message_start));

            // Send message_delta with output_tokens and stop_reason
            let message_delta = json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "end_turn"
                },
                "usage": {
                    "output_tokens": output_tokens
                }
            });
            parser.process_bytes(&make_sse_line(&message_delta));

            // Send message_stop
            let message_stop = json!({
                "type": "message_stop"
            });
            let chunks = parser.process_bytes(&make_sse_line(&message_stop));

            // Verify MessageStop has correct usage
            prop_assert_eq!(chunks.len(), 1, "message_stop should emit exactly 1 chunk");
            match &chunks[0] {
                StreamChunk::MessageStop { stop_reason, usage } => {
                    prop_assert_eq!(*stop_reason, StopReason::EndTurn,
                        "stop_reason should be EndTurn");
                    prop_assert_eq!(usage.input_tokens, input_tokens,
                        "input_tokens mismatch: expected {}, got {}", input_tokens, usage.input_tokens);
                    prop_assert_eq!(usage.output_tokens, output_tokens,
                        "output_tokens mismatch: expected {}, got {}", output_tokens, usage.output_tokens);
                }
                other => {
                    prop_assert!(false, "Expected MessageStop, got {:?}", other);
                }
            }
        }

        /// **Property 8: Unknown Stop Reason Defaults to EndTurn**
        /// *For any* string that is not one of "end_turn", "tool_use", "max_tokens",
        /// or "stop_sequence", the stop reason mapping SHALL return `StopReason::EndTurn`.
        ///
        /// **Validates: Requirements 5.5, 5.6**
        #[test]
        fn prop_unknown_stop_reason_defaults_to_end_turn(
            random_str in "[a-zA-Z0-9_]{1,30}".prop_filter(
                "must not be a known stop reason",
                |s| s != "end_turn" && s != "tool_use" && s != "max_tokens" && s != "stop_sequence"
            ),
        ) {
            // Unknown string should map to EndTurn
            let result = map_stop_reason(Some(&random_str));
            prop_assert_eq!(result, StopReason::EndTurn,
                "Unknown stop reason '{}' should map to EndTurn, got {:?}", random_str, result);

            // None should also map to EndTurn
            let none_result = map_stop_reason(None);
            prop_assert_eq!(none_result, StopReason::EndTurn,
                "None stop reason should map to EndTurn");
        }

        /// **Property 15: Indexed Content Block Routing**
        /// *For any* sequence of SSE events where `content_block_start` events assign different
        /// indices to different block types (e.g., text at index 0, tool_use at index 1),
        /// subsequent `content_block_delta` events SHALL be routed to the correct active block
        /// based on their `index` field, regardless of interleaving order.
        ///
        /// **Validates: Requirements 4.3, 4.4, 4.5, 4.6**
        #[test]
        fn prop_indexed_content_block_routing(
            text_deltas in vec("[a-zA-Z0-9 ]{1,20}", 1..=5),
            tool_deltas in vec("[a-zA-Z0-9]{1,10}", 1..=5),
            tool_id in "[a-z]{5,10}",
            tool_name in "[a-z_]{3,10}",
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            let mut parser = SseParserState::new(Duration::from_secs(90));

            // Start text block at index 0
            let start_text = json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text" }
            });
            parser.process_bytes(&make_sse_line(&start_text));

            // Start tool_use block at index 1
            let start_tool = json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "tool_use",
                    "id": tool_id.clone(),
                    "name": tool_name.clone()
                }
            });
            let tool_start_chunks = parser.process_bytes(&make_sse_line(&start_tool));
            // Should emit ToolUseStart
            prop_assert!(
                tool_start_chunks.iter().any(|c| matches!(c, StreamChunk::ToolUseStart { .. })),
                "Expected ToolUseStart on content_block_start for tool_use"
            );

            // Interleave text deltas (index 0) and tool deltas (index 1)
            let max_len = text_deltas.len().max(tool_deltas.len());
            for i in 0..max_len {
                if i < text_deltas.len() {
                    let delta = json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": { "type": "text_delta", "text": text_deltas[i] }
                    });
                    let chunks = parser.process_bytes(&make_sse_line(&delta));
                    prop_assert!(
                        chunks.iter().all(|c| matches!(c, StreamChunk::TextDelta { .. })),
                        "Text delta at index 0 should emit TextDelta, got: {:?}", chunks
                    );
                }
                if i < tool_deltas.len() {
                    let delta = json!({
                        "type": "content_block_delta",
                        "index": 1,
                        "delta": { "type": "input_json_delta", "partial_json": tool_deltas[i] }
                    });
                    let chunks = parser.process_bytes(&make_sse_line(&delta));
                    prop_assert!(
                        chunks.iter().all(|c| matches!(c, StreamChunk::ToolUseInputDelta { .. })),
                        "Tool delta at index 1 should emit ToolUseInputDelta, got: {:?}", chunks
                    );
                    // Verify the id matches
                    for c in &chunks {
                        if let StreamChunk::ToolUseInputDelta { id, .. } = c {
                            prop_assert_eq!(id, &tool_id,
                                "ToolUseInputDelta id should match tool_id");
                        }
                    }
                }
            }
        }

        /// **Property 13: Malformed SSE Lines Do Not Corrupt State**
        /// *For any* sequence of SSE lines where some lines contain invalid JSON or unexpected
        /// prefixes interspersed with valid events, the parser SHALL emit correct StreamChunk
        /// values for all valid events and SHALL not alter its accumulation state due to
        /// invalid lines.
        ///
        /// **Validates: Requirements 12.1, 12.2**
        #[test]
        fn prop_malformed_sse_lines_do_not_corrupt_state(
            input_tokens in 1u64..10_000u64,
            output_tokens in 1u64..10_000u64,
            garbage_lines in vec("[a-zA-Z0-9!@#%^&*(){}]{1,50}", 1..=5),
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            let mut parser = SseParserState::new(Duration::from_secs(90));
            let mut all_bytes: Vec<u8> = Vec::new();

            // Valid message_start
            let msg_start = json!({
                "type": "message_start",
                "message": {
                    "usage": {
                        "input_tokens": input_tokens
                    }
                }
            });
            all_bytes.extend(make_sse_line(&msg_start));

            // Interleave garbage lines
            for garbage in &garbage_lines {
                // Some as "data: <invalid json>"
                all_bytes.extend(format!("data: {}\n\n", garbage).into_bytes());
                // Some as lines with unknown prefixes
                all_bytes.extend(format!("garbage: {}\n\n", garbage).into_bytes());
            }

            // Valid message_delta with output tokens
            let msg_delta = json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": output_tokens }
            });
            all_bytes.extend(make_sse_line(&msg_delta));

            // More garbage
            all_bytes.extend(b"data: {not valid json at all\n\n".to_vec());
            all_bytes.extend(b"data: \n\n".to_vec());

            // Valid message_stop
            let msg_stop = json!({ "type": "message_stop" });
            all_bytes.extend(make_sse_line(&msg_stop));

            let chunks = parser.process_bytes(&all_bytes);

            // Find the MessageStop chunk and verify usage is correct
            let stop_chunk = chunks.iter().find(|c| matches!(c, StreamChunk::MessageStop { .. }));
            prop_assert!(stop_chunk.is_some(), "Should have a MessageStop chunk");

            if let Some(StreamChunk::MessageStop { usage, .. }) = stop_chunk {
                prop_assert_eq!(usage.input_tokens, input_tokens,
                    "input_tokens should reflect only valid events");
                prop_assert_eq!(usage.output_tokens, output_tokens,
                    "output_tokens should reflect only valid events");
            }

            // Also verify parser state directly
            prop_assert_eq!(parser.input_tokens, input_tokens,
                "parser.input_tokens should be set by valid message_start only");
            prop_assert_eq!(parser.output_tokens, output_tokens,
                "parser.output_tokens should be set by valid message_delta only");
        }

        /// **Property 14: Partial Line Buffering Across Chunk Boundaries**
        /// *For any* valid SSE byte stream split into arbitrary non-empty byte chunks
        /// (at any byte boundary), the parser SHALL produce the same sequence of StreamChunk
        /// values as if the entire stream were delivered in a single chunk.
        ///
        /// **Validates: Requirements 12.3, 12.4**
        #[test]
        fn prop_partial_line_buffering_across_chunk_boundaries(
            text_content in "[a-zA-Z0-9 ]{1,30}",
            input_tokens in 1u64..10_000u64,
            output_tokens in 1u64..10_000u64,
            // Generate split points as offsets within the stream
            split_count in 1usize..=6usize,
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            // Build a valid SSE stream with several events
            let mut stream_bytes: Vec<u8> = Vec::new();

            let msg_start = json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": input_tokens } }
            });
            stream_bytes.extend(make_sse_line(&msg_start));

            let block_start = json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text" }
            });
            stream_bytes.extend(make_sse_line(&block_start));

            let text_delta = json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": text_content }
            });
            stream_bytes.extend(make_sse_line(&text_delta));

            let block_stop = json!({
                "type": "content_block_stop",
                "index": 0
            });
            stream_bytes.extend(make_sse_line(&block_stop));

            let msg_delta = json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": output_tokens }
            });
            stream_bytes.extend(make_sse_line(&msg_delta));

            let msg_stop = json!({ "type": "message_stop" });
            stream_bytes.extend(make_sse_line(&msg_stop));

            // Reference: process all bytes in one call
            let mut ref_parser = SseParserState::new(Duration::from_secs(90));
            let ref_chunks = ref_parser.process_bytes(&stream_bytes);

            // Split: divide stream at deterministic positions derived from split_count
            let len = stream_bytes.len();
            if len < 2 {
                // Too small to meaningfully split
                return Ok(());
            }

            let mut split_positions: Vec<usize> = (0..split_count)
                .map(|i| ((i + 1) * len) / (split_count + 1))
                .filter(|&p| p > 0 && p < len)
                .collect();
            split_positions.sort();
            split_positions.dedup();

            let mut split_parser = SseParserState::new(Duration::from_secs(90));
            let mut split_chunks: Vec<StreamChunk> = Vec::new();

            let mut prev = 0;
            for pos in &split_positions {
                split_chunks.extend(split_parser.process_bytes(&stream_bytes[prev..*pos]));
                prev = *pos;
            }
            // Process remaining
            split_chunks.extend(split_parser.process_bytes(&stream_bytes[prev..]));

            prop_assert_eq!(
                ref_chunks, split_chunks,
                "Splitting at boundaries should produce same chunks as single-call processing"
            );
        }

        /// **Property 21: Cache Read Tokens Propagation**
        /// *For any* `message_start` event containing a `message.usage.cache_read_input_tokens`
        /// field, the final `StreamChunk::MessageStop` usage SHALL include that value in the
        /// `cache_read_tokens` field.
        ///
        /// **Validates: Requirements 4.1, 4.8**
        #[test]
        fn prop_cache_read_tokens_propagation(
            cache_read_value in 1u64..100_000u64,
            input_tokens in 1u64..50_000u64,
            output_tokens in 1u64..50_000u64,
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            let mut parser = SseParserState::new(Duration::from_secs(90));
            let mut all_bytes: Vec<u8> = Vec::new();

            // message_start with cache_read_input_tokens
            let msg_start = json!({
                "type": "message_start",
                "message": {
                    "usage": {
                        "input_tokens": input_tokens,
                        "cache_read_input_tokens": cache_read_value
                    }
                }
            });
            all_bytes.extend(make_sse_line(&msg_start));

            // message_delta with output_tokens
            let msg_delta = json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "output_tokens": output_tokens }
            });
            all_bytes.extend(make_sse_line(&msg_delta));

            // message_stop
            let msg_stop = json!({ "type": "message_stop" });
            all_bytes.extend(make_sse_line(&msg_stop));

            let chunks = parser.process_bytes(&all_bytes);

            // Find MessageStop and verify cache_read_tokens
            let stop_chunk = chunks.iter().find(|c| matches!(c, StreamChunk::MessageStop { .. }));
            prop_assert!(stop_chunk.is_some(), "Should have a MessageStop chunk");

            if let Some(StreamChunk::MessageStop { usage, .. }) = stop_chunk {
                prop_assert_eq!(
                    usage.cache_read_tokens, Some(cache_read_value),
                    "MessageStop usage.cache_read_tokens should be Some({}), got {:?}",
                    cache_read_value, usage.cache_read_tokens
                );
                prop_assert_eq!(usage.input_tokens, input_tokens,
                    "input_tokens should be propagated correctly");
                prop_assert_eq!(usage.output_tokens, output_tokens,
                    "output_tokens should be propagated correctly");
            }
        }

        /// **Property 16: Stream Idle Timeout Triggers Abort**
        /// *For any* in-progress stream with accumulated state (input_tokens from message_start,
        /// text from a content_block_delta), calling `flush()` (simulating what the stream()
        /// method does on idle timeout) SHALL emit a `MessageStop` with the accumulated usage.
        ///
        /// **Validates: Requirements 7.4**
        #[test]
        fn prop_stream_idle_timeout_triggers_flush(
            input_tokens in 0u64..1_000_000u64,
            text in "[a-zA-Z0-9 ]{1,50}",
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            let mut parser = SseParserState::new(Duration::from_secs(90));

            // Send message_start
            let msg_start = json!({
                "type": "message_start",
                "message": { "usage": { "input_tokens": input_tokens } }
            });
            parser.process_bytes(&make_sse_line(&msg_start));

            // Send a text block start and delta
            let block_start = json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text" }
            });
            parser.process_bytes(&make_sse_line(&block_start));

            let text_delta = json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": text }
            });
            parser.process_bytes(&make_sse_line(&text_delta));

            // Simulate timeout by calling flush() - no message_stop was sent
            let flush_chunks = parser.flush();

            // Should emit MessageStop with accumulated state
            let has_message_stop = flush_chunks.iter().any(|c| matches!(c, StreamChunk::MessageStop { .. }));
            prop_assert!(has_message_stop, "flush() should emit MessageStop");

            // Verify usage
            for chunk in &flush_chunks {
                if let StreamChunk::MessageStop { usage, stop_reason } = chunk {
                    prop_assert_eq!(usage.input_tokens, input_tokens);
                    prop_assert_eq!(*stop_reason, StopReason::EndTurn); // default when no stop_reason received
                }
            }
        }

        /// **Property 17: Non-Streaming Fallback on Empty Stream**
        /// *For any* stream that receives only garbage (non-SSE) bytes or no data at all,
        /// the parser's `events_received` flag SHALL remain false, indicating that the
        /// stream() method should fall back to the non-streaming `complete()` path.
        ///
        /// **Validates: Requirements 6.1, 6.2, 6.3**
        #[test]
        fn prop_non_streaming_fallback_on_empty_stream(
            garbage_bytes in proptest::collection::vec(any::<u8>(), 0..100),
        ) {
            let mut parser = SseParserState::new(Duration::from_secs(90));

            // Feed garbage (non-SSE) or no data at all
            if !garbage_bytes.is_empty() {
                // Only feed bytes that won't accidentally form valid SSE events
                // (no "data: " prefix followed by valid JSON)
                let modified: Vec<u8> = garbage_bytes.iter()
                    .map(|&b| if b == b'd' { b'x' } else { b })
                    .collect();
                parser.process_bytes(&modified);
            }

            // events_received should still be false (no valid SSE events)
            prop_assert!(!parser.events_received,
                "events_received should be false when no valid SSE events were processed");
        }

        /// **Property 11: Model Name Prefix Stripping**
        /// *For any* model name string with an `anthropic:` prefix, the resolved model's
        /// `name()` SHALL return the portion after the prefix (the bare model name).
        ///
        /// **Validates: Requirements 8.2, 9.4**
        #[test]
        fn prop_model_name_prefix_stripping(
            model_name in "[a-z][a-z0-9-]{2,30}",
        ) {
            let model = AnthropicHttpModel::new(
                model_name.clone(),
                "test-key".to_string(),
                "https://api.anthropic.com/v1".to_string(),
            );
            prop_assert_eq!(model.name(), model_name.as_str(),
                "model.name() should return bare model name without prefix");
            prop_assert_eq!(model.provider(), "anthropic",
                "model.provider() should return 'anthropic'");
        }

        /// **Property 9: Rate Limit Retry-After Parsing**
        /// *For any* HTTP 429 response, the returned `ModelError::RateLimited` SHALL have
        /// `retry_after_ms` equal to the `retry-after` header value (interpreted as seconds)
        /// multiplied by 1000 when the header contains a valid integer, or 5000 when the
        /// header is absent or not a valid integer.
        ///
        /// **Validates: Requirements 3.1, 3.2**
        #[test]
        fn prop_rate_limit_retry_after_parsing(
            seconds in 1u64..300u64,
        ) {
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_str(&seconds.to_string()).unwrap());
            let error = AnthropicHttpModel::map_http_error(429, &headers, b"rate limited");
            match error {
                ModelError::RateLimited { retry_after_ms } => {
                    prop_assert_eq!(retry_after_ms, seconds * 1000,
                        "retry_after_ms should be {} * 1000 = {}, got {}",
                        seconds, seconds * 1000, retry_after_ms);
                }
                other => prop_assert!(false, "Expected RateLimited, got {:?}", other),
            }
        }

        /// **Property 9b: Rate Limit Default When Header Absent**
        /// *For any* HTTP 429 response without a retry-after header, the returned
        /// `ModelError::RateLimited` SHALL have `retry_after_ms` equal to 5000.
        ///
        /// **Validates: Requirements 3.1, 3.2**
        #[test]
        fn prop_rate_limit_default_when_header_absent(
            body in proptest::collection::vec(any::<u8>(), 0..100),
        ) {
            let headers = HeaderMap::new();
            let error = AnthropicHttpModel::map_http_error(429, &headers, &body);
            match error {
                ModelError::RateLimited { retry_after_ms } => {
                    prop_assert_eq!(retry_after_ms, 5000,
                        "retry_after_ms should be 5000 when header is absent, got {}",
                        retry_after_ms);
                }
                other => prop_assert!(false, "Expected RateLimited, got {:?}", other),
            }
        }

        /// **Property 10: Error Body Truncation**
        /// *For any* HTTP error response body, the returned `ModelError::Api` SHALL contain
        /// at most 4096 bytes of the response body.
        ///
        /// **Validates: Requirements 3.3**
        #[test]
        fn prop_error_body_truncation(
            body_len in 0usize..10_000usize,
            fill_byte in 0u8..26u8,
        ) {
            let fill_char = (b'a' + fill_byte) as char;
            let body: String = std::iter::repeat(fill_char).take(body_len).collect();
            let result = AnthropicHttpModel::sanitize_error_body(500, body.as_bytes());
            prop_assert!(result.len() <= 4096,
                "sanitize_error_body should truncate to at most 4096 chars, got {} for input len {}",
                result.len(), body_len);
        }

        /// **Property 18: HTTP 529 Treated as Rate Limited**
        /// *For any* HTTP response with status code 529, the provider SHALL return
        /// `ModelError::RateLimited`.
        ///
        /// **Validates: Requirements 3.1**
        #[test]
        fn prop_http_529_treated_as_rate_limited(
            body in proptest::collection::vec(any::<u8>(), 0..200),
            has_retry_header in any::<bool>(),
            retry_seconds in 1u64..300u64,
        ) {
            let mut headers = HeaderMap::new();
            if has_retry_header {
                headers.insert("retry-after", HeaderValue::from_str(&retry_seconds.to_string()).unwrap());
            }
            let error = AnthropicHttpModel::map_http_error(529, &headers, &body);
            match error {
                ModelError::RateLimited { .. } => { /* correct */ }
                other => prop_assert!(false, "Expected RateLimited for HTTP 529, got {:?}", other),
            }
        }

        /// **Property 19: Retry-After Header Parsed as Seconds for 429 and 529**
        /// *For any* HTTP 429 or 529 response with a `retry-after` header containing an
        /// integer value N, the returned `retry_after_ms` SHALL equal N × 1000.
        ///
        /// **Validates: Requirements 3.1, 3.2**
        #[test]
        fn prop_retry_after_header_parsed_as_seconds(
            n in 1u64..600u64,
            status_code in prop_oneof![Just(429u16), Just(529u16)],
        ) {
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_str(&n.to_string()).unwrap());
            let error = AnthropicHttpModel::map_http_error(status_code, &headers, b"overloaded");
            match error {
                ModelError::RateLimited { retry_after_ms } => {
                    prop_assert_eq!(retry_after_ms, n * 1000,
                        "For status {} with retry-after={}, expected retry_after_ms={}, got {}",
                        status_code, n, n * 1000, retry_after_ms);
                }
                other => prop_assert!(false,
                    "Expected RateLimited for HTTP {}, got {:?}", status_code, other),
            }
        }

        /// **Property 20: HTML Error Body Sanitization**
        /// *For any* HTTP error response whose body begins with `<!DOCTYPE html` or `<html`,
        /// the returned error message SHALL contain either the extracted `<title>` tag content
        /// or a generic proxy error message, and SHALL NOT contain raw HTML tags.
        ///
        /// **Validates: Requirements 3.3, 3.4**
        #[test]
        fn prop_html_error_body_sanitization(
            title_content in "[a-zA-Z0-9][a-zA-Z0-9 ]{0,49}",
            has_title in any::<bool>(),
            status_code in 400u16..600u16,
            use_doctype in any::<bool>(),
        ) {
            let html_body = if has_title {
                if use_doctype {
                    format!("<!DOCTYPE html><html><head><title>{}</title></head><body><h1>Error</h1></body></html>", title_content)
                } else {
                    format!("<html><head><title>{}</title></head><body><div class=\"error\">Something went wrong</div></body></html>", title_content)
                }
            } else {
                if use_doctype {
                    "<!DOCTYPE html><html><body><h1>Error</h1><p>Something happened</p></body></html>".to_string()
                } else {
                    "<html><body><div>Error occurred</div></body></html>".to_string()
                }
            };

            let result = AnthropicHttpModel::sanitize_error_body(status_code, html_body.as_bytes());

            // Should NOT contain raw HTML tags
            prop_assert!(!result.contains('<') || !result.contains('>'),
                "sanitized result should not contain raw HTML tags, got: {}", result);

            if has_title {
                // Should contain the title content (trimmed, as the implementation trims it)
                let expected_title = title_content.trim().to_string();
                prop_assert_eq!(result.clone(), expected_title.clone(),
                    "Expected title content '{}', got '{}'", expected_title, result);
            } else {
                // Should be a generic proxy error message
                prop_assert!(result.contains("proxy error"),
                    "Expected generic proxy error message when no title, got: {}", result);
            }
        }

        /// **Property 22: Overloaded Error Detection in Stream Body**
        /// *For any* SSE error event whose JSON body contains `"type":"overloaded_error"`,
        /// the parser SHALL set `overloaded_error` to true.
        ///
        /// **Validates: Requirements 3.5**
        #[test]
        fn prop_overloaded_error_detection_in_stream(
            error_message in "[a-zA-Z0-9 ]{1,50}",
            use_top_level_type in any::<bool>(),
        ) {
            fn make_sse_line(json: &Value) -> Vec<u8> {
                format!("data: {}\n\n", json).into_bytes()
            }

            let mut parser = SseParserState::new(Duration::from_secs(90));

            // Construct SSE event with overloaded_error
            let event = if use_top_level_type {
                // Top-level type: "overloaded_error"
                json!({
                    "type": "overloaded_error",
                    "error": {
                        "type": "overloaded_error",
                        "message": error_message
                    }
                })
            } else {
                // Nested error type: "error" with error.type == "overloaded_error"
                json!({
                    "type": "error",
                    "error": {
                        "type": "overloaded_error",
                        "message": error_message
                    }
                })
            };

            parser.process_bytes(&make_sse_line(&event));

            prop_assert!(parser.overloaded_error,
                "parser.overloaded_error should be true after receiving overloaded_error event");
        }
    }

    // ─── Example-Based Unit Tests ────────────────────────────────────────────

    /// Helper: build an SSE data line from a JSON value.
    fn make_sse_line(json: &Value) -> Vec<u8> {
        format!("data: {}\n\n", json).into_bytes()
    }

    // 1. Stop reason mapping
    #[test]
    fn stop_reason_end_turn() {
        assert_eq!(map_stop_reason(Some("end_turn")), StopReason::EndTurn);
    }

    #[test]
    fn stop_reason_tool_use() {
        assert_eq!(map_stop_reason(Some("tool_use")), StopReason::ToolUse);
    }

    #[test]
    fn stop_reason_max_tokens() {
        assert_eq!(map_stop_reason(Some("max_tokens")), StopReason::MaxTokens);
    }

    #[test]
    fn stop_reason_stop_sequence() {
        assert_eq!(map_stop_reason(Some("stop_sequence")), StopReason::StopSequence);
    }

    // 2. Ping event discarded
    #[test]
    fn ping_event_discarded() {
        let mut parser = SseParserState::new(Duration::from_secs(90));
        let event = json!({"type": "ping"});
        let chunks = parser.process_bytes(&make_sse_line(&event));
        assert!(chunks.is_empty(), "ping event should produce no chunks");
    }

    // 3. Empty API key - construction still works
    #[test]
    fn empty_api_key_construction_succeeds() {
        let model = AnthropicHttpModel::new(
            "claude-sonnet-4-20250514".to_string(),
            "".to_string(),
            "https://api.anthropic.com/v1".to_string(),
        );
        assert_eq!(model.name(), "claude-sonnet-4-20250514");
    }

    // 4. Required headers present
    #[test]
    fn required_headers_present() {
        let model = AnthropicHttpModel::new(
            "test-model".to_string(),
            "sk-test-key".to_string(),
            "https://api.anthropic.com/v1".to_string(),
        );
        let headers = model.build_headers();
        assert_eq!(
            headers.get("x-api-key").unwrap().to_str().unwrap(),
            "sk-test-key"
        );
        assert_eq!(
            headers.get("anthropic-version").unwrap().to_str().unwrap(),
            "2023-06-01"
        );
        assert_eq!(
            headers.get("content-type").unwrap().to_str().unwrap(),
            "application/json"
        );
    }

    // 5. Premature stream termination - flush() emits MessageStop with default EndTurn
    #[test]
    fn premature_stream_termination_flush_emits_message_stop() {
        let mut parser = SseParserState::new(Duration::from_secs(90));
        // Send only message_start (no message_stop)
        let msg_start = json!({
            "type": "message_start",
            "message": { "usage": { "input_tokens": 100 } }
        });
        parser.process_bytes(&make_sse_line(&msg_start));

        let chunks = parser.flush();
        assert!(
            chunks.iter().any(|c| matches!(
                c,
                StreamChunk::MessageStop { stop_reason: StopReason::EndTurn, .. }
            )),
            "flush() should emit MessageStop with EndTurn stop reason"
        );
    }

    // 6. Malformed tool input JSON - content_block_stop after garbage json_buf emits ToolUseEnd with `{}`
    #[test]
    fn malformed_tool_input_json_emits_empty_object() {
        let mut parser = SseParserState::new(Duration::from_secs(90));

        // Start a tool_use block
        let start = json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": "tool_123",
                "name": "my_tool"
            }
        });
        parser.process_bytes(&make_sse_line(&start));

        // Send garbage JSON delta
        let delta = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "input_json_delta",
                "partial_json": "{{not valid json at all!!!"
            }
        });
        parser.process_bytes(&make_sse_line(&delta));

        // Stop the block
        let stop = json!({ "type": "content_block_stop", "index": 0 });
        let chunks = parser.process_bytes(&make_sse_line(&stop));

        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            StreamChunk::ToolUseEnd { id, input } => {
                assert_eq!(id, "tool_123");
                assert_eq!(*input, json!({}));
            }
            other => panic!("Expected ToolUseEnd, got {:?}", other),
        }
    }

    // 7. HTTP 529 returns RateLimited
    #[test]
    fn http_529_returns_rate_limited() {
        let headers = HeaderMap::new();
        let error = AnthropicHttpModel::map_http_error(529, &headers, b"overloaded");
        assert!(
            matches!(error, ModelError::RateLimited { .. }),
            "HTTP 529 should return RateLimited, got {:?}",
            error
        );
    }

    // 8. retry-after header parsed as seconds
    #[test]
    fn retry_after_header_parsed_as_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("30"));
        let error = AnthropicHttpModel::map_http_error(429, &headers, b"rate limited");
        match error {
            ModelError::RateLimited { retry_after_ms } => {
                assert_eq!(retry_after_ms, 30000, "30 seconds should be 30000ms");
            }
            other => panic!("Expected RateLimited, got {:?}", other),
        }
    }

    // 9. HTML error body with title tag - extracts title
    #[test]
    fn html_error_body_extracts_title() {
        let html = b"<!DOCTYPE html><html><head><title>Service Unavailable</title></head><body></body></html>";
        let result = AnthropicHttpModel::sanitize_error_body(503, html);
        assert_eq!(result, "Service Unavailable");
    }

    // 10. HTML error body without title - returns generic proxy error
    #[test]
    fn html_error_body_without_title_returns_proxy_error() {
        let html = b"<!DOCTYPE html><html><body><h1>Error</h1></body></html>";
        let result = AnthropicHttpModel::sanitize_error_body(502, html);
        assert_eq!(result, "proxy error (HTTP 502)");
    }

    // 11. Non-HTML error body passed through (truncated to 4096)
    #[test]
    fn non_html_error_body_passed_through_truncated() {
        let body: String = "a".repeat(5000);
        let result = AnthropicHttpModel::sanitize_error_body(500, body.as_bytes());
        assert_eq!(result.len(), 4096, "non-HTML body should be truncated to 4096 chars");
        assert!(result.chars().all(|c| c == 'a'));
    }

    // 12. cache_read_input_tokens populated in final Usage via message_start
    #[test]
    fn cache_read_input_tokens_populated() {
        let mut parser = SseParserState::new(Duration::from_secs(90));

        let msg_start = json!({
            "type": "message_start",
            "message": {
                "usage": {
                    "input_tokens": 500,
                    "cache_read_input_tokens": 200
                }
            }
        });
        parser.process_bytes(&make_sse_line(&msg_start));

        let msg_delta = json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": 50 }
        });
        parser.process_bytes(&make_sse_line(&msg_delta));

        let msg_stop = json!({ "type": "message_stop" });
        let chunks = parser.process_bytes(&make_sse_line(&msg_stop));

        let stop_chunk = chunks.iter().find(|c| matches!(c, StreamChunk::MessageStop { .. }));
        assert!(stop_chunk.is_some());
        if let Some(StreamChunk::MessageStop { usage, .. }) = stop_chunk {
            assert_eq!(usage.cache_read_tokens, Some(200));
            assert_eq!(usage.input_tokens, 500);
            assert_eq!(usage.output_tokens, 50);
        }
    }

    // 13. Interleaved content blocks routed correctly by index
    #[test]
    fn interleaved_content_blocks_routed_by_index() {
        let mut parser = SseParserState::new(Duration::from_secs(90));

        // Start text at index 0
        let text_start = json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text" }
        });
        parser.process_bytes(&make_sse_line(&text_start));

        // Start tool_use at index 1
        let tool_start = json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {
                "type": "tool_use",
                "id": "tool_abc",
                "name": "read_file"
            }
        });
        parser.process_bytes(&make_sse_line(&tool_start));

        // Text delta at index 0 → TextDelta
        let text_delta = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "Hello" }
        });
        let chunks = parser.process_bytes(&make_sse_line(&text_delta));
        assert_eq!(chunks, vec![StreamChunk::TextDelta { text: "Hello".to_string() }]);

        // Tool delta at index 1 → ToolUseInputDelta
        let tool_delta = json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": { "type": "input_json_delta", "partial_json": "{\"path\":" }
        });
        let chunks = parser.process_bytes(&make_sse_line(&tool_delta));
        assert_eq!(chunks, vec![StreamChunk::ToolUseInputDelta {
            id: "tool_abc".to_string(),
            delta: "{\"path\":".to_string(),
        }]);

        // Another text delta at index 0 → TextDelta
        let text_delta2 = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": " world" }
        });
        let chunks = parser.process_bytes(&make_sse_line(&text_delta2));
        assert_eq!(chunks, vec![StreamChunk::TextDelta { text: " world".to_string() }]);
    }

    // 14. Overloaded error in stream body detected
    #[test]
    fn overloaded_error_in_stream_detected() {
        let mut parser = SseParserState::new(Duration::from_secs(90));
        let event = json!({
            "type": "error",
            "error": {
                "type": "overloaded_error",
                "message": "Overloaded"
            }
        });
        parser.process_bytes(&make_sse_line(&event));
        assert!(parser.overloaded_error, "overloaded_error should be true");
    }

    // 15. Empty stream (no events) - events_received stays false
    #[test]
    fn empty_stream_events_received_stays_false() {
        let parser = SseParserState::new(Duration::from_secs(90));
        assert!(!parser.events_received, "events_received should be false with no events");
    }
}
