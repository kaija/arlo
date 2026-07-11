//! `ApprovalHandler` implementation for the web UI: correlates each pending
//! tool call with its own `callId` and `oneshot` channel so concurrent
//! approval requests (parent run + background sub-agents) resolve
//! independently, instead of racing on one shared response channel the way
//! `tui/approval.rs::InteractiveApprovalHandler` does today. See
//! docs/superpowers/specs/2026-07-11-web-ui-design.md's Addendum.

use std::collections::HashMap;
use std::sync::Arc;

use agent_core::{ApprovalContext, ApprovalHandler, ApprovalResponse};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::{mpsc, oneshot, Mutex};
use uuid::Uuid;

use super::agui::AguiEvent;

pub struct WebApprovalHandler {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<ApprovalResponse>>>>,
    out_tx: mpsc::Sender<AguiEvent>,
}

impl WebApprovalHandler {
    pub fn new(out_tx: mpsc::Sender<AguiEvent>) -> Self {
        Self { pending: Arc::new(Mutex::new(HashMap::new())), out_tx }
    }

    /// Called by `session.rs` when an `approval_response` client message
    /// names this `call_id`. Unknown/stale ids (already answered, or left
    /// over after a takeover-closed connection) are silently ignored.
    pub async fn resolve(&self, call_id: &str, response: ApprovalResponse) {
        if let Some(tx) = self.pending.lock().await.remove(call_id) {
            let _ = tx.send(response);
        }
    }

    /// Deny every currently pending approval. Called by `session.rs` when
    /// the WebSocket connection closes (or a run is aborted) so a blocked
    /// `request_approval()` caller doesn't hang forever.
    pub async fn deny_all(&self) {
        let mut pending = self.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(ApprovalResponse::Deny);
        }
    }
}

#[async_trait]
impl ApprovalHandler for WebApprovalHandler {
    async fn request_approval(&self, context: &ApprovalContext) -> Vec<ApprovalResponse> {
        let mut receivers = Vec::with_capacity(context.pending.len());
        for item in &context.pending {
            let call_id = Uuid::new_v4().to_string();
            let (tx, rx) = oneshot::channel();
            self.pending.lock().await.insert(call_id.clone(), tx);

            let sent = self
                .out_tx
                .send(AguiEvent::Custom {
                    name: "arlo.permission_request".to_string(),
                    value: json!({
                        "callId": call_id,
                        "agentName": context.agent_name,
                        "toolName": item.tool_name,
                        "toolInput": item.tool_input,
                        "options": ["allow_once", "allow_always", "reject_once", "reject_always"],
                    }),
                })
                .await
                .is_ok();
            receivers.push((call_id, rx, sent));
        }

        let mut responses = Vec::with_capacity(receivers.len());
        for (call_id, rx, sent) in receivers {
            let response = if sent {
                rx.await.unwrap_or(ApprovalResponse::Deny)
            } else {
                // out_tx already closed — the connection is gone. Don't wait;
                // deny immediately and drop our own now-orphaned registration.
                self.pending.lock().await.remove(&call_id);
                ApprovalResponse::Deny
            };
            responses.push(response);
        }
        responses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::PendingApproval;
    use serde_json::Value;

    fn pending(tool: &str) -> PendingApproval {
        PendingApproval {
            tool_name: tool.to_string(),
            tool_input: json!({"command": "test"}),
            request_id: format!("approval-{tool}"),
        }
    }

    fn call_id_of(event: &AguiEvent) -> String {
        match event {
            AguiEvent::Custom { name, value } if name == "arlo.permission_request" => {
                value["callId"].as_str().unwrap().to_string()
            }
            other => panic!("expected arlo.permission_request Custom event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multiple_pending_items_round_trip_in_order() {
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let handler = Arc::new(WebApprovalHandler::new(out_tx));
        let ctx = ApprovalContext {
            agent_name: None,
            pending: vec![pending("shell"), pending("file_write")],
        };

        let handler_for_task = handler.clone();
        let request = tokio::spawn(async move { handler_for_task.request_approval(&ctx).await });

        let first_event = out_rx.recv().await.unwrap();
        let second_event = out_rx.recv().await.unwrap();
        let first_id = call_id_of(&first_event);
        let second_id = call_id_of(&second_event);

        // Resolve out of order: second item first.
        handler.resolve(&second_id, ApprovalResponse::Deny).await;
        handler.resolve(&first_id, ApprovalResponse::Allow).await;

        let responses = request.await.unwrap();
        // Order matches context.pending order, not resolution order.
        assert_eq!(responses, vec![ApprovalResponse::Allow, ApprovalResponse::Deny]);
    }

    #[tokio::test]
    async fn always_allow_response_round_trips() {
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let handler = Arc::new(WebApprovalHandler::new(out_tx));
        let ctx = ApprovalContext { agent_name: Some("sub-agent".to_string()), pending: vec![pending("shell")] };

        let handler_for_task = handler.clone();
        let request = tokio::spawn(async move { handler_for_task.request_approval(&ctx).await });
        let event = out_rx.recv().await.unwrap();
        let call_id = call_id_of(&event);

        // agentName should be present on the wire event for sub-agent context.
        if let AguiEvent::Custom { value, .. } = &event {
            assert_eq!(value["agentName"], Value::String("sub-agent".to_string()));
        }

        handler
            .resolve(&call_id, ApprovalResponse::AlwaysAllow { pattern: "Bash(npm*)".to_string() })
            .await;
        let responses = request.await.unwrap();
        assert_eq!(responses, vec![ApprovalResponse::AlwaysAllow { pattern: "Bash(npm*)".to_string() }]);
    }

    #[tokio::test]
    async fn deny_all_resolves_pending_requests_with_deny() {
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let handler = Arc::new(WebApprovalHandler::new(out_tx));
        let ctx = ApprovalContext { agent_name: None, pending: vec![pending("shell")] };

        let handler_for_task = handler.clone();
        let request = tokio::spawn(async move { handler_for_task.request_approval(&ctx).await });
        let _event = out_rx.recv().await.unwrap();

        handler.deny_all().await;
        let responses = request.await.unwrap();
        assert_eq!(responses, vec![ApprovalResponse::Deny]);
    }

    #[tokio::test]
    async fn out_tx_closed_before_send_denies_immediately() {
        let (out_tx, out_rx) = mpsc::channel(4);
        drop(out_rx); // simulate connection already gone
        let handler = WebApprovalHandler::new(out_tx);
        let ctx = ApprovalContext { agent_name: None, pending: vec![pending("shell"), pending("file_write")] };

        let responses = handler.request_approval(&ctx).await;
        assert_eq!(responses, vec![ApprovalResponse::Deny, ApprovalResponse::Deny]);
    }

    #[tokio::test]
    async fn concurrent_requests_resolve_independently() {
        // The scenario tui/approval.rs cannot handle: a parent-run approval
        // and a background sub-agent's approval in flight at the same time.
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let handler = Arc::new(WebApprovalHandler::new(out_tx));

        let parent_ctx = ApprovalContext { agent_name: None, pending: vec![pending("shell")] };
        let sub_ctx = ApprovalContext { agent_name: Some("api-check".to_string()), pending: vec![pending("web_fetch")] };

        let h1 = handler.clone();
        let parent_request = tokio::spawn(async move { h1.request_approval(&parent_ctx).await });
        let h2 = handler.clone();
        let sub_request = tokio::spawn(async move { h2.request_approval(&sub_ctx).await });

        let event_a = out_rx.recv().await.unwrap();
        let event_b = out_rx.recv().await.unwrap();
        let id_a = call_id_of(&event_a);
        let id_b = call_id_of(&event_b);
        assert_ne!(id_a, id_b);

        // Resolve the sub-agent's request first; the parent's must still be pending.
        handler.resolve(&id_b, ApprovalResponse::Allow).await;
        handler.resolve(&id_a, ApprovalResponse::Deny).await;

        assert_eq!(parent_request.await.unwrap(), vec![ApprovalResponse::Deny]);
        assert_eq!(sub_request.await.unwrap(), vec![ApprovalResponse::Allow]);
    }

    #[tokio::test]
    async fn unknown_call_id_is_ignored() {
        let (out_tx, mut out_rx) = mpsc::channel(4);
        let handler = Arc::new(WebApprovalHandler::new(out_tx));
        let ctx = ApprovalContext { agent_name: None, pending: vec![pending("shell")] };

        let handler_for_task = handler.clone();
        let request = tokio::spawn(async move { handler_for_task.request_approval(&ctx).await });
        let event = out_rx.recv().await.unwrap();
        let real_id = call_id_of(&event);

        handler.resolve("does-not-exist", ApprovalResponse::Allow).await;
        handler.resolve(&real_id, ApprovalResponse::Allow).await;

        assert_eq!(request.await.unwrap(), vec![ApprovalResponse::Allow]);
    }
}
