//! Integration tests for AnthropicHttpModel with mock HTTP server.
//!
//! Tests the full HTTP path using a lightweight tokio TcpListener-based mock server
//! that returns preset responses, verifying correct behavior for streaming, non-streaming,
//! error handling, and edge cases.

use agent_core::error::ModelError;
use agent_core::model::{Model, ModelRequest};
use agent_core::stream::{StopReason, StreamChunk};
use agent_llm::AnthropicHttpModel;
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Start a mock TCP server that accepts a single connection and responds with the given bytes.
///
/// Returns the URL (http://127.0.0.1:{port}) to connect to.
async fn mock_server(response: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the request (consume all headers + body)
        let mut buf = [0u8; 8192];
        let _ = socket.read(&mut buf).await;
        // Write the preset response
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    });

    url
}

/// Create a minimal ModelRequest for testing.
fn test_request() -> ModelRequest {
    ModelRequest {
        system: "test".to_string(),
        messages: vec![],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        output_schema: None,
    }
}

// ─── Test 1: Full Streaming Round-Trip ───────────────────────────────────────

#[tokio::test]
async fn streaming_round_trip_with_mock() {
    let sse_response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "content-type: text/event-stream\r\n",
        "\r\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":100}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let url = mock_server(sse_response.to_string()).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "test-key".to_string(), url);

    let mut stream = model.stream(test_request()).await.unwrap();
    let mut chunks = Vec::new();
    while let Some(result) = stream.next().await {
        chunks.push(result.unwrap());
    }

    // Should have TextDelta chunks
    let text_deltas: Vec<&StreamChunk> = chunks
        .iter()
        .filter(|c| matches!(c, StreamChunk::TextDelta { .. }))
        .collect();
    assert_eq!(text_deltas.len(), 2);

    // Check the text content
    if let StreamChunk::TextDelta { text } = &text_deltas[0] {
        assert_eq!(text, "Hello");
    }
    if let StreamChunk::TextDelta { text } = &text_deltas[1] {
        assert_eq!(text, " world");
    }

    // Should have MessageStop with correct usage
    let message_stop = chunks
        .iter()
        .find(|c| matches!(c, StreamChunk::MessageStop { .. }));
    assert!(message_stop.is_some());

    if let Some(StreamChunk::MessageStop { stop_reason, usage }) = message_stop {
        assert_eq!(*stop_reason, StopReason::EndTurn);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 5);
    }
}

// ─── Test 2: Non-Streaming Completion ────────────────────────────────────────

#[tokio::test]
async fn non_streaming_completion_with_mock() {
    let body = r#"{"id":"msg_test","type":"message","role":"assistant","content":[{"type":"text","text":"Hello from Anthropic!"}],"stop_reason":"end_turn","usage":{"input_tokens":50,"output_tokens":10}}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let url = mock_server(response).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "test-key".to_string(), url);

    let result = model.complete(test_request()).await.unwrap();

    assert_eq!(result.content.len(), 1);
    if let agent_core::model::ContentBlock::Text { text } = &result.content[0] {
        assert_eq!(text, "Hello from Anthropic!");
    } else {
        panic!("Expected Text content block");
    }
    assert_eq!(result.usage.input_tokens, 50);
    assert_eq!(result.usage.output_tokens, 10);
    assert_eq!(result.stop_reason, StopReason::EndTurn);
}

// ─── Test 3: HTTP 429 Error ──────────────────────────────────────────────────

#[tokio::test]
async fn http_429_returns_rate_limited_error() {
    let body = r#"{"error":{"type":"rate_limit_error"}}"#;
    let response = format!(
        "HTTP/1.1 429 Too Many Requests\r\nretry-after: 30\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let url = mock_server(response).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "test-key".to_string(), url);

    let result = model.complete(test_request()).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        ModelError::RateLimited { retry_after_ms } => {
            // The complete() path hardcodes 5000ms for 429/529
            // (only stream() path uses map_http_error which parses retry-after)
            assert_eq!(retry_after_ms, 5000);
        }
        other => panic!("Expected RateLimited error, got: {:?}", other),
    }
}

// ─── Test 4: HTTP 529 Error ──────────────────────────────────────────────────

#[tokio::test]
async fn http_529_returns_rate_limited_error() {
    let body = r#"{"error":{"type":"overloaded_error"}}"#;
    let response = format!(
        "HTTP/1.1 529 Overloaded\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let url = mock_server(response).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "test-key".to_string(), url);

    let result = model.complete(test_request()).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        ModelError::RateLimited { retry_after_ms } => {
            // No retry-after header, should default to 5000ms
            assert_eq!(retry_after_ms, 5000);
        }
        other => panic!("Expected RateLimited error, got: {:?}", other),
    }
}

// ─── Test 5: HTTP 401 Error ──────────────────────────────────────────────────

#[tokio::test]
async fn http_401_returns_api_error() {
    let body = r#"{"error":{"type":"authentication_error","message":"invalid"}}"#;
    let response = format!(
        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let url = mock_server(response).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "bad-key".to_string(), url);

    let result = model.complete(test_request()).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        ModelError::Api { status, body } => {
            assert_eq!(status, 401);
            assert!(body.contains("authentication_error"));
        }
        other => panic!("Expected Api error, got: {:?}", other),
    }
}

// ─── Test 6: HTTP 500 Error ──────────────────────────────────────────────────

#[tokio::test]
async fn http_500_returns_api_error() {
    let body = r#"{"error":{"type":"server_error","message":"internal"}}"#;
    let response = format!(
        "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let url = mock_server(response).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "test-key".to_string(), url);

    let result = model.complete(test_request()).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        ModelError::Api { status, body } => {
            assert_eq!(status, 500);
            assert!(body.contains("server_error"));
        }
        other => panic!("Expected Api error, got: {:?}", other),
    }
}

// ─── Test 7: Connection Failure ──────────────────────────────────────────────

#[tokio::test]
async fn connection_failure_returns_connection_error() {
    // Use an address that is guaranteed to refuse connections immediately.
    // Port 1 on localhost should refuse the connection.
    let model = AnthropicHttpModel::new(
        "claude-test".to_string(),
        "test-key".to_string(),
        "http://127.0.0.1:1".to_string(),
    );

    let result = model.complete(test_request()).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        ModelError::Connection(msg) => {
            assert!(!msg.is_empty(), "Connection error should have a message");
        }
        other => panic!("Expected Connection error, got: {:?}", other),
    }
}

// ─── Test 8: HTML Error Page ─────────────────────────────────────────────────

#[tokio::test]
async fn html_error_page_returns_sanitized_api_error() {
    // Test through the stream() path which uses map_http_error() with HTML sanitization
    let html_body = concat!(
        "<!DOCTYPE html><html><head><title>502 Bad Gateway</title></head>",
        "<body><h1>502 Bad Gateway</h1><p>CloudFlare proxy error</p></body></html>",
    );
    let response = format!(
        "HTTP/1.1 502 Bad Gateway\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\r\n{}",
        html_body.len(),
        html_body
    );

    let url = mock_server(response).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "test-key".to_string(), url);

    // Use stream() path which invokes map_http_error with HTML sanitization
    let result = model.stream(test_request()).await;

    match result {
        Err(ModelError::Api { status, body }) => {
            assert_eq!(status, 502);
            // The stream path uses sanitize_error_body which extracts <title>
            assert!(
                !body.contains("<html") && !body.contains("<body"),
                "Error body should be sanitized, got: {}",
                body
            );
            // Should contain the title content
            assert!(
                body.contains("502 Bad Gateway") || body.contains("proxy error"),
                "Error body should contain meaningful message, got: {}",
                body
            );
        }
        Err(other) => panic!("Expected Api error, got: {:?}", other),
        Ok(_) => panic!("Expected error, got Ok"),
    }
}

// ─── Test: Streaming with tool use ───────────────────────────────────────────

#[tokio::test]
async fn streaming_with_tool_use_interleaved() {
    let sse_response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "content-type: text/event-stream\r\n",
        "\r\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":50}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me read that.\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_001\",\"name\":\"read_file\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\": \\\"src/main.rs\\\"}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":20}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    let url = mock_server(sse_response.to_string()).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "test-key".to_string(), url);

    let mut stream = model.stream(test_request()).await.unwrap();
    let mut chunks = Vec::new();
    while let Some(result) = stream.next().await {
        chunks.push(result.unwrap());
    }

    // Should have TextDelta
    assert!(chunks
        .iter()
        .any(|c| matches!(c, StreamChunk::TextDelta { .. })));

    // Should have ToolUseStart
    let tool_start = chunks
        .iter()
        .find(|c| matches!(c, StreamChunk::ToolUseStart { .. }));
    assert!(tool_start.is_some());
    if let Some(StreamChunk::ToolUseStart { id, name }) = tool_start {
        assert_eq!(id, "tu_001");
        assert_eq!(name, "read_file");
    }

    // Should have ToolUseEnd with accumulated JSON
    let tool_end = chunks
        .iter()
        .find(|c| matches!(c, StreamChunk::ToolUseEnd { .. }));
    assert!(tool_end.is_some());
    if let Some(StreamChunk::ToolUseEnd { id, input }) = tool_end {
        assert_eq!(id, "tu_001");
        assert_eq!(input["path"], "src/main.rs");
    }

    // Should have MessageStop with tool_use stop reason
    let message_stop = chunks
        .iter()
        .find(|c| matches!(c, StreamChunk::MessageStop { .. }));
    assert!(message_stop.is_some());
    if let Some(StreamChunk::MessageStop { stop_reason, usage }) = message_stop {
        assert_eq!(*stop_reason, StopReason::ToolUse);
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.output_tokens, 20);
    }
}

// ─── Test: HTTP 429 on streaming path ────────────────────────────────────────

#[tokio::test]
async fn streaming_http_429_returns_rate_limited_error() {
    let body = r#"{"error":{"type":"rate_limit_error"}}"#;
    let response = format!(
        "HTTP/1.1 429 Too Many Requests\r\nretry-after: 10\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let url = mock_server(response).await;

    let model = AnthropicHttpModel::new("claude-test".to_string(), "test-key".to_string(), url);

    let result = model.stream(test_request()).await;

    match result {
        Err(ModelError::RateLimited { retry_after_ms }) => {
            // stream() path uses map_http_error which parses retry-after as seconds * 1000
            assert_eq!(retry_after_ms, 10000);
        }
        Err(other) => panic!("Expected RateLimited error, got: {:?}", other),
        Ok(_) => panic!("Expected error, got Ok"),
    }
}
