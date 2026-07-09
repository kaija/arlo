//! NextStep state machine enum controlling the RunLoop's control flow.
//!
//! Each variant encodes exactly what the loop should do next after resolving
//! the model response, tool results, and agent configuration.

use serde::{Deserialize, Serialize};

/// The discriminated enum governing RunLoop state transitions.
///
/// After each turn, the loop resolves a `NextStep` to decide whether to continue,
/// output a final result, pause for approval, attempt recovery, or terminate.
#[derive(Debug, Clone, PartialEq)]
pub enum NextStep {
    /// Continue the loop with another turn (tools were called and executed).
    Continue,

    /// The agent produced a final output and the loop should terminate successfully.
    FinalOutput {
        text: String,
        structured: Option<serde_json::Value>,
    },

    /// The loop is paused waiting for user approval of one or more tool calls.
    Interruption { pending: Vec<PendingApproval> },

    /// An error was encountered and a recovery strategy should be attempted.
    Recovery { strategy: RecoveryStrategy },

    /// The agent has reached its configured maximum turn limit.
    MaxTurns { count: u32 },

    /// The run was aborted for the given reason.
    Aborted { reason: String },
}

/// A pending tool call awaiting user approval before execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// Name of the tool requesting approval.
    pub tool_name: String,
    /// The input arguments for the tool call.
    pub tool_input: serde_json::Value,
    /// Unique identifier for the approval request.
    pub request_id: String,
}

/// Strategy for recovering from an error encountered during the run.
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryStrategy {
    /// Compact the message history and retry the model call.
    CompactAndRetry,
    /// Increase the max output token limit and retry.
    EscalateOutputTokens { max: u32 },
    /// Append a continuation prompt and retry with the given attempt number.
    ContinueMessage { attempt: u32 },
    /// Give up — the error is unrecoverable.
    GiveUp { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_approval_serialization_roundtrip() {
        let approval = PendingApproval {
            tool_name: "file_write".to_string(),
            tool_input: serde_json::json!({"path": "/tmp/test.txt", "content": "hello"}),
            request_id: "abc-def-123".to_string(),
        };
        let json = serde_json::to_string(&approval).unwrap();
        let deserialized: PendingApproval = serde_json::from_str(&json).unwrap();
        assert_eq!(approval, deserialized);
    }
}
