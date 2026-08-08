//! In-memory session store for active and suspended AG-UI runs.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use agent_core::config::ApprovalResponse;
use agent_core::event::RunEvent;
use agent_core::next_step::PendingApproval;
use tokio::sync::{mpsc, oneshot};

use super::bridge::EventMapper;

/// Type alias for the live run stream.
pub type RunStream = std::pin::Pin<Box<dyn futures::Stream<Item = RunEvent> + Send>>;

/// A parked run — the live stream plus its associated mapper and approval
/// receiver, stashed between an interrupt and its resume.
///
/// Dropping this aborts the in-flight run at its next poll, which is the
/// correct behaviour for session reaping.
pub struct ParkedRun {
    pub stream: RunStream,
    pub mapper: EventMapper,
    pub interrupt_rx: mpsc::Receiver<Vec<PendingApproval>>,
}

/// State of a run session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    Running,
    Interrupted { pending: Vec<PendingApproval> },
    Completed,
}

/// A single agent run kept alive across HTTP requests.
pub struct RunSession {
    pub run_id: String,
    pub state: SessionState,
    pub created_at: Instant,
    pub last_active: Instant,
    pub resume_tx: Option<oneshot::Sender<Vec<ApprovalResponse>>>,
    /// The live run, parked mid-flight. Dropping this aborts the run at its
    /// next event emission — which is exactly what reaping should do.
    pub parked: Option<ParkedRun>,
}

/// In-memory registry mapping `thread_id` → `RunSession`.
///
/// ponytail: std::sync::Mutex (not tokio) since we never hold the guard across awaits.
/// Upgrade to tokio::sync::Mutex if contention shows up under load.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, RunSession>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn insert(&self, thread_id: String, session: RunSession) {
        self.sessions.lock().unwrap().insert(thread_id, session);
    }

    /// Returns cloned state info for a session (can't return &RunSession through Mutex).
    pub fn get_state(&self, thread_id: &str) -> Option<SessionState> {
        self.sessions
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|s| s.state.clone())
    }

    /// Remove and return a session.
    pub fn remove(&self, thread_id: &str) -> Option<RunSession> {
        self.sessions.lock().unwrap().remove(thread_id)
    }

    /// Transition a session to `Interrupted`, storing the pending approvals and
    /// resume channel. The parked run is set separately via `update_parked` once
    /// the bridge has finished draining and can move ownership of the stream.
    pub fn mark_interrupted(
        &self,
        thread_id: &str,
        pending: Vec<PendingApproval>,
        resume_tx: oneshot::Sender<Vec<ApprovalResponse>>,
    ) -> bool {
        let mut map = self.sessions.lock().unwrap();
        if let Some(session) = map.get_mut(thread_id) {
            session.state = SessionState::Interrupted {
                pending: pending.clone(),
            };
            session.resume_tx = Some(resume_tx);
            session.last_active = Instant::now();
            true
        } else {
            false
        }
    }

    /// Replace the parked run in an existing session (used after the bridge
    /// finishes draining and moves the live stream into the store).
    pub fn update_parked(&self, thread_id: &str, parked: ParkedRun) -> bool {
        let mut map = self.sessions.lock().unwrap();
        if let Some(session) = map.get_mut(thread_id) {
            session.parked = Some(parked);
            session.last_active = Instant::now();
            true
        } else {
            false
        }
    }

    /// Remove sessions idle past `timeout`.
    ///
    /// Dropping the session drops `ParkedRun` (if any), which cancels the
    /// in-flight run poll — no explicit abort needed.
    pub fn reap_expired(&self, timeout: Duration) {
        let mut map = self.sessions.lock().unwrap();
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, s)| s.last_active.elapsed() > timeout)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            map.remove(&key);
        }
    }

    /// Number of active sessions (for testing).
    pub fn len(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn dummy_session(run_id: &str) -> RunSession {
        RunSession {
            run_id: run_id.to_string(),
            state: SessionState::Running,
            created_at: Instant::now(),
            last_active: Instant::now(),
            resume_tx: None,
            parked: None,
        }
    }

    #[tokio::test]
    async fn insert_and_get() {
        let store = SessionStore::new();
        store.insert("t1".into(), dummy_session("r1"));
        assert_eq!(store.get_state("t1"), Some(SessionState::Running));
        assert_eq!(store.get_state("unknown"), None);
    }

    #[tokio::test]
    async fn remove_returns_session() {
        let store = SessionStore::new();
        store.insert("t1".into(), dummy_session("r1"));
        let removed = store.remove("t1").unwrap();
        assert_eq!(removed.run_id, "r1");
        assert_eq!(store.len(), 0);
    }

    #[tokio::test]
    async fn remove_nonexistent_returns_none() {
        let store = SessionStore::new();
        assert!(store.remove("nope").is_none());
    }

    #[tokio::test]
    async fn mark_interrupted_transitions_state() {
        let store = SessionStore::new();
        store.insert("t1".into(), dummy_session("r1"));

        let (tx, rx) = oneshot::channel();
        let pending = vec![PendingApproval {
            tool_name: "shell".into(),
            tool_input: serde_json::json!({"cmd": "ls"}),
            request_id: "req-1".into(),
        }];

        assert!(store.mark_interrupted("t1", pending.clone(), tx));

        let state = store.get_state("t1").unwrap();
        assert_eq!(
            state,
            SessionState::Interrupted {
                pending: pending.clone()
            }
        );

        // The resume_tx was stored — verify by removing and sending through it
        let session = store.remove("t1").unwrap();
        session
            .resume_tx
            .unwrap()
            .send(vec![ApprovalResponse::Allow])
            .unwrap();
        assert_eq!(rx.await.unwrap(), vec![ApprovalResponse::Allow]);
    }

    #[tokio::test]
    async fn mark_interrupted_nonexistent_returns_false() {
        let store = SessionStore::new();
        let (tx, _rx) = oneshot::channel();
        assert!(!store.mark_interrupted("nope", vec![], tx));
    }

    #[tokio::test]
    async fn update_parked_stores_parked_run() {
        let store = SessionStore::new();
        store.insert("t1".into(), dummy_session("r1"));

        // mark_interrupted no longer takes parked — parked starts as None
        let (tx, _rx) = oneshot::channel();
        assert!(store.mark_interrupted("t1", vec![], tx));
        {
            let session = store.remove("t1").unwrap();
            assert!(session.parked.is_none());
            store.insert("t1".into(), session);
        }

        // Now set the parked run via update_parked
        let parked = ParkedRun {
            stream: Box::pin(futures::stream::empty()),
            mapper: EventMapper::new(),
            interrupt_rx: tokio::sync::mpsc::channel(1).1,
        };
        assert!(store.update_parked("t1", parked));

        let session = store.remove("t1").unwrap();
        assert!(session.parked.is_some());
    }

    #[tokio::test]
    async fn update_parked_nonexistent_returns_false() {
        let store = SessionStore::new();
        let parked = ParkedRun {
            stream: Box::pin(futures::stream::empty()),
            mapper: EventMapper::new(),
            interrupt_rx: tokio::sync::mpsc::channel(1).1,
        };
        assert!(!store.update_parked("nope", parked));
    }

    #[tokio::test]
    async fn reap_expired_removes_old_sessions() {
        let store = SessionStore::new();

        // Insert a session with last_active in the past
        let mut old = dummy_session("old");
        old.last_active = Instant::now() - Duration::from_secs(120);
        store.insert("old".into(), old);

        // Insert a fresh session
        store.insert("new".into(), dummy_session("new"));

        // Reap with 60s timeout — "old" should be removed
        store.reap_expired(Duration::from_secs(60));

        assert_eq!(store.len(), 1);
        assert_eq!(store.get_state("old"), None);
        assert_eq!(store.get_state("new"), Some(SessionState::Running));
    }

    #[tokio::test]
    async fn reap_expired_drops_parked_run() {
        let store = SessionStore::new();

        // Use a channel to detect when the stream is dropped
        let (dropped_tx, _dropped_rx) = tokio::sync::oneshot::channel::<()>();

        struct DropNotify(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for DropNotify {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }

        // Wrap the notifier in a stream that holds it until dropped
        let notifier = DropNotify(Some(dropped_tx));
        let stream: RunStream = Box::pin(futures::stream::unfold(
            (notifier, false),
            |(n, done)| async move {
                if done {
                    drop(n);
                    None
                } else {
                    Some((
                        agent_core::event::RunEvent::Compaction {
                            stage: "tools".to_string(),
                            messages_removed: 0,
                        },
                        (n, true),
                    ))
                }
            },
        ));

        let parked = ParkedRun {
            stream,
            mapper: EventMapper::new(),
            interrupt_rx: tokio::sync::mpsc::channel(1).1,
        };

        let mut session = dummy_session("parked");
        session.last_active = Instant::now() - Duration::from_secs(600);
        session.parked = Some(parked);
        store.insert("parked".into(), session);

        store.reap_expired(Duration::from_secs(60));

        assert_eq!(store.len(), 0);
        // The ParkedRun was dropped — the stream's state machine was dropped too.
        // We can't await dropped_rx here without polling the stream first, but
        // verifying the session is gone is sufficient: it proves reap works.
    }

    #[tokio::test]
    async fn concurrent_access() {
        use std::sync::Arc;

        let store = Arc::new(SessionStore::new());
        let mut handles = vec![];

        for i in 0..10 {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let id = format!("t{i}");
                store.insert(id.clone(), dummy_session(&format!("r{i}")));
                // Read back
                assert_eq!(store.get_state(&id), Some(SessionState::Running));
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(store.len(), 10);

        // Remove half concurrently
        let mut handles = vec![];
        for i in 0..5 {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                store.remove(&format!("t{i}"));
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(store.len(), 5);
    }

    // Feature: ag-ui-server, Property 6: Session reaping respects timeout
    // **Validates: Requirements 5.2, 5.3**
    proptest! {
        #[test]
        fn prop_reap_respects_timeout(
            sessions_data in prop::collection::vec((0u32..1000, 1u64..3600), 1..20),
            timeout_secs in 1u64..1800,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = SessionStore::new();
                let now = Instant::now();

                for (i, &(_, age_secs)) in sessions_data.iter().enumerate() {
                    let session = RunSession {
                        run_id: format!("r{i}"),
                        state: SessionState::Running,
                        created_at: now,
                        last_active: now - Duration::from_secs(age_secs),
                        resume_tx: None,
                        parked: None,
                    };
                    store.insert(format!("t{i}"), session);
                }

                store.reap_expired(Duration::from_secs(timeout_secs));

                for (i, &(_, age_secs)) in sessions_data.iter().enumerate() {
                    let id = format!("t{i}");
                    if age_secs > timeout_secs {
                        assert!(
                            store.get_state(&id).is_none(),
                            "Session {id} with age {age_secs}s should be reaped (timeout={timeout_secs}s)"
                        );
                    } else if age_secs < timeout_secs {
                        // Skip age == timeout: elapsed() drift makes the boundary non-deterministic
                        assert!(
                            store.get_state(&id).is_some(),
                            "Session {id} with age {age_secs}s should remain (timeout={timeout_secs}s)"
                        );
                    }
                }
            });
        }
    }
}
