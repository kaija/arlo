//! Per-WebSocket-connection session driver — the web analogue of
//! `tui/mod.rs::run_tui_repl` + `tui/event_loop.rs`.

use std::sync::Arc;

use agent_core::{
    run_stream, Agent, ApprovalResponse, ContentBlock, Input, Message as CoreMessage,
    PermissionEngine, RunConfig, RunEvent,
};
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

use super::agui::{AguiEvent, AguiEventConverter};
use super::server::WebServerConfig;
use super::ws_approval::WebApprovalHandler;

/// Conversation state shared across reconnects of the *same* `arlo --web`
/// process: a second browser tab must see the history the first tab built
/// up, not the history the process started with.
pub struct SharedSessionState {
    history: Mutex<Vec<CoreMessage>>,
    // unused until Task 5 wires up takeover
    #[allow(dead_code)]
    active: Mutex<Option<oneshot::Sender<()>>>,
}

impl SharedSessionState {
    pub fn new(initial_history: Vec<CoreMessage>) -> Self {
        Self { history: Mutex::new(initial_history), active: Mutex::new(None) }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    UserMessage { text: String },
    ApprovalResponse { responses: Vec<ClientApprovalResponse> },
    Abort,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientApprovalResponse {
    call_id: String,
    decision: String,
    pattern: Option<String>,
}

fn parse_decision(decision: &str, pattern: Option<String>) -> Option<ApprovalResponse> {
    match decision {
        "allow_once" => Some(ApprovalResponse::Allow),
        "reject_once" | "reject_always" => Some(ApprovalResponse::Deny),
        "allow_always" => Some(ApprovalResponse::AlwaysAllow { pattern: pattern.unwrap_or_default() }),
        _ => None,
    }
}

async fn send_event(ws_tx: &mut (impl SinkExt<WsMessage, Error = axum::Error> + Unpin), event: AguiEvent) -> bool {
    let text = serde_json::to_string(&event).expect("AguiEvent always serializes");
    ws_tx.send(WsMessage::Text(text.into())).await.is_ok()
}

async fn send_task_and_todo_snapshot(
    ws_tx: &mut (impl SinkExt<WsMessage, Error = axum::Error> + Unpin),
    task_store: &Arc<dyn agent_core::TaskStore>,
) {
    if let Ok(tasks) = task_store.list_tasks(None).await {
        send_event(ws_tx, AguiEvent::Custom {
            name: "arlo.task_snapshot".to_string(),
            value: serde_json::to_value(&tasks).unwrap_or_default(),
        }).await;
    }
    if let Ok(todos) = task_store.list_todos().await {
        send_event(ws_tx, AguiEvent::Custom {
            name: "arlo.todo_snapshot".to_string(),
            value: serde_json::to_value(&todos).unwrap_or_default(),
        }).await;
    }
}

fn spawn_run(
    config: &WebServerConfig,
    approval_handler: Arc<WebApprovalHandler>,
    shared: Arc<SharedSessionState>,
    history: Vec<CoreMessage>,
    out_tx: mpsc::Sender<AguiEvent>,
) -> JoinHandle<()> {
    let mut builder = Agent::builder("arlo").instructions(config.instructions.clone());
    for tool in &config.tools {
        builder = builder.tool(tool.clone());
    }
    let agent = builder.build();

    let permissions = PermissionEngine::new(config.permission_mode);
    let run_config = RunConfig::builder(config.provider.clone(), &config.model)
        .permissions(permissions)
        .approval_handler(approval_handler as Arc<dyn agent_core::ApprovalHandler>)
        .task_store(config.task_store.clone())
        .build();

    let session_store = config.session_store.clone();
    let session_id = config.session_id.clone();

    tokio::spawn(async move {
        let input = Input::Items { messages: history };
        let stream = run_stream(&agent, input, &run_config);
        futures::pin_mut!(stream);
        let mut converter = AguiEventConverter::new();
        while let Some(event) = stream.next().await {
            if let RunEvent::AgentEnd { ref output, ref usage, .. } = event {
                let snapshot = {
                    let mut history = shared.history.lock().await;
                    history.push(CoreMessage::Assistant {
                        content: vec![ContentBlock::Text { text: output.clone() }],
                        usage: Some(usage.clone()),
                    });
                    history.clone()
                };
                if let Err(e) = session_store.save(&session_id, &snapshot).await {
                    tracing::warn!(error = %e, "failed to persist web session");
                }
            }
            for agui_event in converter.convert(event) {
                if out_tx.send(agui_event).await.is_err() {
                    return;
                }
            }
        }
    })
}

/// Drives one WebSocket connection for its entire lifetime.
pub async fn run_session(socket: WebSocket, config: WebServerConfig, shared: Arc<SharedSessionState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    let (out_tx, mut out_rx) = mpsc::channel::<AguiEvent>(256);
    let approval_handler = Arc::new(WebApprovalHandler::new(out_tx.clone()));

    let history_snapshot = shared.history.lock().await.clone();
    send_event(&mut ws_tx, AguiEvent::MessagesSnapshot { messages: history_snapshot }).await;
    send_task_and_todo_snapshot(&mut ws_tx, &config.task_store).await;

    let mut current_run: Option<JoinHandle<()>> = None;

    loop {
        tokio::select! {
            incoming = ws_rx.next() => {
                match incoming {
                    Some(Ok(WsMessage::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(ClientMessage::UserMessage { text }) => {
                                if let Some(handle) = current_run.take() {
                                    handle.abort();
                                    approval_handler.deny_all().await;
                                }
                                let history = {
                                    let mut h = shared.history.lock().await;
                                    h.push(CoreMessage::User { content: vec![ContentBlock::Text { text }] });
                                    h.clone()
                                };
                                let handle = spawn_run(&config, approval_handler.clone(), shared.clone(), history, out_tx.clone());
                                current_run = Some(handle);
                            }
                            Ok(ClientMessage::ApprovalResponse { responses }) => {
                                for r in responses {
                                    if let Some(mapped) = parse_decision(&r.decision, r.pattern) {
                                        approval_handler.resolve(&r.call_id, mapped).await;
                                    }
                                }
                            }
                            Ok(ClientMessage::Abort) => {
                                if let Some(handle) = current_run.take() {
                                    handle.abort();
                                    approval_handler.deny_all().await;
                                }
                            }
                            Err(e) => {
                                send_event(&mut ws_tx, AguiEvent::RunError {
                                    message: format!("malformed message: {e}"),
                                    code: Some("bad_request".to_string()),
                                }).await;
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            Some(event) = out_rx.recv() => {
                if !send_event(&mut ws_tx, event).await {
                    break;
                }
            }
        }
    }

    if let Some(handle) = current_run.take() {
        handle.abort();
    }
    approval_handler.deny_all().await;
}
