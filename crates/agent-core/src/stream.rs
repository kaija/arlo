//! Canonical streaming chunk types for normalized model output.
//!
//! `StreamChunk` represents a single piece of streaming model output,
//! normalized from provider-specific formats before reaching the main loop.

use serde::{Deserialize, Serialize};

use crate::message::Usage;

/// A single chunk of streaming model output.
///
/// Provider-specific streaming formats are converted to this canonical
/// representation before being processed by the RunLoop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StreamChunk {
    /// A delta of generated text content.
    TextDelta { text: String },

    /// A delta of model thinking/reasoning content.
    ThinkingDelta { text: String },

    /// Signals the start of a tool use block.
    ToolUseStart { id: String, name: String },

    /// A delta of tool use input JSON being streamed.
    ToolUseInputDelta { id: String, delta: String },

    /// Signals the end of a tool use block with the complete parsed input.
    ToolUseEnd { id: String, input: serde_json::Value },

    /// Signals the end of the model's message with a stop reason and usage.
    MessageStop {
        stop_reason: StopReason,
        usage: Usage,
    },
}

/// The reason the model stopped generating output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// The model naturally finished its turn.
    EndTurn,

    /// The model wants to invoke one or more tools.
    ToolUse,

    /// The model hit the maximum output token limit.
    MaxTokens,

    /// The model encountered a stop sequence.
    StopSequence,

    /// The output was filtered by content safety.
    ContentFilter,
}
