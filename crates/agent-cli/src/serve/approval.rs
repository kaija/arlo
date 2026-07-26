//! Custom `ApprovalHandler` for the AG-UI HITL flow.

use std::sync::Arc;

use ag_ui_protocol::{ResumeEntry, ResumeStatus};
use agent_core::config::{ApprovalContext, ApprovalHandler, ApprovalResponse};
use agent_core::next_step::PendingApproval;
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use super::session::SessionStore;

/// Approval handler that parks the agent run on a oneshot channel, signals
/// the event mapper to emit an AG-UI `Interrupt`, and waits for the next
/// POST to deliver decisions.
pub struct AgUiApprovalHandler {
    pub sessions: Arc<SessionStore>,
    pub thread_id: String,
    pub interrupt_tx: mpsc::Sender<Vec<PendingApproval>>,
}

#[async_trait]
impl ApprovalHandler for AgUiApprovalHandler {
    async fn request_approval(&self, context: &ApprovalContext) -> Vec<ApprovalResponse> {
        let pending = context.pending.clone();
        let count = pending.len();

        // 1. Create oneshot channel for receiving resume decisions
        let (tx, rx) = oneshot::channel();

        // 2. Register interrupt state in SessionStore (stores tx, pending approvals)
        self.sessions
            .mark_interrupted(&self.thread_id, pending.clone(), tx);

        // 3. Signal interrupt_tx so the event mapper knows to emit RUN_FINISHED(interrupt)
        let _ = self.interrupt_tx.send(pending).await;

        // 4. Block on rx.await — parks the run until the next POST delivers decisions
        // 5. Deny all on channel drop (safe fallback)
        rx.await
            .unwrap_or_else(|_| vec![ApprovalResponse::Deny; count])
    }
}

/// Map AG-UI `ResumeEntry` values to Arlo `ApprovalResponse` variants.
///
/// - `Resolved` + payload `{"action":"always_allow","pattern":"..."}` → `AlwaysAllow`
/// - `Resolved` (otherwise) → `Allow`
/// - `Cancelled` → `Deny`
pub fn map_resume_to_responses(resume: &[ResumeEntry]) -> Vec<ApprovalResponse> {
    resume.iter().map(map_single_resume).collect()
}

fn map_single_resume(entry: &ResumeEntry) -> ApprovalResponse {
    match entry.status {
        ResumeStatus::Cancelled => ApprovalResponse::Deny,
        ResumeStatus::Resolved => {
            // Check payload for always_allow action
            if let Some(ref payload) = entry.payload {
                if let Some(action) = payload.get("action").and_then(|v| v.as_str()) {
                    if action == "always_allow" {
                        let pattern = payload
                            .get("pattern")
                            .and_then(|v| v.as_str())
                            .unwrap_or("*")
                            .to_string();
                        return ApprovalResponse::AlwaysAllow { pattern };
                    }
                    if action == "deny" || action == "reject" {
                        return ApprovalResponse::Deny;
                    }
                }
            }
            ApprovalResponse::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    // -- map_resume_to_responses tests -----------------------------------------

    #[test]
    fn resolved_maps_to_allow() {
        let entries = vec![ResumeEntry {
            interrupt_id: "i1".into(),
            status: ResumeStatus::Resolved,
            payload: None,
        }];
        let responses = map_resume_to_responses(&entries);
        assert_eq!(responses, vec![ApprovalResponse::Allow]);
    }

    #[test]
    fn cancelled_maps_to_deny() {
        let entries = vec![ResumeEntry {
            interrupt_id: "i1".into(),
            status: ResumeStatus::Cancelled,
            payload: None,
        }];
        let responses = map_resume_to_responses(&entries);
        assert_eq!(responses, vec![ApprovalResponse::Deny]);
    }

    #[test]
    fn resolved_with_always_allow_payload() {
        let entries = vec![ResumeEntry {
            interrupt_id: "i1".into(),
            status: ResumeStatus::Resolved,
            payload: Some(json!({"action": "always_allow", "pattern": "shell(ls*)"})),
        }];
        let responses = map_resume_to_responses(&entries);
        assert_eq!(
            responses,
            vec![ApprovalResponse::AlwaysAllow {
                pattern: "shell(ls*)".into()
            }]
        );
    }

    #[test]
    fn resolved_with_always_allow_no_pattern_defaults_to_star() {
        let entries = vec![ResumeEntry {
            interrupt_id: "i1".into(),
            status: ResumeStatus::Resolved,
            payload: Some(json!({"action": "always_allow"})),
        }];
        let responses = map_resume_to_responses(&entries);
        assert_eq!(
            responses,
            vec![ApprovalResponse::AlwaysAllow {
                pattern: "*".into()
            }]
        );
    }

    #[test]
    fn resolved_with_deny_action_in_payload() {
        let entries = vec![ResumeEntry {
            interrupt_id: "i1".into(),
            status: ResumeStatus::Resolved,
            payload: Some(json!({"action": "deny"})),
        }];
        let responses = map_resume_to_responses(&entries);
        assert_eq!(responses, vec![ApprovalResponse::Deny]);
    }

    #[test]
    fn multiple_entries_mixed() {
        let entries = vec![
            ResumeEntry {
                interrupt_id: "i1".into(),
                status: ResumeStatus::Resolved,
                payload: None,
            },
            ResumeEntry {
                interrupt_id: "i2".into(),
                status: ResumeStatus::Cancelled,
                payload: None,
            },
            ResumeEntry {
                interrupt_id: "i3".into(),
                status: ResumeStatus::Resolved,
                payload: Some(json!({"action": "always_allow", "pattern": "fs_*"})),
            },
        ];
        let responses = map_resume_to_responses(&entries);
        assert_eq!(
            responses,
            vec![
                ApprovalResponse::Allow,
                ApprovalResponse::Deny,
                ApprovalResponse::AlwaysAllow {
                    pattern: "fs_*".into()
                },
            ]
        );
    }

    // -- ApprovalHandler integration tests -------------------------------------

    #[tokio::test]
    async fn approval_parks_and_resumes() {
        let store = Arc::new(SessionStore::new());
        let (interrupt_tx, mut interrupt_rx) = mpsc::channel(1);

        // Insert a dummy running session so mark_interrupted has something to update
        use super::super::session::{RunSession, SessionState};
        use std::time::Instant;
        store.insert(
            "t1".into(),
            RunSession {
                run_id: "r1".into(),
                state: SessionState::Running,
                created_at: Instant::now(),
                last_active: Instant::now(),
                resume_tx: None,
                task_handle: tokio::spawn(async {}),
            },
        );

        let handler = AgUiApprovalHandler {
            sessions: Arc::clone(&store),
            thread_id: "t1".into(),
            interrupt_tx,
        };

        let context = ApprovalContext {
            agent_name: None,
            pending: vec![PendingApproval {
                tool_name: "shell".into(),
                tool_input: json!({"cmd": "ls"}),
                request_id: "req-1".into(),
            }],
        };

        // Spawn the approval request — it will park on the oneshot
        let handle = tokio::spawn(async move { handler.request_approval(&context).await });

        // The interrupt signal should arrive
        let pending = interrupt_rx.recv().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name, "shell");

        // Session should now be interrupted with a resume_tx
        let session = store.remove("t1").unwrap();
        assert!(matches!(session.state, SessionState::Interrupted { .. }));

        // Send the resume decision
        session
            .resume_tx
            .unwrap()
            .send(vec![ApprovalResponse::Allow])
            .unwrap();

        // The handler should unblock and return the decision
        let result = handle.await.unwrap();
        assert_eq!(result, vec![ApprovalResponse::Allow]);
    }

    #[tokio::test]
    async fn channel_drop_returns_deny() {
        let store = Arc::new(SessionStore::new());
        let (interrupt_tx, _interrupt_rx) = mpsc::channel(1);

        use super::super::session::{RunSession, SessionState};
        use std::time::Instant;
        store.insert(
            "t2".into(),
            RunSession {
                run_id: "r2".into(),
                state: SessionState::Running,
                created_at: Instant::now(),
                last_active: Instant::now(),
                resume_tx: None,
                task_handle: tokio::spawn(async {}),
            },
        );

        let handler = AgUiApprovalHandler {
            sessions: Arc::clone(&store),
            thread_id: "t2".into(),
            interrupt_tx,
        };

        let context = ApprovalContext {
            agent_name: None,
            pending: vec![
                PendingApproval {
                    tool_name: "shell".into(),
                    tool_input: json!({"cmd": "rm"}),
                    request_id: "req-1".into(),
                },
                PendingApproval {
                    tool_name: "file_write".into(),
                    tool_input: json!({"path": "/tmp/x"}),
                    request_id: "req-2".into(),
                },
            ],
        };

        let handle = tokio::spawn(async move { handler.request_approval(&context).await });

        // Drop the session (and thus the resume_tx oneshot sender)
        store.remove("t2");

        // The handler should get Deny for all pending
        let result = handle.await.unwrap();
        assert_eq!(result, vec![ApprovalResponse::Deny, ApprovalResponse::Deny]);
    }

    // -- Property-based tests --------------------------------------------------

    // Feature: ag-ui-server, Property 5: Interrupt round-trip preserves approval semantics
    // **Validates: Requirements 4.1, 4.3, 4.5**

    /// The "action" variants a resolved resume entry can carry.
    #[derive(Debug, Clone)]
    enum ResumeAction {
        /// No payload at all → Allow
        None,
        /// Resolved with an unrecognized or empty payload → Allow
        ArbitraryPayload(String),
        /// `{"action": "always_allow", "pattern": "..."}` → AlwaysAllow
        AlwaysAllow(String),
        /// `{"action": "always_allow"}` (no pattern key) → AlwaysAllow { pattern: "*" }
        AlwaysAllowNoPattern,
        /// `{"action": "deny"}` → Deny
        DenyAction,
    }

    fn arb_resume_action() -> impl Strategy<Value = ResumeAction> {
        prop_oneof![
            Just(ResumeAction::None),
            "[a-z0-9_*]{1,20}".prop_map(ResumeAction::ArbitraryPayload),
            "[a-z0-9_*()]{1,30}".prop_map(ResumeAction::AlwaysAllow),
            Just(ResumeAction::AlwaysAllowNoPattern),
            Just(ResumeAction::DenyAction),
        ]
    }

    fn arb_resume_entry() -> impl Strategy<Value = (ResumeEntry, ApprovalResponse)> {
        // Generate status: true = Resolved, false = Cancelled
        (any::<bool>(), arb_resume_action(), "[a-z0-9]{1,10}").prop_map(
            |(is_resolved, action, id)| {
                if !is_resolved {
                    let entry = ResumeEntry {
                        interrupt_id: id,
                        status: ResumeStatus::Cancelled,
                        payload: None,
                    };
                    (entry, ApprovalResponse::Deny)
                } else {
                    let (payload, expected) = match action {
                        ResumeAction::None => (None, ApprovalResponse::Allow),
                        ResumeAction::ArbitraryPayload(val) => {
                            (Some(json!({ "note": val })), ApprovalResponse::Allow)
                        }
                        ResumeAction::AlwaysAllow(pat) => (
                            Some(json!({"action": "always_allow", "pattern": pat})),
                            ApprovalResponse::AlwaysAllow { pattern: pat },
                        ),
                        ResumeAction::AlwaysAllowNoPattern => (
                            Some(json!({"action": "always_allow"})),
                            ApprovalResponse::AlwaysAllow {
                                pattern: "*".into(),
                            },
                        ),
                        ResumeAction::DenyAction => {
                            (Some(json!({"action": "deny"})), ApprovalResponse::Deny)
                        }
                    };
                    let entry = ResumeEntry {
                        interrupt_id: id,
                        status: ResumeStatus::Resolved,
                        payload,
                    };
                    (entry, expected)
                }
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_interrupt_round_trip_preserves_semantics(
            pairs in proptest::collection::vec(arb_resume_entry(), 1..20)
        ) {
            let (entries, expected): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();

            let actual = map_resume_to_responses(&entries);

            // Length preserved
            prop_assert_eq!(actual.len(), expected.len());

            // Each mapping correct
            for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
                prop_assert_eq!(
                    a, e,
                    "mismatch at index {}: entry status={:?}, payload={:?}",
                    i, entries[i].status, entries[i].payload
                );
            }
        }
    }
}
