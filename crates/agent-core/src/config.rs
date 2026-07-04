//! Run configuration, input, and result types for the agent framework.
//!
//! Defines `RunConfig` (with builder pattern), `Input` enum for starting runs,
//! `RunResult` for completed runs, and the `ApprovalHandler` trait for
//! interactive permission prompts.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::message::{Message, Usage};
use crate::model::ModelProvider;
use crate::next_step::PendingApproval;
use crate::permission::{PermissionEngine, PermissionMode};
use crate::settings::{PolicyMerger, SettingsLoader};
use crate::state::RunState;

/// The user's decision for a single pending approval request.
///
/// Each variant encodes a different level of permission grant:
/// - `Allow`: approve this single invocation
/// - `Deny`: reject this single invocation
/// - `AlwaysAllow`: approve and remember a pattern-based session grant
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResponse {
    /// Approve this single tool invocation.
    Allow,
    /// Reject this single tool invocation.
    Deny,
    /// Approve and register a session-wide pattern grant so future matching
    /// calls are auto-approved without prompting again.
    AlwaysAllow { pattern: String },
}

/// Context passed to an `ApprovalHandler` when requesting approval.
///
/// Contains information about which agent is requesting approval and
/// the list of pending tool calls awaiting a decision.
#[derive(Debug, Clone)]
pub struct ApprovalContext {
    /// The name of the agent requesting approval, if it is a sub-agent.
    /// `None` for the top-level agent.
    pub agent_name: Option<String>,
    /// The pending tool calls requiring approval decisions.
    pub pending: Vec<PendingApproval>,
}

/// Trait for handling interactive approval prompts during tool execution.
///
/// When the permission engine determines a tool call requires user approval,
/// it delegates to an `ApprovalHandler` to present the request and collect
/// the user's decision.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Request approval for one or more pending tool calls.
    ///
    /// Returns a `Vec<ApprovalResponse>` of the same length as `context.pending`,
    /// encoding the user's decision for each pending tool call.
    async fn request_approval(&self, context: &ApprovalContext) -> Vec<ApprovalResponse>;
}

/// An `ApprovalHandler` that unconditionally denies every pending approval.
///
/// Used in non-interactive mode (CI/CD, piped stdin, `--non-interactive` flag)
/// to prevent the run loop from blocking on user input that will never arrive.
/// Each denied tool call is logged at `warn` level for observability.
pub struct DenyAllApprovalHandler;

#[async_trait]
impl ApprovalHandler for DenyAllApprovalHandler {
    async fn request_approval(&self, context: &ApprovalContext) -> Vec<ApprovalResponse> {
        for pending in &context.pending {
            tracing::warn!(
                tool = %pending.tool_name,
                agent = ?context.agent_name,
                "Tool call denied: no interactive approval handler available"
            );
        }
        context.pending.iter().map(|_| ApprovalResponse::Deny).collect()
    }
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
    /// Optional name identifying this agent (used in `ApprovalContext` for sub-agent identification).
    /// `None` for the top-level agent; `Some(name)` for sub-agents.
    pub agent_name: Option<String>,
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
            .field("agent_name", &self.agent_name)
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
            agent_name: None,
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
    agent_name: Option<String>,
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

    /// Set the agent name for sub-agent identification in approval contexts.
    ///
    /// When set, the name is included in `ApprovalContext` so the approval handler
    /// can distinguish which agent is requesting permission.
    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = Some(name.into());
        self
    }

    /// Load user-level and project-level settings files and merge them into the permission engine.
    ///
    /// This method is opt-in: if not called, no settings files are loaded and
    /// the permission engine retains its default (or manually configured) state.
    ///
    /// Settings are loaded from:
    /// - User-level: `~/.arlo/settings.json`
    /// - Project-level: `{working_dir}/.arlo/settings.json`
    ///
    /// The merged policy is applied to the permission engine's static allow/deny lists.
    /// Runtime rules (via `with_static_allow`/`with_static_deny`) should be applied
    /// separately through the existing API after this call if needed.
    pub fn load_settings(mut self, working_dir: &Path) -> Self {
        // Load user-level settings (if home dir is available and file exists)
        let user_settings = match SettingsLoader::user_path() {
            Some(path) => SettingsLoader::load(&path),
            None => Default::default(),
        };

        // Load project-level settings
        let project_path = SettingsLoader::project_path(working_dir);
        let project_settings = SettingsLoader::load(&project_path);

        // Merge user + project (runtime rules are applied separately via existing API)
        let policy = PolicyMerger::merge(&user_settings, &project_settings, &[], &[]);

        // Apply merged policy to permissions
        self.permissions = self.permissions.with_merged_policy(policy);
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
            agent_name: self.agent_name,
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
        async fn request_approval(&self, context: &ApprovalContext) -> Vec<ApprovalResponse> {
            // Approve everything
            context.pending.iter().map(|_| ApprovalResponse::Allow).collect()
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
        let context = ApprovalContext {
            agent_name: None,
            pending: pending.clone(),
        };
        let decisions = handler.request_approval(&context).await;
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0], ApprovalResponse::Allow);
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
        let context = ApprovalContext {
            agent_name: Some("sub-agent".to_string()),
            pending: pending.clone(),
        };
        let decisions = handler.request_approval(&context).await;
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

    // ===================================================================
    // Settings integration tests (Task 5.2)
    // Validates: Requirements 3.5, 9.6
    // ===================================================================

    #[test]
    fn load_settings_no_call_has_empty_policy() {
        // When load_settings is never called, the permission engine should have
        // empty static allow/deny lists (no file loading by default).
        // Use Normal mode so static rules are actually evaluated.
        let config = RunConfig::builder(mock_provider(), "test-model")
            .permissions(PermissionEngine::new(PermissionMode::Normal))
            .build();

        // In Normal mode with no static rules, a tool with Never approval requirement
        // should be allowed (falls through to Layer 4 which allows Never).
        let decision = config.permissions.check(
            "any_tool",
            &crate::tool::ApprovalRequirement::Never,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::Allow { .. }),
            "Without load_settings, tools should pass through (no static deny rules)"
        );

        // A tool that requires approval should get NeedsApproval (not statically denied/allowed).
        let decision = config.permissions.check(
            "some_tool",
            &crate::tool::ApprovalRequirement::Always,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::NeedsApproval { .. }),
            "Without load_settings, no static allow rules → falls through to approval requirement"
        );
    }

    #[test]
    fn load_settings_missing_files_empty_policy() {
        // Point to a temp directory with no .arlo/settings.json file.
        // Should produce empty policy (no panics, no errors).
        let tmp = tempfile::TempDir::new().unwrap();

        let config = RunConfig::builder(mock_provider(), "test-model")
            .permissions(PermissionEngine::new(PermissionMode::Normal))
            .load_settings(tmp.path())
            .build();

        // Same behavior as no load_settings call — empty static rules.
        let decision = config.permissions.check(
            "any_tool",
            &crate::tool::ApprovalRequirement::Never,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::Allow { .. }),
            "Missing settings files should result in empty policy (no static deny rules)"
        );

        let decision = config.permissions.check(
            "restricted_tool",
            &crate::tool::ApprovalRequirement::Always,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::NeedsApproval { .. }),
            "Missing settings files should not add any static allow rules"
        );
    }

    #[test]
    fn load_settings_valid_file_applies_rules() {
        // Create a valid settings.json in a temp directory and verify rules are applied.
        let tmp = tempfile::TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        std::fs::create_dir_all(&arlo_dir).unwrap();
        let settings_path = arlo_dir.join("settings.json");
        std::fs::write(
            &settings_path,
            r#"{
                "permissions": {
                    "allow": ["read_file", "fs_*"],
                    "deny": ["Bash(rm *)"]
                }
            }"#,
        )
        .unwrap();

        let config = RunConfig::builder(
            mock_provider(),
            "test-model",
        )
        .permissions(PermissionEngine::new(PermissionMode::Normal))
        .load_settings(tmp.path())
        .build();

        // read_file should be statically allowed (matches bare pattern "read_file")
        let decision = config.permissions.check(
            "read_file",
            &crate::tool::ApprovalRequirement::Always,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::Allow { .. }),
            "read_file should be statically allowed from settings"
        );

        // fs_write should be statically allowed (matches glob "fs_*")
        let decision = config.permissions.check(
            "fs_write",
            &crate::tool::ApprovalRequirement::Always,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::Allow { .. }),
            "fs_write should match 'fs_*' allow pattern from settings"
        );

        // Bash with "rm /tmp/foo" should be denied (matches compound pattern "Bash(rm *)")
        let decision = config.permissions.check(
            "Bash",
            &crate::tool::ApprovalRequirement::Never,
            Some(&json!({"command": "rm /tmp/foo"})),
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::Deny { .. }),
            "Bash(rm *) should be statically denied from settings"
        );

        // An unrelated tool should fall through to approval requirement
        let decision = config.permissions.check(
            "unknown_tool",
            &crate::tool::ApprovalRequirement::Always,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::NeedsApproval { .. }),
            "Tools not in allow/deny should fall through to approval requirement"
        );
    }

    #[test]
    fn load_settings_with_runtime_overrides() {
        // Test that calling load_settings first, then modifying the engine with
        // add_static_deny, can override what settings allowed.
        let tmp = tempfile::TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        std::fs::create_dir_all(&arlo_dir).unwrap();
        let settings_path = arlo_dir.join("settings.json");
        std::fs::write(
            &settings_path,
            r#"{
                "permissions": {
                    "allow": ["read_file", "fs_*"],
                    "deny": []
                }
            }"#,
        )
        .unwrap();

        let mut config = RunConfig::builder(
            mock_provider(),
            "test-model",
        )
        .permissions(PermissionEngine::new(PermissionMode::Normal))
        .load_settings(tmp.path())
        .build();

        // Initially, read_file is allowed from settings
        let decision = config.permissions.check(
            "read_file",
            &crate::tool::ApprovalRequirement::Always,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::Allow { .. }),
            "read_file should be allowed from settings initially"
        );

        // Now add a runtime deny for read_file (simulating runtime override)
        config.permissions.add_static_deny("read_file");

        // Deny should take precedence since it's checked first in Layer 2
        let decision = config.permissions.check(
            "read_file",
            &crate::tool::ApprovalRequirement::Always,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::Deny { .. }),
            "Runtime add_static_deny should override settings allow (deny checked first)"
        );

        // fs_write should still be allowed (not overridden)
        let decision = config.permissions.check(
            "fs_write",
            &crate::tool::ApprovalRequirement::Always,
            None,
        );
        assert!(
            matches!(decision, crate::permission::PermissionDecision::Allow { .. }),
            "fs_write should still match 'fs_*' allow (not overridden by runtime)"
        );
    }

    // ===================================================================
    // DenyAllApprovalHandler tests (Task 7.3)
    // Property 15: Non-Interactive Mode Default Deny
    // **Validates: Requirements 10.1, 10.2, 10.3, 10.5**
    // ===================================================================

    #[tokio::test]
    async fn deny_all_handler_single_pending_returns_deny() {
        let handler = DenyAllApprovalHandler;
        let context = ApprovalContext {
            agent_name: None,
            pending: vec![PendingApproval {
                tool_name: "Bash".to_string(),
                tool_input: json!({"command": "rm -rf /"}),
                request_id: "req-1".to_string(),
            }],
        };
        let responses = handler.request_approval(&context).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], ApprovalResponse::Deny);
    }

    #[tokio::test]
    async fn deny_all_handler_multiple_pending_returns_deny_for_all() {
        let handler = DenyAllApprovalHandler;
        let context = ApprovalContext {
            agent_name: Some("research-agent".to_string()),
            pending: vec![
                PendingApproval {
                    tool_name: "Bash".to_string(),
                    tool_input: json!({"command": "ls"}),
                    request_id: "req-1".to_string(),
                },
                PendingApproval {
                    tool_name: "file_write".to_string(),
                    tool_input: json!({"path": "/etc/passwd"}),
                    request_id: "req-2".to_string(),
                },
                PendingApproval {
                    tool_name: "web_fetch".to_string(),
                    tool_input: json!({"url": "https://example.com"}),
                    request_id: "req-3".to_string(),
                },
            ],
        };
        let responses = handler.request_approval(&context).await;
        assert_eq!(responses.len(), 3);
        for response in &responses {
            assert_eq!(*response, ApprovalResponse::Deny);
        }
    }

    #[tokio::test]
    async fn deny_all_handler_empty_pending_returns_empty() {
        let handler = DenyAllApprovalHandler;
        let context = ApprovalContext {
            agent_name: None,
            pending: vec![],
        };
        let responses = handler.request_approval(&context).await;
        assert!(responses.is_empty());
    }

    #[tokio::test]
    async fn deny_all_handler_never_blocks() {
        // Verify the handler returns immediately without blocking.
        // Use a timeout to detect blocking behavior.
        let handler = DenyAllApprovalHandler;
        let context = ApprovalContext {
            agent_name: Some("sub-agent".to_string()),
            pending: vec![PendingApproval {
                tool_name: "dangerous_tool".to_string(),
                tool_input: json!({}),
                request_id: "req-1".to_string(),
            }],
        };

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handler.request_approval(&context),
        )
        .await;

        assert!(result.is_ok(), "DenyAllApprovalHandler should return immediately without blocking");
        let responses = result.unwrap();
        assert_eq!(responses, vec![ApprovalResponse::Deny]);
    }

    #[tokio::test]
    async fn deny_all_handler_implements_approval_handler_trait() {
        // Verify DenyAllApprovalHandler can be used as Arc<dyn ApprovalHandler>
        let handler: Arc<dyn ApprovalHandler> = Arc::new(DenyAllApprovalHandler);
        let context = ApprovalContext {
            agent_name: None,
            pending: vec![PendingApproval {
                tool_name: "test_tool".to_string(),
                tool_input: json!({}),
                request_id: "req-1".to_string(),
            }],
        };
        let responses = handler.request_approval(&context).await;
        assert_eq!(responses, vec![ApprovalResponse::Deny]);
    }

    #[test]
    fn deny_all_handler_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DenyAllApprovalHandler>();
    }

    // Property test: DenyAllApprovalHandler returns Deny for any number of pending items
    proptest! {
        #[test]
        fn prop_deny_all_handler_denies_all_items(count in 0usize..50) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let handler = DenyAllApprovalHandler;
                let pending: Vec<PendingApproval> = (0..count)
                    .map(|i| PendingApproval {
                        tool_name: format!("tool_{}", i),
                        tool_input: json!({"arg": i}),
                        request_id: format!("req-{}", i),
                    })
                    .collect();
                let context = ApprovalContext {
                    agent_name: if count % 2 == 0 { None } else { Some("agent".to_string()) },
                    pending,
                };
                let responses = handler.request_approval(&context).await;
                assert_eq!(responses.len(), count);
                for response in &responses {
                    assert_eq!(*response, ApprovalResponse::Deny);
                }
            });
        }
    }
}
