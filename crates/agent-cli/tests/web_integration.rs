//! Integration test: spins up the real axum server on an ephemeral port,
//! drives it with a `tokio-tungstenite` WebSocket client, and asserts the
//! AG-UI event sequence — the single place this plan exercises server.rs,
//! session.rs, agui.rs, and ws_approval.rs wired together for real.

use std::net::SocketAddr;
use std::sync::Arc;

use agent_core::{
    ApprovalRequirement, Concurrency, InMemoryTaskStore, Instructions, Message, Model, ModelError,
    ModelProvider, ModelRequest, ModelResponse, ModelStream, PermissionMode, SessionStore,
    StopReason, StreamChunk, TaskStore, Tool, ToolContext, ToolError, ToolOutput, Usage,
};
use agent_cli::web;
use async_trait::async_trait;
use futures::{stream, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// A no-op session store — these tests don't exercise resume/history-on-disk.
struct NullSessionStore;

#[async_trait]
impl SessionStore for NullSessionStore {
    async fn append(&self, _id: &str, _messages: &[Message]) -> Result<(), agent_core::SessionStoreError> {
        Ok(())
    }
    async fn save(&self, _id: &str, _messages: &[Message]) -> Result<(), agent_core::SessionStoreError> {
        Ok(())
    }
    async fn load(&self, _id: &str) -> Result<Vec<Message>, agent_core::SessionStoreError> {
        Ok(Vec::new())
    }
    async fn list(&self) -> Result<Vec<agent_core::SessionMeta>, agent_core::SessionStoreError> {
        Ok(Vec::new())
    }
    async fn delete(&self, _id: &str) -> Result<(), agent_core::SessionStoreError> {
        Ok(())
    }
}

/// Replies "hello from mock" with no tool calls — exercises the plain chat path.
struct EchoModel;

#[async_trait]
impl Model for EchoModel {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
        let chunks = vec![
            Ok(StreamChunk::TextDelta { text: "hello from mock".to_string() }),
            Ok(StreamChunk::MessageStop { stop_reason: StopReason::EndTurn, usage: Usage::default() }),
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }
    fn name(&self) -> &str { "echo-model" }
    fn provider(&self) -> &str { "mock" }
    fn context_window(&self) -> usize { 128_000 }
    fn max_output_tokens(&self) -> usize { 4096 }
    fn supports_tools(&self) -> bool { true }
    fn input_cost_per_million(&self) -> f64 { 0.0 }
    fn output_cost_per_million(&self) -> f64 { 0.0 }
}

/// Calls the `always_ask` tool (which always requires approval) once, then
/// finishes with a text reply once it sees the tool result.
struct ApprovalFlowModel;

#[async_trait]
impl Model for ApprovalFlowModel {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
        let has_tool_result = request.messages.iter().any(|m| matches!(m, Message::ToolResult { .. }));
        let chunks = if has_tool_result {
            vec![
                Ok(StreamChunk::TextDelta { text: "acknowledged".to_string() }),
                Ok(StreamChunk::MessageStop { stop_reason: StopReason::EndTurn, usage: Usage::default() }),
            ]
        } else {
            vec![
                Ok(StreamChunk::ToolUseStart { id: "tu_1".to_string(), name: "always_ask".to_string() }),
                Ok(StreamChunk::ToolUseEnd { id: "tu_1".to_string(), input: json!({}) }),
                Ok(StreamChunk::MessageStop { stop_reason: StopReason::ToolUse, usage: Usage::default() }),
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }
    fn name(&self) -> &str { "approval-flow-model" }
    fn provider(&self) -> &str { "mock" }
    fn context_window(&self) -> usize { 128_000 }
    fn max_output_tokens(&self) -> usize { 4096 }
    fn supports_tools(&self) -> bool { true }
    fn input_cost_per_million(&self) -> f64 { 0.0 }
    fn output_cost_per_million(&self) -> f64 { 0.0 }
}

struct SingleModelProvider<M: Model + 'static> {
    model: Arc<M>,
}

#[async_trait]
impl<M: Model + 'static> ModelProvider for SingleModelProvider<M> {
    async fn resolve(&self, _model_name: &str) -> Result<Arc<dyn Model>, ModelError> {
        Ok(self.model.clone())
    }
    fn available_models(&self) -> Vec<String> {
        vec!["mock".to_string()]
    }
}

struct AlwaysAskTool;

#[async_trait]
impl Tool for AlwaysAskTool {
    fn name(&self) -> &str { "always_ask" }
    fn description(&self) -> &str { "test tool that always requires approval" }
    fn parameters_schema(&self) -> Value { json!({"type": "object", "properties": {}}) }
    fn concurrency(&self, _input: &Value) -> Concurrency { Concurrency::Safe }
    async fn execute(&self, _input: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Text("did the risky thing".to_string()))
    }
    fn approval_requirement(&self) -> ApprovalRequirement { ApprovalRequirement::Always }
}

/// Starts a real web server on an OS-assigned port and returns its address.
async fn start_test_server<M: Model + 'static>(
    model: Arc<M>,
    tools: Vec<Arc<dyn Tool>>,
    permission_mode: PermissionMode,
) -> SocketAddr {
    let config = web::server::WebServerConfig {
        provider: Arc::new(SingleModelProvider { model }),
        model: "mock".to_string(),
        tools,
        instructions: Instructions::Static("test agent".to_string()),
        permission_mode,
        task_store: Arc::new(InMemoryTaskStore::new()) as Arc<dyn TaskStore>,
        session_store: Arc::new(NullSessionStore) as Arc<dyn SessionStore>,
        session_id: "test-session".to_string(),
    };
    let shared = Arc::new(web::session::SharedSessionState::new(Vec::new()));
    let router = web::server::build_router(config, shared);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

type TestWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: SocketAddr) -> TestWs {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws")).await.unwrap();
    ws
}

async fn recv_json(ws: &mut TestWs) -> Value {
    loop {
        match ws.next().await.expect("stream ended unexpectedly").unwrap() {
            WsMessage::Text(text) => return serde_json::from_str(&text).unwrap(),
            _ => continue,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn user_message_produces_expected_agui_event_sequence() {
    let addr = start_test_server(Arc::new(EchoModel), vec![], PermissionMode::Bypass).await;
    let mut ws = connect(addr).await;

    // Connect-time snapshot events.
    let snapshot = recv_json(&mut ws).await;
    assert_eq!(snapshot["type"], "MessagesSnapshot");
    assert_eq!(snapshot["messages"], json!([]));
    let task_snapshot = recv_json(&mut ws).await;
    assert_eq!(task_snapshot["name"], "arlo.task_snapshot");
    let todo_snapshot = recv_json(&mut ws).await;
    assert_eq!(todo_snapshot["name"], "arlo.todo_snapshot");

    ws.send(WsMessage::Text(json!({"type": "user_message", "text": "hi"}).to_string().into()))
        .await
        .unwrap();

    // The run loop emits TurnStart (-> StepStarted) at the top of every turn,
    // before any model output.
    let step_started = recv_json(&mut ws).await;
    assert_eq!(step_started["type"], "StepStarted");

    let start = recv_json(&mut ws).await;
    assert_eq!(start["type"], "TextMessageStart");
    let content = recv_json(&mut ws).await;
    assert_eq!(content["type"], "TextMessageContent");
    assert_eq!(content["delta"], "hello from mock");
    let end = recv_json(&mut ws).await;
    assert_eq!(end["type"], "TextMessageEnd");
    let finished = recv_json(&mut ws).await;
    assert_eq!(finished["type"], "RunFinished");
    assert_eq!(finished["result"]["output"], "hello from mock");
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_call_requiring_approval_round_trips_through_websocket() {
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(AlwaysAskTool)];
    let addr = start_test_server(Arc::new(ApprovalFlowModel), tools, PermissionMode::Normal).await;
    let mut ws = connect(addr).await;

    // Drain connect-time snapshots.
    recv_json(&mut ws).await;
    recv_json(&mut ws).await;
    recv_json(&mut ws).await;

    ws.send(WsMessage::Text(json!({"type": "user_message", "text": "do the risky thing"}).to_string().into()))
        .await
        .unwrap();

    let step_started = recv_json(&mut ws).await;
    assert_eq!(step_started["type"], "StepStarted");

    let tool_start = recv_json(&mut ws).await;
    assert_eq!(tool_start["type"], "ToolCallStart");
    assert_eq!(tool_start["toolCallName"], "always_ask");

    // agent-core's run loop executes the tool eagerly (Phase 4) and only
    // checks the permission engine afterward, in resolve_next_step — so
    // ToolCallEnd/ToolCallResult arrive *before* the permission_request that
    // gates whether the run continues to the next turn.
    let tool_end = recv_json(&mut ws).await;
    assert_eq!(tool_end["type"], "ToolCallEnd");
    let tool_result = recv_json(&mut ws).await;
    assert_eq!(tool_result["type"], "ToolCallResult");
    assert_eq!(tool_result["content"], "did the risky thing");

    let permission_request = recv_json(&mut ws).await;
    assert_eq!(permission_request["type"], "Custom");
    assert_eq!(permission_request["name"], "arlo.permission_request");
    let call_id = permission_request["value"]["callId"].as_str().unwrap().to_string();
    assert_eq!(permission_request["value"]["toolName"], "always_ask");

    ws.send(WsMessage::Text(
        json!({
            "type": "approval_response",
            "responses": [{"callId": call_id, "decision": "allow_once", "pattern": null}]
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    let step_started_2 = recv_json(&mut ws).await;
    assert_eq!(step_started_2["type"], "StepStarted");

    let text_start = recv_json(&mut ws).await;
    assert_eq!(text_start["type"], "TextMessageStart");
    let text_content = recv_json(&mut ws).await;
    assert_eq!(text_content["delta"], "acknowledged");
    let text_end = recv_json(&mut ws).await;
    assert_eq!(text_end["type"], "TextMessageEnd");
    let finished = recv_json(&mut ws).await;
    assert_eq!(finished["type"], "RunFinished");
}
