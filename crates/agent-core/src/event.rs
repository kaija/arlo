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
/// - Exactly one terminal event (AgentEnd, MaxTurns, Aborted, Error,
///   Interruption, or GuardrailTripped) closes the stream
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
