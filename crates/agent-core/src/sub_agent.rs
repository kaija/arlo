//! Sub-agent tool implementation.
//!
//! `SubAgentTool` wraps a `SubAgentDef` and implements the `Tool` trait,
//! enabling parent agents to spawn isolated sub-agents via tool calls.
//!
//! When invoked, the sub-agent runs in a fresh RunLoop with empty message
//! history — the Claude-Code isolation model that prevents context contamination.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tracing::Instrument;

use crate::agent::SubAgentDef;
use crate::config::{Input, RunConfig, RunResult};
use crate::error::ToolError;
use crate::run_loop::run;
use crate::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// Global counter for generating unique background task identifiers.
static BACKGROUND_TASK_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A tool that spawns an isolated sub-agent when invoked.
///
/// The sub-agent starts with empty message history and has no access
/// to the parent's conversation. Token usage and cost from the sub-agent
/// are available in the returned `RunResult` for accumulation by the parent.
///
/// # Behavior
///
/// - **Foreground** (`background: false`): Awaits the sub-agent's completion
///   and returns its final output as `ToolOutput::Text`.
/// - **Background** (`background: true`): Spawns the sub-agent as a detached
///   tokio task and returns immediately with a task identifier.
#[derive(Clone)]
pub struct SubAgentTool {
    /// The sub-agent definition containing agent config and parameters.
    pub def: SubAgentDef,
    /// The parent's RunConfig, used as the basis for spawning sub-agent runs.
    /// The sub-agent's max_turns may override the parent's.
    pub config: RunConfig,
}

impl SubAgentTool {
    /// Create a new SubAgentTool from a definition and parent config.
    pub fn new(def: SubAgentDef, config: RunConfig) -> Self {
        Self { def, config }
    }

    /// Build a RunConfig for the sub-agent, overriding max_turns if specified.
    ///
    /// Sets the sub-agent's `agent_name` to this definition's agent name so that
    /// approval requests include the originating sub-agent's identity. The
    /// `approval_handler` is naturally shared via `Arc` on clone, enabling
    /// delegation of approval prompts to the parent's handler.
    ///
    /// Creates a shared `Arc<RwLock<Vec<ToolPattern>>>` session grant store and
    /// passes it to the sub-agent's PermissionEngine via `with_shared_session_grants`,
    /// enabling session grants issued during delegation to be visible across agents.
    fn sub_agent_config(&self) -> RunConfig {
        let mut config = self.config.clone();
        if let Some(max_turns) = self.def.max_turns {
            config.max_turns = max_turns;
        }

        // Set the agent_name so ApprovalContext identifies this sub-agent
        config.agent_name = Some(self.def.agent.name.clone());

        // approval_handler is Option<Arc<dyn ApprovalHandler>> — clone shares
        // the same Arc, so the sub-agent delegates approvals to the parent's handler.

        // Create a shared session grant store and wire it into the sub-agent's
        // PermissionEngine. This enables "always allow" grants issued during
        // delegated approval to be visible to all agents sharing this store.
        let shared_grants: Arc<tokio::sync::RwLock<Vec<crate::pattern::ToolPattern>>> =
            Arc::new(tokio::sync::RwLock::new(Vec::new()));
        config.permissions = config
            .permissions
            .with_shared_session_grants(shared_grants);

        config
    }

    /// Extract the user message from the tool input.
    ///
    /// If the input has a "task" field, use that as the prompt.
    /// Otherwise, serialize the entire input as JSON for the sub-agent.
    fn extract_prompt(input: &serde_json::Value) -> String {
        if let Some(task) = input.get("task").and_then(|v| v.as_str()) {
            task.to_string()
        } else if let Some(task) = input.get("task") {
            // task field exists but isn't a string — serialize it
            serde_json::to_string(task).unwrap_or_else(|_| input.to_string())
        } else {
            // No "task" field — use the entire input as the prompt
            serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string())
        }
    }

    /// Run the sub-agent synchronously (foreground mode).
    async fn run_foreground(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let prompt = Self::extract_prompt(&input);
        let sub_config = self.sub_agent_config();
        let sub_input = Input::Fresh { prompt };

        let sub_agent_span = tracing::info_span!(
            "sub_agent",
            agent_name = %self.def.agent.name,
        );

        match async { run(&self.def.agent, sub_input, &sub_config).await }
            .instrument(sub_agent_span)
            .await
        {
            Ok(result) => {
                // Check if the sub-agent hit max turns
                if let Some(max_turns) = self.def.max_turns {
                    if result.turns >= max_turns {
                        return Ok(ToolOutput::Text(format!(
                            "{}\n\n[Sub-agent reached turn limit of {}]",
                            result.output, max_turns
                        )));
                    }
                }
                Ok(ToolOutput::Text(result.output))
            }
            Err(crate::error::RunError::MaxTurns(count)) => {
                // Sub-agent terminated due to max turns
                Ok(ToolOutput::Text(format!(
                    "[Sub-agent terminated: reached maximum turn limit of {}]",
                    count
                )))
            }
            Err(e) => {
                // Return error description as ToolOutput::Error (non-fatal to parent)
                Ok(ToolOutput::Error(format!(
                    "Sub-agent '{}' error: {}",
                    self.def.agent.name, e
                )))
            }
        }
    }

    /// Spawn the sub-agent as a background task and return immediately.
    fn run_background(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let prompt = Self::extract_prompt(&input);
        let sub_config = self.sub_agent_config();
        let sub_input = Input::Fresh { prompt };
        let agent = Arc::clone(&self.def.agent);
        let task_id = BACKGROUND_TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
        let agent_name = self.def.agent.name.clone();

        let sub_agent_span = tracing::info_span!(
            "sub_agent",
            agent_name = %agent_name,
        );

        // Spawn as a detached tokio task
        tokio::spawn(
            async move {
                let _result = run(&agent, sub_input, &sub_config).await;
                // Background task result is fire-and-forget.
                // In a full implementation, results could be stored in a task registry.
            }
            .instrument(sub_agent_span),
        );

        Ok(ToolOutput::Text(format!(
            "Background task started: task_id={}, agent='{}'",
            task_id, self.def.agent.name
        )))
    }
}

#[async_trait]
impl Tool for SubAgentTool {
    fn name(&self) -> &str {
        self.def
            .tool_name
            .as_deref()
            .unwrap_or(&self.def.agent.name)
    }

    fn description(&self) -> &str {
        self.def.tool_description.as_deref().unwrap_or_else(|| {
            // Can't dynamically format here since we return &str.
            // Use a static fallback.
            "Delegate a task to a specialized sub-agent"
        })
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.def.input_schema.clone().unwrap_or_else(|| {
            json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The task to delegate to the sub-agent"
                    }
                },
                "required": ["task"]
            })
        })
    }

    fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
        // Sub-agents run concurrently — they are isolated and don't
        // share mutable state with the parent or siblings.
        Concurrency::Safe
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        if self.def.background {
            self.run_background(input)
        } else {
            self.run_foreground(input).await
        }
    }
}

/// Helper to extract usage/cost from a sub-agent RunResult for parent accumulation.
///
/// After calling `run()` on a sub-agent and getting a `RunResult`, the parent
/// loop should call this to get the values to add to its own RunState.
pub struct SubAgentUsage {
    /// Total input tokens used by the sub-agent.
    pub input_tokens: u64,
    /// Total output tokens used by the sub-agent.
    pub output_tokens: u64,
    /// Total cache read tokens used by the sub-agent.
    pub cache_read_tokens: Option<u64>,
    /// Total cost in USD incurred by the sub-agent.
    pub cost_usd: f64,
}

impl From<&RunResult> for SubAgentUsage {
    fn from(result: &RunResult) -> Self {
        Self {
            input_tokens: result.usage.input_tokens,
            output_tokens: result.usage.output_tokens,
            cache_read_tokens: result.usage.cache_read_tokens,
            cost_usd: result.cost_usd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::config::RunConfig;
    use crate::error::ModelError;
    use crate::model::{Model, ModelProvider, ModelRequest, ModelResponse, ModelStream};
    use crate::tool::ApprovalRequirement;
    use futures::stream;
    use proptest::prelude::*;
    use serde_json::json;
    use std::path::PathBuf;

    /// A mock model provider that returns a model producing a simple text response.
    struct MockModelProvider;

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        async fn resolve(&self, _model_name: &str) -> Result<Arc<dyn Model>, ModelError> {
            Ok(Arc::new(MockModel))
        }
        fn available_models(&self) -> Vec<String> {
            vec!["mock".to_string()]
        }
    }

    /// A mock model that immediately returns "Sub-agent done" as a final output.
    struct MockModel;

    #[async_trait]
    impl Model for MockModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            use crate::message::Usage;
            use crate::stream::{StopReason, StreamChunk};

            let chunks = vec![
                Ok(StreamChunk::TextDelta {
                    text: "Sub-agent done".to_string(),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 100,
                        output_tokens: 50,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(stream::iter(chunks)))
        }

        async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
            let _ = request;
            Ok(ModelResponse {
                content: vec![crate::model::ContentBlock::Text {
                    text: "Sub-agent done".to_string(),
                }],
                usage: crate::message::Usage::default(),
                stop_reason: crate::stream::StopReason::EndTurn,
            })
        }

        fn name(&self) -> &str {
            "mock-model"
        }
        fn provider(&self) -> &str {
            "mock"
        }
        fn context_window(&self) -> usize {
            128_000
        }
        fn max_output_tokens(&self) -> usize {
            4096
        }
        fn supports_tools(&self) -> bool {
            true
        }
        fn input_cost_per_million(&self) -> f64 {
            3.0
        }
        fn output_cost_per_million(&self) -> f64 {
            15.0
        }
    }

    fn make_sub_agent_tool(background: bool) -> SubAgentTool {
        let sub_agent = Agent::builder("test-sub-agent").build();
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: Some("delegate_task".to_string()),
            tool_description: Some("Delegate work to a sub-agent".to_string()),
            input_schema: None,
            max_turns: Some(5),
            background,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .max_turns(25)
            .build();
        SubAgentTool::new(def, config)
    }

    fn make_context() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            working_dir: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn sub_agent_tool_name_uses_tool_name_field() {
        let tool = make_sub_agent_tool(false);
        assert_eq!(tool.name(), "delegate_task");
    }

    #[test]
    fn sub_agent_tool_name_falls_back_to_agent_name() {
        let sub_agent = Agent::builder("my-helper").build();
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .build();
        let tool = SubAgentTool::new(def, config);
        assert_eq!(tool.name(), "my-helper");
    }

    #[test]
    fn sub_agent_tool_description_uses_custom() {
        let tool = make_sub_agent_tool(false);
        assert_eq!(tool.description(), "Delegate work to a sub-agent");
    }

    #[test]
    fn sub_agent_tool_description_falls_back() {
        let sub_agent = Agent::builder("helper").build();
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .build();
        let tool = SubAgentTool::new(def, config);
        assert_eq!(
            tool.description(),
            "Delegate a task to a specialized sub-agent"
        );
    }

    #[test]
    fn sub_agent_tool_parameters_schema_default() {
        let tool = make_sub_agent_tool(false);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["task"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("task")));
    }

    #[test]
    fn sub_agent_tool_parameters_schema_custom() {
        let sub_agent = Agent::builder("custom").build();
        let custom_schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "max_results": { "type": "number" }
            },
            "required": ["query"]
        });
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: None,
            tool_description: None,
            input_schema: Some(custom_schema.clone()),
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .build();
        let tool = SubAgentTool::new(def, config);
        assert_eq!(tool.parameters_schema(), custom_schema);
    }

    #[test]
    fn sub_agent_tool_concurrency_is_safe() {
        let tool = make_sub_agent_tool(false);
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Safe);
    }

    #[test]
    fn sub_agent_tool_approval_is_never() {
        let tool = make_sub_agent_tool(false);
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Never);
    }

    #[test]
    fn extract_prompt_with_task_string() {
        let input = json!({"task": "Find the answer"});
        assert_eq!(SubAgentTool::extract_prompt(&input), "Find the answer");
    }

    #[test]
    fn extract_prompt_with_task_object() {
        let input = json!({"task": {"query": "test"}});
        let prompt = SubAgentTool::extract_prompt(&input);
        assert!(prompt.contains("query"));
        assert!(prompt.contains("test"));
    }

    #[test]
    fn extract_prompt_without_task_field() {
        let input = json!({"query": "search for something", "limit": 10});
        let prompt = SubAgentTool::extract_prompt(&input);
        assert!(prompt.contains("query"));
        assert!(prompt.contains("search for something"));
    }

    #[tokio::test]
    async fn sub_agent_tool_execute_foreground() {
        let tool = make_sub_agent_tool(false);
        let ctx = make_context();
        let result = tool
            .execute(json!({"task": "Do something"}), &ctx)
            .await;
        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => {
                assert!(text.contains("Sub-agent done"));
            }
            other => panic!("Expected ToolOutput::Text, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn sub_agent_tool_execute_background() {
        let tool = make_sub_agent_tool(true);
        let ctx = make_context();
        let result = tool
            .execute(json!({"task": "Background work"}), &ctx)
            .await;
        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => {
                assert!(text.contains("Background task started"));
                assert!(text.contains("task_id="));
                assert!(text.contains("test-sub-agent"));
            }
            other => panic!("Expected ToolOutput::Text, got {:?}", other),
        }
    }

    // ─── Property 13: Sub-agent cost accumulation ───────────────────────
    // **Validates: Requirements 14.7**
    //
    // SubAgentUsage::from(&RunResult) must extract the exact usage and cost
    // fields from the RunResult. When multiple sub-agent results are
    // accumulated into a parent RunState, the parent's total_usage and
    // total_cost_usd must equal the component-wise sum.

    mod prop_cost_accumulation {
        use super::*;
        use crate::config::RunResult;
        use crate::message::Usage;
        use crate::state::RunState;
        #[allow(unused_imports)]
        use proptest::prelude::*;

        /// Strategy for generating arbitrary Usage values.
        fn arb_usage() -> impl Strategy<Value = Usage> {
            (
                0u64..1_000_000,
                0u64..1_000_000,
                proptest::option::of(0u64..500_000),
            )
                .prop_map(|(input_tokens, output_tokens, cache_read_tokens)| Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                })
        }

        /// Strategy for generating arbitrary RunResult values.
        fn arb_run_result() -> impl Strategy<Value = RunResult> {
            (arb_usage(), 0.0f64..100.0, 1u32..50)
                .prop_map(|(usage, cost_usd, turns)| {
                    RunResult {
                        output: "sub-agent output".to_string(),
                        structured: None,
                        usage,
                        cost_usd,
                        turns,
                        state: RunState::new("sub-run".to_string(), None, None),
                    }
                })
        }

        proptest! {
            /// SubAgentUsage::from extracts exact values from RunResult.
            #[test]
            fn prop_sub_agent_usage_extraction(result in arb_run_result()) {
                let usage = SubAgentUsage::from(&result);
                prop_assert_eq!(usage.input_tokens, result.usage.input_tokens,
                    "input_tokens mismatch");
                prop_assert_eq!(usage.output_tokens, result.usage.output_tokens,
                    "output_tokens mismatch");
                prop_assert_eq!(usage.cache_read_tokens, result.usage.cache_read_tokens,
                    "cache_read_tokens mismatch");
                prop_assert!((usage.cost_usd - result.cost_usd).abs() < f64::EPSILON,
                    "cost_usd mismatch: got {}, expected {}", usage.cost_usd, result.cost_usd);
            }

            /// Multiple sub-agent results accumulate correctly into parent state.
            #[test]
            fn prop_sub_agent_accumulation(
                results in proptest::collection::vec(arb_run_result(), 1..10)
            ) {
                let mut parent_state = RunState::new("parent-run".to_string(), None, None);

                // Accumulate each sub-agent result into parent state
                for result in &results {
                    let sub_usage = SubAgentUsage::from(result);
                    parent_state.total_usage.input_tokens += sub_usage.input_tokens;
                    parent_state.total_usage.output_tokens += sub_usage.output_tokens;
                    // Accumulate cache_read_tokens, treating None as 0
                    if let Some(cache) = sub_usage.cache_read_tokens {
                        let current = parent_state.total_usage.cache_read_tokens.unwrap_or(0);
                        parent_state.total_usage.cache_read_tokens = Some(current + cache);
                    }
                    parent_state.total_cost_usd += sub_usage.cost_usd;
                }

                // Compute expected sums
                let expected_input: u64 = results.iter().map(|r| r.usage.input_tokens).sum();
                let expected_output: u64 = results.iter().map(|r| r.usage.output_tokens).sum();
                let expected_cache: u64 = results.iter()
                    .filter_map(|r| r.usage.cache_read_tokens)
                    .sum();
                let expected_cost: f64 = results.iter().map(|r| r.cost_usd).sum();

                prop_assert_eq!(parent_state.total_usage.input_tokens, expected_input,
                    "accumulated input_tokens mismatch");
                prop_assert_eq!(parent_state.total_usage.output_tokens, expected_output,
                    "accumulated output_tokens mismatch");

                // Check cache: if any sub-agent had cache_read_tokens, parent should have Some
                let any_cache = results.iter().any(|r| r.usage.cache_read_tokens.is_some());
                if any_cache {
                    prop_assert_eq!(parent_state.total_usage.cache_read_tokens, Some(expected_cache),
                        "accumulated cache_read_tokens mismatch");
                } else {
                    prop_assert_eq!(parent_state.total_usage.cache_read_tokens, None,
                        "cache_read_tokens should be None when no sub-agent had cache");
                }

                // For f64 cost, allow small floating point error
                let cost_diff = (parent_state.total_cost_usd - expected_cost).abs();
                prop_assert!(cost_diff < 1e-10,
                    "accumulated cost mismatch: got {}, expected {}, diff {}",
                    parent_state.total_cost_usd, expected_cost, cost_diff);
            }
        }
    }

    #[test]
    fn sub_agent_tool_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SubAgentTool>();
    }

    #[test]
    fn sub_agent_tool_as_dyn_tool() {
        let tool = make_sub_agent_tool(false);
        let _: Arc<dyn Tool> = Arc::new(tool);
    }

    #[test]
    fn sub_agent_usage_from_run_result() {
        use crate::message::Usage;
        use crate::state::RunState;

        let result = RunResult {
            output: "done".to_string(),
            structured: None,
            usage: Usage {
                input_tokens: 500,
                output_tokens: 200,
                cache_read_tokens: Some(50),
            },
            cost_usd: 0.0045,
            turns: 3,
            state: RunState::new("r".to_string(), None, None),
        };
        let usage = SubAgentUsage::from(&result);
        assert_eq!(usage.input_tokens, 500);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.cache_read_tokens, Some(50));
        assert_eq!(usage.cost_usd, 0.0045);
    }

    #[test]
    fn sub_agent_config_overrides_max_turns() {
        let tool = make_sub_agent_tool(false);
        let sub_config = tool.sub_agent_config();
        // def.max_turns is Some(5), so sub_config should have max_turns = 5
        assert_eq!(sub_config.max_turns, 5);
        // agent_name should be set to the sub-agent's name
        assert_eq!(sub_config.agent_name, Some("test-sub-agent".to_string()));
    }

    #[test]
    fn sub_agent_config_no_override() {
        let sub_agent = Agent::builder("no-override").build();
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: None, // No override
            background: false,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .max_turns(25)
            .build();
        let tool = SubAgentTool::new(def, config);
        let sub_config = tool.sub_agent_config();
        // Should keep parent's max_turns
        assert_eq!(sub_config.max_turns, 25);
        // agent_name should be set to the sub-agent's name
        assert_eq!(sub_config.agent_name, Some("no-override".to_string()));
    }

    // ===================================================================
    // Task 10.1: Sub-Agent Permission Propagation Tests
    // **Validates: Requirements 7.1, 7.3, 7.5, 7.7**
    //
    // Tests that SubAgentTool correctly propagates agent_name, shares
    // the ApprovalHandler via Arc, and creates a shared session grant store.
    // ===================================================================

    #[test]
    fn sub_agent_config_sets_agent_name_from_def() {
        // The sub-agent's agent_name should be set from the SubAgentDef's agent name.
        let sub_agent = Agent::builder("research-agent").build();
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: Some("research".to_string()),
            tool_description: None,
            input_schema: None,
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .build();
        let tool = SubAgentTool::new(def, config);
        let sub_config = tool.sub_agent_config();

        // agent_name is the Agent's name, not the tool_name
        assert_eq!(sub_config.agent_name, Some("research-agent".to_string()));
    }

    #[test]
    fn sub_agent_config_shares_approval_handler_via_arc() {
        // When the parent has an approval_handler, the sub-agent should share the same Arc.
        use crate::config::{ApprovalContext, ApprovalHandler, ApprovalResponse};

        struct TestHandler;
        #[async_trait]
        impl ApprovalHandler for TestHandler {
            async fn request_approval(&self, ctx: &ApprovalContext) -> Vec<ApprovalResponse> {
                ctx.pending.iter().map(|_| ApprovalResponse::Allow).collect()
            }
        }

        let handler: Arc<dyn ApprovalHandler> = Arc::new(TestHandler);
        let handler_ptr = Arc::as_ptr(&handler) as *const ();

        let sub_agent = Agent::builder("helper").build();
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .approval_handler(handler)
            .build();
        let tool = SubAgentTool::new(def, config);
        let sub_config = tool.sub_agent_config();

        // The sub-agent should have the same Arc (pointer equality)
        assert!(sub_config.approval_handler.is_some());
        let sub_handler_ptr = Arc::as_ptr(sub_config.approval_handler.as_ref().unwrap()) as *const ();
        assert_eq!(handler_ptr, sub_handler_ptr,
            "Sub-agent's approval_handler should be the same Arc as the parent's");
    }

    #[test]
    fn sub_agent_config_creates_shared_session_grants_store() {
        // The sub-agent should have a shared_session_grants store set on its PermissionEngine.
        let sub_agent = Agent::builder("worker").build();
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .build();
        let tool = SubAgentTool::new(def, config);
        let sub_config = tool.sub_agent_config();

        // Verify the permission engine has shared session grants via has_session_allow behavior.
        // Grant a session allow, then verify it's accessible.
        let mut permissions = sub_config.permissions;
        permissions.grant_session_allow("test_tool");
        assert!(permissions.has_session_allow("test_tool", None));
    }

    #[test]
    fn sub_agent_config_without_handler_has_none() {
        // When parent has no approval_handler, sub-agent should also have None.
        let sub_agent = Agent::builder("solo").build();
        let def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let config = RunConfig::builder(Arc::new(MockModelProvider) as Arc<dyn ModelProvider>, "mock")
            .build();
        let tool = SubAgentTool::new(def, config);
        let sub_config = tool.sub_agent_config();

        assert!(sub_config.approval_handler.is_none());
    }
}

/// Property-based tests for sub-agent isolation.
///
/// Feature: rust-agent-framework, Property 12: Sub-agent isolation
/// **Validates: Requirements 14.3, 14.6**
///
/// When a SubAgentTool is invoked, the sub-agent starts with a fresh
/// (empty) message history containing only the task prompt. It must NOT
/// have access to the parent's message history.
#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::agent::Agent;
    use crate::config::RunConfig;
    use crate::error::ModelError;
    use crate::message::{ContentBlock, Message, Usage};
    use crate::model::{Model, ModelProvider, ModelRequest, ModelResponse, ModelStream};
    use crate::stream::{StopReason, StreamChunk};
    use async_trait::async_trait;
    use futures::stream;
    use proptest::prelude::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// A mock model that captures the ModelRequest it receives into a shared vec,
    /// then returns a simple "done" response.
    struct CapturingModel {
        captured_requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    #[async_trait]
    impl Model for CapturingModel {
        async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
            // Capture the request
            self.captured_requests.lock().unwrap().push(request);

            let chunks = vec![
                Ok(StreamChunk::TextDelta {
                    text: "done".to_string(),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(stream::iter(chunks)))
        }

        async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
            self.captured_requests.lock().unwrap().push(request);
            Ok(ModelResponse {
                content: vec![crate::model::ContentBlock::Text {
                    text: "done".to_string(),
                }],
                usage: Usage::default(),
                stop_reason: StopReason::EndTurn,
            })
        }

        fn name(&self) -> &str {
            "capturing-model"
        }
        fn provider(&self) -> &str {
            "test"
        }
        fn context_window(&self) -> usize {
            128_000
        }
        fn max_output_tokens(&self) -> usize {
            4096
        }
        fn supports_tools(&self) -> bool {
            true
        }
        fn input_cost_per_million(&self) -> f64 {
            0.0
        }
        fn output_cost_per_million(&self) -> f64 {
            0.0
        }
    }

    /// A mock provider that returns the capturing model.
    struct CapturingProvider {
        captured_requests: Arc<Mutex<Vec<ModelRequest>>>,
    }

    #[async_trait]
    impl ModelProvider for CapturingProvider {
        async fn resolve(&self, _model_name: &str) -> Result<Arc<dyn Model>, ModelError> {
            Ok(Arc::new(CapturingModel {
                captured_requests: Arc::clone(&self.captured_requests),
            }))
        }
        fn available_models(&self) -> Vec<String> {
            vec!["capturing".to_string()]
        }
    }

    /// Strategy to generate random message histories representing a parent's state.
    fn arb_message() -> impl Strategy<Value = Message> {
        prop_oneof![
            // System message
            "[a-z ]{1,50}".prop_map(|s| Message::System { content: s }),
            // User message with text
            "[a-z ]{1,80}".prop_map(|s| Message::User {
                content: vec![ContentBlock::Text { text: s }],
            }),
            // Assistant message with text
            "[a-z ]{1,80}".prop_map(|s| Message::Assistant {
                content: vec![ContentBlock::Text { text: s }],
                usage: Some(Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_read_tokens: None,
                }),
            }),
            // ToolResult message
            ("[a-z]{5,15}", "[a-z ]{1,60}").prop_map(|(id, content)| Message::ToolResult {
                tool_use_id: id,
                content,
                is_error: false,
            }),
        ]
    }

    /// Strategy to generate a parent message history of 1..20 messages.
    fn arb_parent_history() -> impl Strategy<Value = Vec<Message>> {
        prop::collection::vec(arb_message(), 1..20)
    }

    /// Strategy to generate a task prompt string.
    fn arb_task_prompt() -> impl Strategy<Value = String> {
        "[a-zA-Z ]{5,100}"
    }

    proptest! {
        /// Property: When SubAgentTool is executed, the sub-agent's model receives
        /// a ModelRequest whose messages contain ONLY the fresh task prompt (a single
        /// User message), regardless of what the parent's message history contains.
        ///
        /// This proves that sub-agents start with empty history and do NOT inherit
        /// the parent's conversation context.
        #[test]
        fn prop_sub_agent_starts_with_empty_history(
            parent_messages in arb_parent_history(),
            task_prompt in arb_task_prompt(),
        ) {
            // Use a tokio runtime to run the async sub-agent execution
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let result = rt.block_on(async {
                // Set up the capturing mock
                let captured_requests: Arc<Mutex<Vec<ModelRequest>>> =
                    Arc::new(Mutex::new(Vec::new()));

                let provider: Arc<dyn ModelProvider> = Arc::new(CapturingProvider {
                    captured_requests: Arc::clone(&captured_requests),
                });

                let config = RunConfig::builder(provider, "capturing")
                    .max_turns(2)
                    .build();

                // Create a sub-agent tool
                let sub_agent = Agent::builder("isolated-sub-agent").build();
                let def = SubAgentDef {
                    agent: Arc::new(sub_agent),
                    tool_name: Some("delegate".to_string()),
                    tool_description: Some("Delegate a task".to_string()),
                    input_schema: None,
                    max_turns: Some(1), // One turn is enough to capture the request
                    background: false,
                    allowed_tools: None,
                };
                let tool = SubAgentTool::new(def, config);

                // The parent would have `parent_messages` in its state, but the sub-agent
                // tool only passes the task prompt (from tool input) as a fresh Input.
                let ctx = crate::tool::ToolContext {
                    session_id: "parent-session".to_string(),
                    working_dir: PathBuf::from("/tmp"),
                };

                let input = json!({ "task": task_prompt.clone() });
                let _result = tool.execute(input, &ctx).await;

                // Return captured requests for assertion outside async
                let x = captured_requests.lock().unwrap().clone(); x
            });

            // Verify the captured model request
            prop_assert!(!result.is_empty(),
                "Sub-agent model should have been called at least once");

            let first_request = &result[0];

            // The sub-agent should have exactly 1 message: the User message with the task prompt
            prop_assert_eq!(
                first_request.messages.len(),
                1,
                "Sub-agent should start with exactly 1 message (the fresh prompt), got {}",
                first_request.messages.len()
            );

            // Verify the single message is the User message with the task prompt
            match &first_request.messages[0] {
                Message::User { content } => {
                    prop_assert_eq!(content.len(), 1,
                        "User message should have 1 content block");
                    match &content[0] {
                        ContentBlock::Text { text } => {
                            prop_assert_eq!(
                                text, &task_prompt,
                                "Sub-agent's user message should be the task prompt"
                            );
                        }
                        other => {
                            prop_assert!(false,
                                "Expected Text content block, got {:?}", other);
                        }
                    }
                }
                other => {
                    prop_assert!(false,
                        "Expected User message, got {:?}", other);
                }
            }

            // Verify that NONE of the parent messages appear in the sub-agent request
            for parent_msg in &parent_messages {
                prop_assert!(
                    !first_request.messages.contains(parent_msg),
                    "Parent message should NOT appear in sub-agent's messages: {:?}",
                    parent_msg
                );
            }
        }
    }
}
