//! Run configuration, input, and result types for the agent framework.
//!
//! Defines `RunConfig` (with builder pattern), `Input` enum for starting runs,
//! `RunResult` for completed runs, and the `ApprovalHandler` trait for
//! interactive permission prompts.

use std::sync::Arc;

use async_trait::async_trait;

use crate::message::{Message, Usage};
use crate::model::ModelProvider;
use crate::next_step::PendingApproval;
use crate::permission::{PermissionEngine, PermissionMode};
use crate::state::RunState;

/// Trait for handling interactive approval prompts during tool execution.
///
/// When the permission engine determines a tool call requires user approval,
/// it delegates to an `ApprovalHandler` to present the request and collect
/// the user's decision.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Request approval for one or more pending tool calls.
    ///
    /// Returns a `Vec<bool>` of the same length as `pending`, where `true`
    /// means the corresponding tool call is approved and `false` means rejected.
    async fn request_approval(&self, pending: &[PendingApproval]) -> Vec<bool>;
}

/// Configuration for a single agent run.
///
/// Created via the builder pattern: `RunConfig::builder(provider, model)`.
#[derive(Clone)]
pub struct RunConfig {
    /// The model provider used to resolve model names to usable instances.
    pub provider: Arc<dyn ModelProvider>,
    /// The model name to use for this run.
    pub model: String,
    /// Maximum output tokens for model responses.
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature (validated to be within 0.0–2.0).
    pub temperature: Option<f32>,
    /// Maximum number of concurrent Safe tool executions.
    pub concurrency_limit: u32,
    /// Maximum number of turns before the run is terminated.
    pub max_turns: u32,
    /// Optional budget in USD; run aborts if exceeded.
    pub budget_usd: Option<f64>,
    /// Optional handler for interactive approval prompts.
    pub approval_handler: Option<Arc<dyn ApprovalHandler>>,
    /// Permission engine for tool call authorization.
    pub permissions: PermissionEngine,
}

impl std::fmt::Debug for RunConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunConfig")
            .field("model", &self.model)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("temperature", &self.temperature)
            .field("concurrency_limit", &self.concurrency_limit)
            .field("max_turns", &self.max_turns)
            .field("budget_usd", &self.budget_usd)
            .field("approval_handler", &self.approval_handler.is_some())
            .field("permissions", &self.permissions)
            .finish()
    }
}

impl RunConfig {
    /// Create a new `RunConfigBuilder` with required fields.
    ///
    /// # Arguments
    /// * `provider` — The model provider for resolving model names.
    /// * `model` — The model name string to use for this run.
    pub fn builder(provider: Arc<dyn ModelProvider>, model: impl Into<String>) -> RunConfigBuilder {
        RunConfigBuilder {
            provider,
            model: model.into(),
            max_output_tokens: None,
            temperature: None,
            concurrency_limit: 8,
            max_turns: 25,
            budget_usd: None,
            approval_handler: None,
            permissions: PermissionEngine::new(PermissionMode::Bypass),
        }
    }
}

/// Builder for constructing a `RunConfig` with validation.
pub struct RunConfigBuilder {
    provider: Arc<dyn ModelProvider>,
    model: String,
    max_output_tokens: Option<u32>,
    temperature: Option<f32>,
    concurrency_limit: u32,
    max_turns: u32,
    budget_usd: Option<f64>,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
    permissions: PermissionEngine,
}

impl RunConfigBuilder {
    /// Set the sampling temperature.
    ///
    /// # Panics
    /// Panics if `temperature` is outside the range [0.0, 2.0].
    pub fn temperature(mut self, temperature: f32) -> Self {
        assert!(
            (0.0..=2.0).contains(&temperature),
            "temperature must be between 0.0 and 2.0, got {}",
            temperature
        );
        self.temperature = Some(temperature);
        self
    }

    /// Set the maximum output tokens for model responses.
    pub fn max_output_tokens(mut self, max_output_tokens: u32) -> Self {
        self.max_output_tokens = Some(max_output_tokens);
        self
    }

    /// Set the concurrency limit for parallel tool execution.
    pub fn concurrency_limit(mut self, concurrency_limit: u32) -> Self {
        self.concurrency_limit = concurrency_limit;
        self
    }

    /// Set the maximum number of turns before the run terminates.
    pub fn max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// Set the budget in USD. The run will abort if cost exceeds this value.
    pub fn budget_usd(mut self, budget_usd: f64) -> Self {
        self.budget_usd = Some(budget_usd);
        self
    }

    /// Set the approval handler for interactive permission prompts.
    pub fn approval_handler(mut self, handler: Arc<dyn ApprovalHandler>) -> Self {
        self.approval_handler = Some(handler);
        self
    }

    /// Set the permission engine for tool call authorization.
    pub fn permissions(mut self, permissions: PermissionEngine) -> Self {
        self.permissions = permissions;
        self
    }

    /// Consume the builder and produce a `RunConfig`.
    pub fn build(self) -> RunConfig {
        RunConfig {
            provider: self.provider,
            model: self.model,
            max_output_tokens: self.max_output_tokens,
            temperature: self.temperature,
            concurrency_limit: self.concurrency_limit,
            max_turns: self.max_turns,
            budget_usd: self.budget_usd,
            approval_handler: self.approval_handler,
            permissions: self.permissions,
        }
    }
}

/// Input for starting or resuming an agent run.
#[derive(Debug, Clone)]
pub enum Input {
    /// Start a fresh conversation with the given prompt.
    Fresh { prompt: String },
    /// Provide specific messages as the conversation history.
    Items { messages: Vec<Message> },
    /// Resume a run from a previously serialized state.
    Resume { state: RunState },
}

/// The result of a completed agent run.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// The final text output from the agent.
    pub output: String,
    /// Optional structured (JSON) output if the agent produced one.
    pub structured: Option<serde_json::Value>,
    /// Token usage statistics for the entire run.
    pub usage: Usage,
    /// Total monetary cost of the run in USD.
    pub cost_usd: f64,
    /// Number of turns executed during the run.
    pub turns: u32,
    /// The final run state (can be serialized for later resumption).
    pub state: RunState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ModelError;
    use crate::model::Model;
    use proptest::prelude::*;
    use serde_json::json;
    use std::panic;

    /// A minimal mock ModelProvider for testing.
    struct MockProvider;

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn resolve(
            &self,
            _model_name: &str,
        ) -> Result<Arc<dyn Model>, ModelError> {
            Err(ModelError::Connection("mock provider".to_string()))
        }

        fn available_models(&self) -> Vec<String> {
            vec!["mock-model".to_string()]
        }
    }

    /// A minimal mock ApprovalHandler for testing.
    struct MockApprovalHandler;

    #[async_trait]
    impl ApprovalHandler for MockApprovalHandler {
        async fn request_approval(&self, pending: &[PendingApproval]) -> Vec<bool> {
            // Approve everything
            pending.iter().map(|_| true).collect()
        }
    }

    fn mock_provider() -> Arc<dyn ModelProvider> {
        Arc::new(MockProvider)
    }

    #[test]
    fn run_config_builder_defaults() {
        let config = RunConfig::builder(mock_provider(), "test-model").build();
        assert_eq!(config.model, "test-model");
        assert_eq!(config.max_output_tokens, None);
        assert_eq!(config.temperature, None);
        assert_eq!(config.concurrency_limit, 8);
        assert_eq!(config.max_turns, 25);
        assert_eq!(config.budget_usd, None);
        assert!(config.approval_handler.is_none());
    }

    #[test]
    fn run_config_builder_all_fields() {
        let handler: Arc<dyn ApprovalHandler> = Arc::new(MockApprovalHandler);
        let config = RunConfig::builder(mock_provider(), "claude-sonnet-4-20250514")
            .temperature(0.7)
            .max_output_tokens(4096)
            .concurrency_limit(4)
            .max_turns(50)
            .budget_usd(1.5)
            .approval_handler(handler)
            .build();

        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.max_output_tokens, Some(4096));
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.concurrency_limit, 4);
        assert_eq!(config.max_turns, 50);
        assert_eq!(config.budget_usd, Some(1.5));
        assert!(config.approval_handler.is_some());
    }

    #[test]
    fn run_config_builder_temperature_zero() {
        let config = RunConfig::builder(mock_provider(), "model")
            .temperature(0.0)
            .build();
        assert_eq!(config.temperature, Some(0.0));
    }

    #[test]
    fn run_config_builder_temperature_max() {
        let config = RunConfig::builder(mock_provider(), "model")
            .temperature(2.0)
            .build();
        assert_eq!(config.temperature, Some(2.0));
    }

    #[test]
    #[should_panic(expected = "temperature must be between 0.0 and 2.0")]
    fn run_config_builder_temperature_too_high() {
        RunConfig::builder(mock_provider(), "model")
            .temperature(2.1)
            .build();
    }

    #[test]
    #[should_panic(expected = "temperature must be between 0.0 and 2.0")]
    fn run_config_builder_temperature_negative() {
        RunConfig::builder(mock_provider(), "model")
            .temperature(-0.1)
            .build();
    }

    #[test]
    #[should_panic(expected = "temperature must be between 0.0 and 2.0")]
    fn run_config_builder_temperature_very_high() {
        RunConfig::builder(mock_provider(), "model")
            .temperature(100.0)
            .build();
    }

    #[test]
    fn run_config_debug() {
        let config = RunConfig::builder(mock_provider(), "model")
            .temperature(1.0)
            .build();
        let debug = format!("{:?}", config);
        assert!(debug.contains("RunConfig"));
        assert!(debug.contains("model"));
        assert!(debug.contains("1.0"));
    }

    #[test]
    fn input_fresh_variant() {
        let input = Input::Fresh {
            prompt: "Hello, agent!".to_string(),
        };
        let cloned = input.clone();
        let debug = format!("{:?}", cloned);
        assert!(debug.contains("Fresh"));
        assert!(debug.contains("Hello, agent!"));
    }

    #[test]
    fn input_items_variant() {
        let input = Input::Items {
            messages: vec![Message::User {
                content: vec![crate::message::ContentBlock::Text {
                    text: "test".to_string(),
                }],
            }],
        };
        let cloned = input.clone();
        let debug = format!("{:?}", cloned);
        assert!(debug.contains("Items"));
    }

    #[test]
    fn input_resume_variant() {
        use crate::state::RunState;
        let state = RunState::new("run-1".to_string(), None, None);
        let input = Input::Resume { state };
        let cloned = input.clone();
        let debug = format!("{:?}", cloned);
        assert!(debug.contains("Resume"));
    }

    #[test]
    fn run_result_construction() {
        use crate::state::RunState;
        let result = RunResult {
            output: "Done!".to_string(),
            structured: Some(json!({"status": "ok"})),
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 500,
                cache_read_tokens: Some(200),
            },
            cost_usd: 0.0045,
            turns: 3,
            state: RunState::new("run-1".to_string(), None, None),
        };
        assert_eq!(result.output, "Done!");
        assert_eq!(result.turns, 3);
        assert_eq!(result.cost_usd, 0.0045);
        assert!(result.structured.is_some());
    }

    #[test]
    fn run_result_debug() {
        use crate::state::RunState;
        let result = RunResult {
            output: "Result text".to_string(),
            structured: None,
            usage: Usage::default(),
            cost_usd: 0.0,
            turns: 1,
            state: RunState::new("r".to_string(), None, None),
        };
        let debug = format!("{:?}", result);
        assert!(debug.contains("RunResult"));
        assert!(debug.contains("Result text"));
    }

    #[tokio::test]
    async fn approval_handler_trait_object_safety() {
        let handler: Arc<dyn ApprovalHandler> = Arc::new(MockApprovalHandler);
        let pending = vec![PendingApproval {
            tool_name: "shell".to_string(),
            tool_input: json!({"command": "ls"}),
            request_id: "req-1".to_string(),
        }];
        let decisions = handler.request_approval(&pending).await;
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0]);
    }

    #[tokio::test]
    async fn approval_handler_multiple_pending() {
        let handler: Arc<dyn ApprovalHandler> = Arc::new(MockApprovalHandler);
        let pending = vec![
            PendingApproval {
                tool_name: "shell".to_string(),
                tool_input: json!({"command": "rm -rf /tmp/test"}),
                request_id: "req-1".to_string(),
            },
            PendingApproval {
                tool_name: "file_write".to_string(),
                tool_input: json!({"path": "/etc/hosts"}),
                request_id: "req-2".to_string(),
            },
        ];
        let decisions = handler.request_approval(&pending).await;
        assert_eq!(decisions.len(), 2);
    }

    #[test]
    fn approval_handler_send_sync_bounds() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<dyn ApprovalHandler>>();
    }

    // Feature: rust-agent-framework, Property 20: Temperature validation
    // **Validates: Requirements 22.3**
    //
    // For any floating-point value outside the range [0.0, 2.0], the RunConfig
    // builder shall reject the temperature configuration (panic). Values within
    // [0.0, 2.0] shall be accepted without panicking.

    proptest! {
        #[test]
        fn prop_temperature_valid_values_accepted(temp in 0.0f32..=2.0f32) {
            let provider = mock_provider();
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                RunConfig::builder(provider, "test-model")
                    .temperature(temp)
                    .build()
            }));
            prop_assert!(result.is_ok(),
                "Temperature {} within [0.0, 2.0] should be accepted", temp);
            let config = result.unwrap();
            prop_assert_eq!(config.temperature, Some(temp));
        }

        #[test]
        fn prop_temperature_above_range_rejected(temp in 2.001f32..1000.0f32) {
            let provider = mock_provider();
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                RunConfig::builder(provider, "test-model")
                    .temperature(temp)
                    .build()
            }));
            prop_assert!(result.is_err(),
                "Temperature {} above 2.0 should be rejected", temp);
        }

        #[test]
        fn prop_temperature_below_range_rejected(temp in -1000.0f32..-0.001f32) {
            let provider = mock_provider();
            let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                RunConfig::builder(provider, "test-model")
                    .temperature(temp)
                    .build()
            }));
            prop_assert!(result.is_err(),
                "Temperature {} below 0.0 should be rejected", temp);
        }
    }

    #[test]
    fn prop_temperature_nan_rejected() {
        let provider = mock_provider();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            RunConfig::builder(provider, "test-model")
                .temperature(f32::NAN)
                .build()
        }));
        assert!(result.is_err(), "NaN temperature should be rejected");
    }

    #[test]
    fn prop_temperature_positive_infinity_rejected() {
        let provider = mock_provider();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            RunConfig::builder(provider, "test-model")
                .temperature(f32::INFINITY)
                .build()
        }));
        assert!(result.is_err(), "Positive infinity temperature should be rejected");
    }

    #[test]
    fn prop_temperature_negative_infinity_rejected() {
        let provider = mock_provider();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            RunConfig::builder(provider, "test-model")
                .temperature(f32::NEG_INFINITY)
                .build()
        }));
        assert!(result.is_err(), "Negative infinity temperature should be rejected");
    }
}
