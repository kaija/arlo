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

    // agent-core's run loop (a separate tokio task spawned by run_stream)
    // emits TurnStart/ToolStart/ToolEnd into its own internal channel, and
    // separately calls approval_handler.request_approval() (which sends the
    // Custom permission_request directly to the shared out_tx) once
    // resolve_next_step sees the tool needs approval. spawn_run's own task
    // has to be scheduled separately to drain that internal channel and
    // forward the converted AguiEvents to the same out_tx, so these two
    // producers race: StepStarted/ToolCallStart/ToolCallEnd/ToolCallResult
    // and the permission_request can arrive in any relative order.
    let mut got_step_started = false;
    let mut got_tool_start = false;
    let mut got_tool_end = false;
    let mut got_tool_result = false;
    let mut call_id = None;
    for _ in 0..5 {
        let event = recv_json(&mut ws).await;
        match event["type"].as_str().unwrap() {
            "StepStarted" => got_step_started = true,
            "ToolCallStart" => {
                got_tool_start = true;
                assert_eq!(event["toolCallName"], "always_ask");
            }
            "ToolCallEnd" => got_tool_end = true,
            "ToolCallResult" => {
                got_tool_result = true;
                assert_eq!(event["content"], "did the risky thing");
            }
            "Custom" if event["name"] == "arlo.permission_request" => {
                call_id = Some(event["value"]["callId"].as_str().unwrap().to_string());
                assert_eq!(event["value"]["toolName"], "always_ask");
            }
            other => panic!("unexpected event type: {other}"),
        }
    }
    assert!(got_step_started, "expected a StepStarted event");
    assert!(got_tool_start, "expected a ToolCallStart event");
    assert!(got_tool_end, "expected a ToolCallEnd event");
    assert!(got_tool_result, "expected a ToolCallResult event");
    let call_id = call_id.expect("expected an arlo.permission_request event");

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

#[tokio::test(flavor = "multi_thread")]
async fn second_connection_takes_over_and_first_is_closed() {
    let addr = start_test_server(Arc::new(EchoModel), vec![], PermissionMode::Bypass).await;

    let mut tab_a = connect(addr).await;
    recv_json(&mut tab_a).await; // MessagesSnapshot
    recv_json(&mut tab_a).await; // task_snapshot
    recv_json(&mut tab_a).await; // todo_snapshot

    tab_a
        .send(WsMessage::Text(json!({"type": "user_message", "text": "hi"}).to_string().into()))
        .await
        .unwrap();
    // Drain tab A's reply so shared history has one exchange before tab B connects.
    loop {
        let event = recv_json(&mut tab_a).await;
        if event["type"] == "RunFinished" {
            break;
        }
    }

    let mut tab_b = connect(addr).await;

    // Tab A should see a session_closed notice, then the connection closes.
    let closed = recv_json(&mut tab_a).await;
    assert_eq!(closed["type"], "Custom");
    assert_eq!(closed["name"], "arlo.session_closed");
    let next = tab_a.next().await;
    assert!(
        matches!(next, None | Some(Ok(WsMessage::Close(_)))),
        "expected the connection to close after arlo.session_closed, got {next:?}"
    );

    // Tab B sees the history tab A built up, not an empty fresh session.
    let snapshot = recv_json(&mut tab_b).await;
    assert_eq!(snapshot["type"], "MessagesSnapshot");
    let messages = snapshot["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "expected the user + assistant messages from tab A: {messages:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ticker_pushes_task_notice_on_terminal_transition() {
    let store = Arc::new(InMemoryTaskStore::new());
    let config = web::server::WebServerConfig {
        provider: Arc::new(SingleModelProvider { model: Arc::new(EchoModel) }),
        model: "mock".to_string(),
        tools: vec![],
        instructions: Instructions::Static("test agent".to_string()),
        permission_mode: PermissionMode::Bypass,
        task_store: store.clone() as Arc<dyn TaskStore>,
        session_store: Arc::new(NullSessionStore) as Arc<dyn SessionStore>,
        session_id: "test-session".to_string(),
    };
    let shared = Arc::new(web::session::SharedSessionState::new(Vec::new()));
    let router = web::server::build_router(config, shared);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let mut ws = connect(addr).await;
    recv_json(&mut ws).await; // MessagesSnapshot
    recv_json(&mut ws).await; // task_snapshot (empty)
    recv_json(&mut ws).await; // todo_snapshot (empty)

    // Register and complete a task out-of-band, as SubAgentTool would.
    let id = store
        .create_task(agent_core::CreateTaskParams {
            description: "background check".to_string(),
            task_type: agent_core::TaskType::Background,
            dependencies: vec![],
            max_retries: 0,
        })
        .await
        .unwrap();
    store.transition_task(&id, agent_core::TaskStatus::Running, None).await.unwrap();
    store
        .transition_task(&id, agent_core::TaskStatus::Completed, Some("42 files".to_string()))
        .await
        .unwrap();

    // The 500ms ticker should notice within a couple of ticks.
    let mut saw_notice = false;
    let mut saw_snapshot = false;
    for _ in 0..10 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), recv_json(&mut ws))
            .await
            .expect("timed out waiting for ticker event");
        if event["type"] == "Custom" && event["name"] == "arlo.task_notice" {
            assert_eq!(event["value"]["taskId"], id.clone());
            assert_eq!(event["value"]["status"], "Completed");
            assert_eq!(event["value"]["summary"], "42 files");
            saw_notice = true;
        }
        if event["type"] == "Custom" && event["name"] == "arlo.task_snapshot" {
            let tasks = event["value"].as_array().unwrap();
            if tasks.len() == 1 && tasks[0]["status"] == "Completed" {
                saw_snapshot = true;
            }
        }
        if saw_notice && saw_snapshot {
            break;
        }
    }
    assert!(saw_notice, "expected an arlo.task_notice event");
    assert!(saw_snapshot, "expected an updated arlo.task_snapshot event");
}
