//! RunState: the fully serializable snapshot of a run's state.
//!
//! Enables pause/resume at any point by persisting the complete run state
//! to bytes via `serde_json`. Deserialization validates the schema version
//! and returns typed errors for malformed input or unrecognized versions.

use serde::{Deserialize, Serialize};

use crate::error::RunError;
use crate::message::{Message, Usage};
use crate::next_step::PendingApproval;

/// The current schema version for RunState serialization.
///
/// Follows semantic versioning (MAJOR.MINOR.PATCH).
pub const SCHEMA_VERSION: &str = "1.0.0";

/// Compaction state tracking for context compaction history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompactionState {
    /// Total number of compaction operations performed during this run.
    pub total_compactions: u32,
    /// Cumulative count of messages removed by compaction.
    pub messages_removed: usize,
    /// The turn number at which the last compaction occurred, if any.
    pub last_compaction_turn: Option<u32>,
    /// Number of consecutive compaction failures (for circuit breaker logic).
    pub consecutive_failures: u32,
    /// Whether the circuit breaker has tripped, disabling further compaction attempts.
    pub circuit_broken: bool,
    /// The last observed token count before compaction was attempted.
    pub last_token_count: Option<usize>,
}

/// The fully serializable snapshot of a run's state.
///
/// `RunState` captures everything needed to pause a run, persist it,
/// and resume later. It derives `Serialize`, `Deserialize`, and `PartialEq`
/// for byte-level persistence and equality checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunState {
    /// Unique identifier for this run.
    pub run_id: String,
    /// Optional session identifier for grouping related runs.
    pub session_id: Option<String>,
    /// The complete conversation history.
    pub messages: Vec<Message>,
    /// The current turn number (starts at 0, incremented each loop iteration).
    pub current_turn: u32,
    /// Optional maximum turn limit for this run.
    pub max_turns: Option<u32>,
    /// Accumulated total cost in USD for this run.
    pub total_cost_usd: f64,
    /// Accumulated total token usage across all turns.
    pub total_usage: Usage,
    /// Tool calls currently pending user approval.
    pub pending_approvals: Vec<PendingApproval>,
    /// Compaction tracking state.
    pub compaction_state: CompactionState,
    /// Trace identifier for distributed tracing/observability.
    pub trace_id: String,
    /// Schema version for forward/backward compatibility.
    pub schema_version: String,
}

impl RunState {
    /// Creates a new `RunState` with the given identifiers and turn limit.
    ///
    /// The `schema_version` is automatically set to the current version ("1.0.0").
    /// All accumulator fields (cost, usage, messages) start empty/zero.
    pub fn new(run_id: String, session_id: Option<String>, max_turns: Option<u32>) -> Self {
        Self {
            run_id,
            session_id,
            messages: Vec::new(),
            current_turn: 0,
            max_turns,
            total_cost_usd: 0.0,
            total_usage: Usage::default(),
            pending_approvals: Vec::new(),
            compaction_state: CompactionState::default(),
            trace_id: String::new(),
            schema_version: SCHEMA_VERSION.to_string(),
        }
    }

    /// Serializes this `RunState` to JSON bytes.
    ///
    /// Returns `Ok(Vec<u8>)` on success, or `Err(RunError::Serialization(_))` if
    /// serialization fails.
    pub fn serialize(&self) -> Result<Vec<u8>, RunError> {
        serde_json::to_vec(self).map_err(|e| RunError::Serialization(e.to_string()))
    }

    /// Deserializes a `RunState` from JSON bytes.
    ///
    /// Returns typed errors for:
    /// - Malformed JSON input → `RunError::Serialization` with parse error details
    /// - Unrecognized schema version → `RunError::Serialization` indicating version mismatch
    ///
    /// This method never panics regardless of input.
    pub fn deserialize(bytes: &[u8]) -> Result<RunState, RunError> {
        let state: RunState =
            serde_json::from_slice(bytes).map_err(|e| RunError::Serialization(e.to_string()))?;

        // Validate schema version
        if state.schema_version != SCHEMA_VERSION {
            return Err(RunError::Serialization(format!(
                "unrecognized schema version '{}', expected '{}'",
                state.schema_version, SCHEMA_VERSION
            )));
        }

        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, ToolUseBlock, Usage};
    use crate::next_step::PendingApproval;
    use serde_json::json;

    #[test]
    fn new_sets_schema_version() {
        let state = RunState::new("run-1".into(), None, Some(25));
        assert_eq!(state.schema_version, "1.0.0");
    }

    #[test]
    fn new_defaults_are_correct() {
        let state = RunState::new("run-1".into(), Some("sess-1".into()), Some(10));
        assert_eq!(state.run_id, "run-1");
        assert_eq!(state.session_id, Some("sess-1".to_string()));
        assert_eq!(state.max_turns, Some(10));
        assert_eq!(state.current_turn, 0);
        assert_eq!(state.total_cost_usd, 0.0);
        assert_eq!(state.total_usage, Usage::default());
        assert!(state.messages.is_empty());
        assert!(state.pending_approvals.is_empty());
        assert_eq!(state.compaction_state, CompactionState::default());
        assert_eq!(state.trace_id, "");
    }

    #[test]
    fn serialize_deserialize_roundtrip_empty() {
        let state = RunState::new("run-abc".into(), None, None);
        let bytes = state.serialize().unwrap();
        let restored = RunState::deserialize(&bytes).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn serialize_deserialize_roundtrip_with_messages() {
        let mut state = RunState::new("run-123".into(), Some("session-456".into()), Some(25));
        state.current_turn = 3;
        state.total_cost_usd = 0.0042;
        state.total_usage = Usage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: Some(200),
        };
        state.messages = vec![
            Message::System {
                content: "You are helpful.".to_string(),
            },
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "Hello!".to_string(),
                }],
            },
            Message::Assistant {
                content: vec![ContentBlock::ToolUse {
                    block: ToolUseBlock {
                        id: "tool-1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "/tmp/test.txt"}),
                    },
                }],
                usage: Some(Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: None,
                }),
            },
            Message::ToolResult {
                tool_use_id: "tool-1".to_string(),
                content: "file contents".to_string(),
                is_error: false,
            },
        ];
        state.pending_approvals = vec![PendingApproval {
            tool_name: "shell".to_string(),
            tool_input: json!({"command": "rm -rf /tmp"}),
            request_id: "req-001".to_string(),
        }];
        state.compaction_state = CompactionState {
            total_compactions: 2,
            messages_removed: 15,
            last_compaction_turn: Some(2),
            consecutive_failures: 0,
            circuit_broken: false,
            last_token_count: None,
        };
        state.trace_id = "trace-xyz-789".to_string();

        let bytes = state.serialize().unwrap();
        let restored = RunState::deserialize(&bytes).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn deserialize_malformed_bytes_returns_error() {
        let result = RunState::deserialize(b"not valid json at all");
        assert!(result.is_err());
        match result.unwrap_err() {
            RunError::Serialization(msg) => {
                assert!(!msg.is_empty());
            }
            other => panic!("expected Serialization error, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_empty_bytes_returns_error() {
        let result = RunState::deserialize(b"");
        assert!(result.is_err());
        match result.unwrap_err() {
            RunError::Serialization(_) => {}
            other => panic!("expected Serialization error, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_unrecognized_schema_version_returns_error() {
        let mut state = RunState::new("run-1".into(), None, None);
        state.schema_version = "99.0.0".to_string();
        let bytes = serde_json::to_vec(&state).unwrap();

        let result = RunState::deserialize(&bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            RunError::Serialization(msg) => {
                assert!(msg.contains("unrecognized schema version"));
                assert!(msg.contains("99.0.0"));
            }
            other => panic!("expected Serialization error, got {:?}", other),
        }
    }

    #[test]
    fn deserialize_valid_json_but_wrong_shape_returns_error() {
        let bytes = br#"{"some": "random", "json": true}"#;
        let result = RunState::deserialize(bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            RunError::Serialization(_) => {}
            other => panic!("expected Serialization error, got {:?}", other),
        }
    }

    #[test]
    fn compaction_state_default() {
        let cs = CompactionState::default();
        assert_eq!(cs.total_compactions, 0);
        assert_eq!(cs.messages_removed, 0);
        assert_eq!(cs.last_compaction_turn, None);
        assert_eq!(cs.consecutive_failures, 0);
        assert!(!cs.circuit_broken);
        assert_eq!(cs.last_token_count, None);
    }

    #[test]
    fn compaction_state_serialization_roundtrip() {
        let cs = CompactionState {
            total_compactions: 5,
            messages_removed: 42,
            last_compaction_turn: Some(10),
            consecutive_failures: 1,
            circuit_broken: false,
            last_token_count: Some(150000),
        };
        let json = serde_json::to_string(&cs).unwrap();
        let restored: CompactionState = serde_json::from_str(&json).unwrap();
        assert_eq!(cs, restored);
    }

    #[test]
    fn run_state_partial_eq_works() {
        let a = RunState::new("run-1".into(), None, Some(10));
        let b = RunState::new("run-1".into(), None, Some(10));
        assert_eq!(a, b);

        let c = RunState::new("run-2".into(), None, Some(10));
        assert_ne!(a, c);
    }

    #[test]
    fn run_state_with_none_session_and_max_turns() {
        let state = RunState::new("run-x".into(), None, None);
        assert_eq!(state.session_id, None);
        assert_eq!(state.max_turns, None);

        let bytes = state.serialize().unwrap();
        let restored = RunState::deserialize(&bytes).unwrap();
        assert_eq!(restored.session_id, None);
        assert_eq!(restored.max_turns, None);
    }
}
