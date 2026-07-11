//! Interactive approval handler for the TUI.
//!
//! Delegates approval requests from the agent run loop to the TUI event loop
//! via channels, enabling the user to interactively approve or deny tool calls.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex};

use agent_core::{ApprovalContext, ApprovalHandler, ApprovalResponse, PendingApproval};

/// A request sent from the agent run loop to the TUI for approval.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// The name of the agent requesting approval (for sub-agent context display).
    pub agent_name: Option<String>,
    /// The pending tool calls requiring user decisions.
    pub pending: Vec<PendingApproval>,
}

/// Interactive approval handler that delegates to the TUI.
///
/// Sends approval requests to the TUI event loop via a channel
/// and waits for the user's response. If either channel disconnects
/// (e.g., the TUI exits), all pending items are denied gracefully.
pub struct InteractiveApprovalHandler {
    request_tx: mpsc::Sender<ApprovalRequest>,
    response_rx: Arc<Mutex<mpsc::Receiver<Vec<ApprovalResponse>>>>,
}

impl InteractiveApprovalHandler {
    /// Create a new `InteractiveApprovalHandler`.
    ///
    /// # Arguments
    /// * `request_tx` — Sender for pushing approval requests to the TUI event loop.
    /// * `response_rx` — Receiver for getting the user's approval responses back.
    pub fn new(
        request_tx: mpsc::Sender<ApprovalRequest>,
        response_rx: mpsc::Receiver<Vec<ApprovalResponse>>,
    ) -> Self {
        Self {
            request_tx,
            response_rx: Arc::new(Mutex::new(response_rx)),
        }
    }
}

#[async_trait]
impl ApprovalHandler for InteractiveApprovalHandler {
    async fn request_approval(&self, context: &ApprovalContext) -> Vec<ApprovalResponse> {
        let request = ApprovalRequest {
            agent_name: context.agent_name.clone(),
            pending: context.pending.clone(),
        };

        // Send request to TUI event loop
        if self.request_tx.send(request).await.is_err() {
            // Channel disconnected — deny everything gracefully
            return context
                .pending
                .iter()
                .map(|_| ApprovalResponse::Deny)
                .collect();
        }

        // Wait for the user's response from the TUI
        let mut rx = self.response_rx.lock().await;
        match rx.recv().await {
            Some(responses) => responses,
            None => {
                // Channel disconnected — deny everything gracefully
                context
                    .pending
                    .iter()
                    .map(|_| ApprovalResponse::Deny)
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Helper to create a PendingApproval for tests.
    fn pending(tool: &str) -> PendingApproval {
        PendingApproval {
            tool_name: tool.to_string(),
            tool_input: json!({"command": "test"}),
            request_id: format!("req-{}", tool),
        }
    }

    #[tokio::test]
    async fn handler_sends_request_and_receives_response() {
        let (req_tx, mut req_rx) = mpsc::channel::<ApprovalRequest>(1);
        let (resp_tx, resp_rx) = mpsc::channel::<Vec<ApprovalResponse>>(1);

        let handler = InteractiveApprovalHandler::new(req_tx, resp_rx);

        let context = ApprovalContext {
            agent_name: Some("test-agent".to_string()),
            pending: vec![pending("Bash"), pending("fs_write")],
        };

        // Spawn the handler call in a task
        let handle = tokio::spawn(async move { handler.request_approval(&context).await });

        // The TUI side receives the request
        let received = req_rx.recv().await.unwrap();
        assert_eq!(received.agent_name, Some("test-agent".to_string()));
        assert_eq!(received.pending.len(), 2);
        assert_eq!(received.pending[0].tool_name, "Bash");
        assert_eq!(received.pending[1].tool_name, "fs_write");

        // The TUI side sends back a response
        resp_tx
            .send(vec![ApprovalResponse::Allow, ApprovalResponse::Deny])
            .await
            .unwrap();

        // The handler returns the response
        let result = handle.await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ApprovalResponse::Allow);
        assert_eq!(result[1], ApprovalResponse::Deny);
    }

    #[tokio::test]
    async fn handler_denies_all_when_request_channel_closed() {
        let (req_tx, req_rx) = mpsc::channel::<ApprovalRequest>(1);
        let (_resp_tx, resp_rx) = mpsc::channel::<Vec<ApprovalResponse>>(1);

        let handler = InteractiveApprovalHandler::new(req_tx, resp_rx);

        // Drop the receiver — simulates TUI exit
        drop(req_rx);

        let context = ApprovalContext {
            agent_name: None,
            pending: vec![pending("Bash"), pending("fs_read"), pending("fs_write")],
        };

        let result = handler.request_approval(&context).await;
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|r| *r == ApprovalResponse::Deny));
    }

    #[tokio::test]
    async fn handler_denies_all_when_response_channel_closed() {
        let (req_tx, mut req_rx) = mpsc::channel::<ApprovalRequest>(1);
        let (resp_tx, resp_rx) = mpsc::channel::<Vec<ApprovalResponse>>(1);

        let handler = InteractiveApprovalHandler::new(req_tx, resp_rx);

        let context = ApprovalContext {
            agent_name: Some("sub-agent".to_string()),
            pending: vec![pending("dangerous_tool")],
        };

        // Spawn handler
        let handle = tokio::spawn(async move { handler.request_approval(&context).await });

        // Receive the request on the TUI side
        let _received = req_rx.recv().await.unwrap();

        // Drop the response sender — simulates TUI crash/exit before responding
        drop(resp_tx);

        let result = handle.await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], ApprovalResponse::Deny);
    }

    #[tokio::test]
    async fn handler_with_always_allow_response() {
        let (req_tx, mut req_rx) = mpsc::channel::<ApprovalRequest>(1);
        let (resp_tx, resp_rx) = mpsc::channel::<Vec<ApprovalResponse>>(1);

        let handler = InteractiveApprovalHandler::new(req_tx, resp_rx);

        let context = ApprovalContext {
            agent_name: None,
            pending: vec![pending("Bash")],
        };

        let handle = tokio::spawn(async move { handler.request_approval(&context).await });

        // Receive and respond with AlwaysAllow
        let _received = req_rx.recv().await.unwrap();
        resp_tx
            .send(vec![ApprovalResponse::AlwaysAllow {
                pattern: "Bash(npm*)".to_string(),
            }])
            .await
            .unwrap();

        let result = handle.await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0],
            ApprovalResponse::AlwaysAllow {
                pattern: "Bash(npm*)".to_string()
            }
        );
    }

    #[tokio::test]
    async fn handler_with_no_agent_name() {
        let (req_tx, mut req_rx) = mpsc::channel::<ApprovalRequest>(1);
        let (resp_tx, resp_rx) = mpsc::channel::<Vec<ApprovalResponse>>(1);

        let handler = InteractiveApprovalHandler::new(req_tx, resp_rx);

        let context = ApprovalContext {
            agent_name: None,
            pending: vec![pending("shell")],
        };

        let handle = tokio::spawn(async move { handler.request_approval(&context).await });

        let received = req_rx.recv().await.unwrap();
        assert_eq!(received.agent_name, None);

        resp_tx.send(vec![ApprovalResponse::Allow]).await.unwrap();

        let result = handle.await.unwrap();
        assert_eq!(result[0], ApprovalResponse::Allow);
    }
}
