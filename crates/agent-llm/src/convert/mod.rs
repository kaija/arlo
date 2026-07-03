//! Per-provider message format converters.
//!
//! Each submodule converts canonical `Message` types to/from
//! provider-specific wire format (JSON). Modules are gated behind
//! their respective feature flags.

use thiserror::Error;

/// Errors that can occur during message format conversion.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum ConvertError {
    /// A required field was missing in the wire format.
    #[error("Missing field '{field}' in {context}")]
    MissingField { field: String, context: String },

    /// A field had an unexpected type or value.
    #[error("Invalid value for '{field}' in {context}: {detail}")]
    InvalidValue {
        field: String,
        context: String,
        detail: String,
    },

    /// The role string was unrecognized.
    #[error("Unknown role: {0}")]
    UnknownRole(String),

    /// The content block type was unrecognized.
    #[error("Unknown content block type: {0}")]
    UnknownBlockType(String),
}

#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "anthropic")]
pub mod anthropic;

#[cfg(feature = "ollama")]
pub mod ollama;
