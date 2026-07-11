//! agent-llm: Unified LLM provider abstraction with feature-flag-gated backends.
//!
//! This crate provides a single `UnifiedProvider` that implements the
//! `ModelProvider` trait from `agent-core`, routing model requests to the
//! appropriate backend (OpenAI, Anthropic, Ollama) based on model name
//! prefixes and feature flags.
//!
//! # Feature Flags
//!
//! - `openai` (default): Enable OpenAI API backend
//! - `anthropic` (default): Enable Anthropic Messages API backend
//! - `ollama` (optional): Enable local Ollama server backend
//! - `all-providers`: Enable all provider backends

pub mod anthropic_http;
pub mod convert;
pub mod model_override;
pub mod openai_http;
pub mod provider;
pub mod retry;

pub use agent_core;
pub use anthropic_http::AnthropicHttpModel;
pub use model_override::ModelOverrideWrapper;
pub use openai_http::OpenAIHttpModel;
pub use provider::UnifiedProvider;
pub use retry::RetryConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_provider_is_exported() {
        // Verify the type is accessible from the crate root
        let _: fn() -> Option<String> = || {
            let _provider_type = std::any::type_name::<UnifiedProvider>();
            None
        };
    }
}
