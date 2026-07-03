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
    /// Continue the loop with another turn.
    Continue { reason: ContinueReason },

    /// The agent produced a final output and the loop should terminate successfully.
    FinalOutput {
        text: String,
        structured: Option<serde_json::Value>,
    },

    /// The loop is paused waiting for user approval of one or more tool calls.
    Interruption { pending: Vec<PendingApproval> },

    /// An error was encountered and a recovery strategy should be attempted.
    Recovery { strategy: RecoveryStrategy },

    /// Budget-aware continuation: the agent may continue but with limited turns remaining.
    BudgetContinue { remaining_turns: u32, reason: String },

    /// The agent has reached its configured maximum turn limit.
    MaxTurns { count: u32 },

    /// The run was aborted for the given reason.
    Aborted { reason: String },
}

/// Reason the loop is continuing for another turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContinueReason {
    /// The model invoked one or more tools and the loop needs to process results.
    ToolUse,
    /// The model produced a partial response that requires continuation.
    PartialResponse,
    /// The model is handing off to a sub-agent or skill.
    Handoff,
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
    /// Try a different model as a fallback.
    FallbackModel { model: String },
    /// Give up — the error is unrecoverable.
    GiveUp { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_step_continue_debug_clone_partialeq() {
        let step = NextStep::Continue {
            reason: ContinueReason::ToolUse,
        };
        let cloned = step.clone();
        assert_eq!(step, cloned);
        // Debug format should contain the variant name
        let debug = format!("{:?}", step);
        assert!(debug.contains("Continue"));
        assert!(debug.contains("ToolUse"));
    }

    #[test]
    fn next_step_final_output() {
        let step = NextStep::FinalOutput {
            text: "Hello, world!".to_string(),
            structured: Some(serde_json::json!({"key": "value"})),
        };
        let cloned = step.clone();
        assert_eq!(step, cloned);
    }

    #[test]
    fn next_step_interruption() {
        let step = NextStep::Interruption {
            pending: vec![PendingApproval {
                tool_name: "shell".to_string(),
                tool_input: serde_json::json!({"command": "rm -rf /"}),
                request_id: "req-123".to_string(),
            }],
        };
        let cloned = step.clone();
        assert_eq!(step, cloned);
    }

    #[test]
    fn next_step_recovery_variants() {
        let strategies = vec![
            RecoveryStrategy::CompactAndRetry,
            RecoveryStrategy::EscalateOutputTokens { max: 8192 },
            RecoveryStrategy::ContinueMessage { attempt: 2 },
            RecoveryStrategy::FallbackModel {
                model: "gpt-4".to_string(),
            },
            RecoveryStrategy::GiveUp {
                error: "unrecoverable".to_string(),
            },
        ];

        for strategy in strategies {
            let step = NextStep::Recovery {
                strategy: strategy.clone(),
            };
            assert_eq!(step.clone(), step);
        }
    }

    #[test]
    fn next_step_budget_continue() {
        let step = NextStep::BudgetContinue {
            remaining_turns: 5,
            reason: "approaching limit".to_string(),
        };
        assert_eq!(step.clone(), step);
    }

    #[test]
    fn next_step_max_turns() {
        let step = NextStep::MaxTurns { count: 25 };
        assert_eq!(step.clone(), step);
        let debug = format!("{:?}", step);
        assert!(debug.contains("25"));
    }

    #[test]
    fn next_step_aborted() {
        let step = NextStep::Aborted {
            reason: "budget_exceeded".to_string(),
        };
        assert_eq!(step.clone(), step);
        let debug = format!("{:?}", step);
        assert!(debug.contains("budget_exceeded"));
    }

    #[test]
    fn continue_reason_serialization_roundtrip() {
        let reasons = vec![
            ContinueReason::ToolUse,
            ContinueReason::PartialResponse,
            ContinueReason::Handoff,
        ];
        for reason in reasons {
            let json = serde_json::to_string(&reason).unwrap();
            let deserialized: ContinueReason = serde_json::from_str(&json).unwrap();
            assert_eq!(reason, deserialized);
        }
    }

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

    #[test]
    fn next_step_inequality() {
        let a = NextStep::MaxTurns { count: 10 };
        let b = NextStep::MaxTurns { count: 20 };
        assert_ne!(a, b);

        let c = NextStep::Continue {
            reason: ContinueReason::ToolUse,
        };
        let d = NextStep::Continue {
            reason: ContinueReason::Handoff,
        };
        assert_ne!(c, d);
    }
}
