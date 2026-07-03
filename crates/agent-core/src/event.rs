//! Run event types and streaming type alias for the agent loop.
//!
//! `RunEvent` represents every observable event emitted by the RunLoop during
//! execution. `RunStream` is the streaming type alias for consuming these events.

use std::pin::Pin;

use futures::Stream;

use crate::message::Usage;
use crate::next_step::{NextStep, PendingApproval};
use crate::stream::StreamChunk;

/// An event emitted by the RunLoop during agent execution.
///
/// The stream guarantees:
/// - Exactly one terminal event (AgentEnd, MaxTurns, Aborted, or Error) closes the stream
/// - ToolStart always precedes the corresponding ToolEnd for the same tool id
/// - TurnStart is emitted at the start of each turn (turn numbers start at 1)
#[derive(Debug, Clone)]
pub enum RunEvent {
    /// Emitted at the start of each turn.
    TurnStart {
        /// The turn number (starting at 1).
        turn: u32,
        /// The name of the agent executing this turn.
        agent: String,
    },

    /// A streaming chunk from the model.
    StreamChunk(StreamChunk),

    /// A tool execution has started.
    ToolStart {
        /// The unique identifier for this tool invocation.
        id: String,
        /// The name of the tool being executed.
        name: String,
    },

    /// A tool execution has completed.
    ToolEnd {
        /// The unique identifier for this tool invocation.
        id: String,
        /// The name of the tool that was executed.
        name: String,
        /// The output produced by the tool (text representation).
        output: String,
        /// Whether the tool execution resulted in an error.
        is_error: bool,
    },

    /// A sub-agent has been spawned.
    SubAgentStart {
        /// The name of the sub-agent.
        agent: String,
        /// Description of what the sub-agent is working on.
        task: String,
    },

    /// A sub-agent has completed.
    SubAgentEnd {
        /// The name of the sub-agent.
        agent: String,
        /// The output produced by the sub-agent.
        output: String,
    },

    /// Context compaction was applied.
    Compaction {
        /// The compaction stage that was applied.
        stage: String,
        /// Number of messages removed or summarized.
        messages_removed: usize,
    },

    /// A NextStep resolution has been determined.
    StepResolved(NextStep),

    /// The agent has finished successfully. This is a terminal event.
    AgentEnd {
        /// The name of the agent that completed.
        agent: String,
        /// The final output text.
        output: String,
        /// Token usage statistics for the run.
        usage: Usage,
    },

    /// The run is paused awaiting user approval. This is a terminal event.
    Interruption {
        /// The tool calls pending approval.
        pending: Vec<PendingApproval>,
    },

    /// A guardrail check failed. This is a terminal event.
    GuardrailTripped {
        /// The name of the guardrail that triggered.
        name: String,
        /// The reason the guardrail was tripped.
        reason: String,
    },

    /// The agent reached its maximum turn limit. This is a terminal event.
    MaxTurns {
        /// The number of turns completed.
        count: u32,
    },

    /// The run was aborted. This is a terminal event.
    Aborted {
        /// The reason for the abort.
        reason: String,
    },

    /// An unrecoverable error occurred. This is a terminal event.
    Error {
        /// Description of the error.
        error: String,
    },
}

/// A stream of `RunEvent`s emitted by the RunLoop.
///
/// This is the primary interface for consumers to observe agent execution progress.
/// The stream will emit exactly one terminal event before completing.
pub type RunStream = Pin<Box<dyn Stream<Item = RunEvent> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_event_turn_start_debug_clone() {
        let event = RunEvent::TurnStart {
            turn: 1,
            agent: "main".to_string(),
        };
        let cloned = event.clone();
        let debug = format!("{:?}", cloned);
        assert!(debug.contains("TurnStart"));
        assert!(debug.contains("1"));
        assert!(debug.contains("main"));
    }

    #[test]
    fn run_event_stream_chunk() {
        let event = RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "hello".to_string(),
        });
        let cloned = event.clone();
        let debug = format!("{:?}", cloned);
        assert!(debug.contains("StreamChunk"));
        assert!(debug.contains("hello"));
    }

    #[test]
    fn run_event_tool_start_end() {
        let start = RunEvent::ToolStart {
            id: "tool_123".to_string(),
            name: "read_file".to_string(),
        };
        let end = RunEvent::ToolEnd {
            id: "tool_123".to_string(),
            name: "read_file".to_string(),
            output: "file contents".to_string(),
            is_error: false,
        };
        // Both should be cloneable and debuggable
        let _ = start.clone();
        let _ = end.clone();
        let debug = format!("{:?}", start);
        assert!(debug.contains("ToolStart"));
        assert!(debug.contains("tool_123"));
    }

    #[test]
    fn run_event_sub_agent() {
        let start = RunEvent::SubAgentStart {
            agent: "researcher".to_string(),
            task: "find relevant papers".to_string(),
        };
        let end = RunEvent::SubAgentEnd {
            agent: "researcher".to_string(),
            output: "Found 3 papers".to_string(),
        };
        let _ = start.clone();
        let _ = end.clone();
    }

    #[test]
    fn run_event_compaction() {
        let event = RunEvent::Compaction {
            stage: "snip".to_string(),
            messages_removed: 5,
        };
        let debug = format!("{:?}", event.clone());
        assert!(debug.contains("Compaction"));
        assert!(debug.contains("5"));
    }

    #[test]
    fn run_event_step_resolved() {
        let event = RunEvent::StepResolved(NextStep::MaxTurns { count: 25 });
        let debug = format!("{:?}", event.clone());
        assert!(debug.contains("StepResolved"));
        assert!(debug.contains("25"));
    }

    #[test]
    fn run_event_agent_end() {
        let event = RunEvent::AgentEnd {
            agent: "main".to_string(),
            output: "Task completed.".to_string(),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 500,
                cache_read_tokens: Some(200),
            },
        };
        let debug = format!("{:?}", event.clone());
        assert!(debug.contains("AgentEnd"));
        assert!(debug.contains("1000"));
    }

    #[test]
    fn run_event_interruption() {
        let event = RunEvent::Interruption {
            pending: vec![PendingApproval {
                tool_name: "shell".to_string(),
                tool_input: serde_json::json!({"command": "ls"}),
                request_id: "req-1".to_string(),
            }],
        };
        let debug = format!("{:?}", event.clone());
        assert!(debug.contains("Interruption"));
        assert!(debug.contains("shell"));
    }

    #[test]
    fn run_event_guardrail_tripped() {
        let event = RunEvent::GuardrailTripped {
            name: "content_filter".to_string(),
            reason: "harmful content detected".to_string(),
        };
        let debug = format!("{:?}", event.clone());
        assert!(debug.contains("GuardrailTripped"));
        assert!(debug.contains("content_filter"));
    }

    #[test]
    fn run_event_max_turns() {
        let event = RunEvent::MaxTurns { count: 30 };
        let debug = format!("{:?}", event.clone());
        assert!(debug.contains("MaxTurns"));
        assert!(debug.contains("30"));
    }

    #[test]
    fn run_event_aborted() {
        let event = RunEvent::Aborted {
            reason: "budget_exceeded".to_string(),
        };
        let debug = format!("{:?}", event.clone());
        assert!(debug.contains("Aborted"));
        assert!(debug.contains("budget_exceeded"));
    }

    #[test]
    fn run_event_error() {
        let event = RunEvent::Error {
            error: "unrecoverable model failure".to_string(),
        };
        let debug = format!("{:?}", event.clone());
        assert!(debug.contains("Error"));
        assert!(debug.contains("unrecoverable"));
    }

    #[test]
    fn run_event_all_variants_are_send() {
        // Compile-time check that RunEvent is Send (required for RunStream)
        fn assert_send<T: Send>() {}
        assert_send::<RunEvent>();
    }

    #[test]
    fn run_stream_type_is_valid() {
        // Compile-time check that RunStream is a valid type
        #[allow(dead_code)]
        fn assert_stream_type<T: Stream<Item = RunEvent> + Send>() {}
        // This verifies the type alias compiles correctly
        #[allow(dead_code)]
        fn _takes_run_stream(_s: RunStream) {}
    }
}
