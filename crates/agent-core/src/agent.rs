//! Agent configuration and builder pattern for defining autonomous agents.
//!
//! The `Agent` struct is the top-level configuration defining an agent's behavior,
//! tools, instructions, sub-agents, guardrails, and lifecycle hooks.
//! Constructed via the builder pattern: `Agent::builder("name")`.

use std::fmt;
use std::pin::Pin;
use std::sync::Arc;

use futures::Future;

use crate::guardrail::{InputGuardrail, OutputGuardrail};
use crate::state::RunState;
use crate::tool::Tool;

/// Context provided to dynamic instructions and lifecycle hooks during execution.
///
/// Contains run-scoped information needed for context-aware behaviors.
#[derive(Debug, Clone)]
pub struct RunContext {
    /// The current run state snapshot.
    pub state: RunState,
}

/// A boxed future that is Send.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The instructions for an agent, either static or dynamically generated.
pub enum Instructions {
    /// A fixed instruction string.
    Static(String),
    /// A function that generates instructions based on the current run context.
    Dynamic(Arc<dyn Fn(&RunContext) -> BoxFuture<'static, String> + Send + Sync>),
}

impl fmt::Debug for Instructions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Instructions::Static(s) => f.debug_tuple("Static").field(s).finish(),
            Instructions::Dynamic(_) => f.debug_tuple("Dynamic").field(&"<fn>").finish(),
        }
    }
}

impl Clone for Instructions {
    fn clone(&self) -> Self {
        match self {
            Instructions::Static(s) => Instructions::Static(s.clone()),
            Instructions::Dynamic(f) => Instructions::Dynamic(Arc::clone(f)),
        }
    }
}

impl Default for Instructions {
    fn default() -> Self {
        Instructions::Static(String::new())
    }
}

/// Lifecycle hook callback type.
///
/// Hooks receive the current `RunContext` and return a boxed future.
pub type HookCallback = Arc<dyn Fn(&RunContext) -> BoxFuture<'static, ()> + Send + Sync>;

/// Optional lifecycle hooks that fire during agent execution.
///
/// All hooks are optional. When set, they are invoked at the corresponding
/// point in the RunLoop lifecycle.
#[derive(Clone, Default)]
pub struct AgentHooks {
    /// Called at the start of each turn.
    pub on_turn_start: Option<HookCallback>,
    /// Called at the end of each turn.
    pub on_turn_end: Option<HookCallback>,
    /// Called before a tool starts executing.
    pub on_tool_start: Option<HookCallback>,
    /// Called after a tool finishes executing.
    pub on_tool_end: Option<HookCallback>,
}

impl fmt::Debug for AgentHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentHooks")
            .field("on_turn_start", &self.on_turn_start.is_some())
            .field("on_turn_end", &self.on_turn_end.is_some())
            .field("on_tool_start", &self.on_tool_start.is_some())
            .field("on_tool_end", &self.on_tool_end.is_some())
            .finish()
    }
}

/// Definition of a sub-agent that can be spawned by the parent agent as a tool.
#[derive(Clone)]
pub struct SubAgentDef {
    /// The sub-agent configuration.
    pub agent: Arc<Agent>,
    /// Optional custom tool name (defaults to the sub-agent's name).
    pub tool_name: Option<String>,
    /// Optional custom tool description.
    pub tool_description: Option<String>,
    /// Optional JSON schema for the tool's input parameters.
    pub input_schema: Option<serde_json::Value>,
    /// Optional maximum turns for the sub-agent's run.
    pub max_turns: Option<u32>,
    /// Whether to run the sub-agent in the background.
    pub background: bool,
    /// Optional list of tools the sub-agent is allowed to use.
    pub allowed_tools: Option<Vec<String>>,
}

impl fmt::Debug for SubAgentDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubAgentDef")
            .field("agent", &self.agent.name)
            .field("tool_name", &self.tool_name)
            .field("tool_description", &self.tool_description)
            .field("max_turns", &self.max_turns)
            .field("background", &self.background)
            .field("allowed_tools", &self.allowed_tools)
            .finish()
    }
}

/// The top-level configuration struct defining an autonomous agent's behavior.
///
/// Created via the builder pattern:
/// ```ignore
/// let agent = Agent::builder("my-agent")
///     .instructions(Instructions::Static("You are helpful.".into()))
///     .model("claude-sonnet-4-20250514")
///     .tool(my_tool)
///     .build();
/// ```
#[derive(Clone)]
pub struct Agent {
    /// The unique name of this agent.
    pub name: String,
    /// The instructions (system prompt) for this agent.
    pub instructions: Instructions,
    /// The model name to use (resolved via ModelProvider at runtime).
    pub model: Option<String>,
    /// Tools available to this agent.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Sub-agents that can be spawned as tools.
    pub sub_agents: Vec<SubAgentDef>,
    /// Input guardrails checked on the first turn.
    pub input_guardrails: Vec<Arc<dyn InputGuardrail>>,
    /// Output guardrails checked before delivering final output.
    pub output_guardrails: Vec<Arc<dyn OutputGuardrail>>,
    /// Optional JSON schema for structured output.
    pub output_schema: Option<serde_json::Value>,
    /// Optional maximum turns override (takes precedence over RunConfig).
    pub max_turns: Option<u32>,
    /// Lifecycle hooks for this agent.
    pub hooks: AgentHooks,
}

impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("name", &self.name)
            .field("instructions", &self.instructions)
            .field("model", &self.model)
            .field("tools_count", &self.tools.len())
            .field("sub_agents_count", &self.sub_agents.len())
            .field("input_guardrails_count", &self.input_guardrails.len())
            .field("output_guardrails_count", &self.output_guardrails.len())
            .field("output_schema", &self.output_schema)
            .field("max_turns", &self.max_turns)
            .field("hooks", &self.hooks)
            .finish()
    }
}

impl Agent {
    /// Create a new `AgentBuilder` with the given agent name.
    ///
    /// The name is required and identifies the agent in events and tracing.
    pub fn builder(name: impl Into<String>) -> AgentBuilder {
        AgentBuilder {
            name: name.into(),
            instructions: Instructions::default(),
            model: None,
            tools: Vec::new(),
            sub_agents: Vec::new(),
            input_guardrails: Vec::new(),
            output_guardrails: Vec::new(),
            output_schema: None,
            max_turns: None,
            hooks: AgentHooks::default(),
        }
    }
}

/// Builder for constructing an `Agent` with chainable setters.
///
/// All collection-typed fields use additive methods that append to the
/// existing collection. Options default to `None`, Vecs to empty.
pub struct AgentBuilder {
    name: String,
    instructions: Instructions,
    model: Option<String>,
    tools: Vec<Arc<dyn Tool>>,
    sub_agents: Vec<SubAgentDef>,
    input_guardrails: Vec<Arc<dyn InputGuardrail>>,
    output_guardrails: Vec<Arc<dyn OutputGuardrail>>,
    output_schema: Option<serde_json::Value>,
    max_turns: Option<u32>,
    hooks: AgentHooks,
}

impl AgentBuilder {
    /// Set the instructions for the agent.
    pub fn instructions(mut self, instructions: Instructions) -> Self {
        self.instructions = instructions;
        self
    }

    /// Set the model name for the agent.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Add a tool to the agent's tool set (additive).
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Add a sub-agent definition (additive).
    pub fn sub_agent(mut self, sub_agent: SubAgentDef) -> Self {
        self.sub_agents.push(sub_agent);
        self
    }

    /// Add an input guardrail (additive).
    pub fn input_guardrail(mut self, guardrail: Arc<dyn InputGuardrail>) -> Self {
        self.input_guardrails.push(guardrail);
        self
    }

    /// Add an output guardrail (additive).
    pub fn output_guardrail(mut self, guardrail: Arc<dyn OutputGuardrail>) -> Self {
        self.output_guardrails.push(guardrail);
        self
    }

    /// Set the JSON schema for structured output.
    pub fn output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Set the maximum number of turns for this agent.
    pub fn max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// Set the lifecycle hooks for this agent.
    pub fn hooks(mut self, hooks: AgentHooks) -> Self {
        self.hooks = hooks;
        self
    }

    /// Consume the builder and produce an `Agent`.
    ///
    /// Defaults:
    /// - `instructions` → `Static("")`
    /// - `model` → `None`
    /// - `tools` → empty Vec
    /// - `sub_agents` → empty Vec
    /// - `input_guardrails` → empty Vec
    /// - `output_guardrails` → empty Vec
    /// - `output_schema` → `None`
    /// - `max_turns` → `None`
    /// - `hooks` → all `None`
    pub fn build(self) -> Agent {
        Agent {
            name: self.name,
            instructions: self.instructions,
            model: self.model,
            tools: self.tools,
            sub_agents: self.sub_agents,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            output_schema: self.output_schema,
            max_turns: self.max_turns,
            hooks: self.hooks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ToolError;
    use crate::guardrail::GuardrailResult;
    use crate::message::Message;
    use crate::tool::{Concurrency, ToolContext, ToolOutput};
    use async_trait::async_trait;
    use serde_json::json;

    /// A minimal test tool for builder tests.
    struct MockTool {
        tool_name: String,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "a mock tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Text("mock".to_string()))
        }
    }

    /// A minimal test input guardrail.
    struct MockInputGuardrail;

    #[async_trait]
    impl InputGuardrail for MockInputGuardrail {
        fn name(&self) -> &str {
            "mock_input_guardrail"
        }
        async fn check(&self, _messages: &[Message]) -> GuardrailResult {
            GuardrailResult::pass()
        }
    }

    /// A minimal test output guardrail.
    struct MockOutputGuardrail;

    #[async_trait]
    impl OutputGuardrail for MockOutputGuardrail {
        fn name(&self) -> &str {
            "mock_output_guardrail"
        }
        async fn check(
            &self,
            _output: &str,
            _structured: Option<&serde_json::Value>,
        ) -> GuardrailResult {
            GuardrailResult::pass()
        }
    }

    #[test]
    fn agent_builder_defaults() {
        let agent = Agent::builder("test-agent").build();
        assert_eq!(agent.name, "test-agent");
        assert!(matches!(agent.instructions, Instructions::Static(ref s) if s.is_empty()));
        assert_eq!(agent.model, None);
        assert!(agent.tools.is_empty());
        assert!(agent.sub_agents.is_empty());
        assert!(agent.input_guardrails.is_empty());
        assert!(agent.output_guardrails.is_empty());
        assert_eq!(agent.output_schema, None);
        assert_eq!(agent.max_turns, None);
        assert!(agent.hooks.on_turn_start.is_none());
        assert!(agent.hooks.on_turn_end.is_none());
        assert!(agent.hooks.on_tool_start.is_none());
        assert!(agent.hooks.on_tool_end.is_none());
    }

    #[test]
    fn agent_builder_with_all_fields() {
        let tool: Arc<dyn Tool> = Arc::new(MockTool {
            tool_name: "echo".to_string(),
        });
        let input_guard: Arc<dyn InputGuardrail> = Arc::new(MockInputGuardrail);
        let output_guard: Arc<dyn OutputGuardrail> = Arc::new(MockOutputGuardrail);

        let sub_agent_inner = Agent::builder("sub").build();
        let sub_def = SubAgentDef {
            agent: Arc::new(sub_agent_inner),
            tool_name: Some("delegate".to_string()),
            tool_description: Some("Delegate work".to_string()),
            input_schema: Some(json!({"type": "object"})),
            max_turns: Some(5),
            background: false,
            allowed_tools: Some(vec!["read_file".to_string()]),
        };

        let hooks = AgentHooks {
            on_turn_start: Some(Arc::new(|_ctx| Box::pin(async {}))),
            on_turn_end: None,
            on_tool_start: None,
            on_tool_end: None,
        };

        let agent = Agent::builder("full-agent")
            .instructions(Instructions::Static("Be helpful.".into()))
            .model("claude-sonnet-4-20250514")
            .tool(tool)
            .sub_agent(sub_def)
            .input_guardrail(input_guard)
            .output_guardrail(output_guard)
            .output_schema(json!({"type": "object", "properties": {"answer": {"type": "string"}}}))
            .max_turns(10)
            .hooks(hooks)
            .build();

        assert_eq!(agent.name, "full-agent");
        assert!(matches!(agent.instructions, Instructions::Static(ref s) if s == "Be helpful."));
        assert_eq!(agent.model, Some("claude-sonnet-4-20250514".to_string()));
        assert_eq!(agent.tools.len(), 1);
        assert_eq!(agent.tools[0].name(), "echo");
        assert_eq!(agent.sub_agents.len(), 1);
        assert_eq!(agent.sub_agents[0].tool_name, Some("delegate".to_string()));
        assert_eq!(agent.input_guardrails.len(), 1);
        assert_eq!(agent.output_guardrails.len(), 1);
        assert!(agent.output_schema.is_some());
        assert_eq!(agent.max_turns, Some(10));
        assert!(agent.hooks.on_turn_start.is_some());
    }

    #[test]
    fn agent_builder_additive_tools() {
        let tool1: Arc<dyn Tool> = Arc::new(MockTool {
            tool_name: "tool1".to_string(),
        });
        let tool2: Arc<dyn Tool> = Arc::new(MockTool {
            tool_name: "tool2".to_string(),
        });
        let tool3: Arc<dyn Tool> = Arc::new(MockTool {
            tool_name: "tool3".to_string(),
        });

        let agent = Agent::builder("multi-tool")
            .tool(tool1)
            .tool(tool2)
            .tool(tool3)
            .build();

        assert_eq!(agent.tools.len(), 3);
        assert_eq!(agent.tools[0].name(), "tool1");
        assert_eq!(agent.tools[1].name(), "tool2");
        assert_eq!(agent.tools[2].name(), "tool3");
    }

    #[test]
    fn agent_builder_additive_sub_agents() {
        let sub1 = SubAgentDef {
            agent: Arc::new(Agent::builder("sub1").build()),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let sub2 = SubAgentDef {
            agent: Arc::new(Agent::builder("sub2").build()),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: Some(3),
            background: true,
            allowed_tools: None,
        };

        let agent = Agent::builder("parent")
            .sub_agent(sub1)
            .sub_agent(sub2)
            .build();

        assert_eq!(agent.sub_agents.len(), 2);
        assert_eq!(agent.sub_agents[0].agent.name, "sub1");
        assert_eq!(agent.sub_agents[1].agent.name, "sub2");
        assert!(agent.sub_agents[1].background);
    }

    #[test]
    fn agent_builder_additive_guardrails() {
        let g1: Arc<dyn InputGuardrail> = Arc::new(MockInputGuardrail);
        let g2: Arc<dyn InputGuardrail> = Arc::new(MockInputGuardrail);
        let o1: Arc<dyn OutputGuardrail> = Arc::new(MockOutputGuardrail);

        let agent = Agent::builder("guarded")
            .input_guardrail(g1)
            .input_guardrail(g2)
            .output_guardrail(o1)
            .build();

        assert_eq!(agent.input_guardrails.len(), 2);
        assert_eq!(agent.output_guardrails.len(), 1);
    }

    #[test]
    fn instructions_static_debug() {
        let inst = Instructions::Static("Hello".to_string());
        let debug = format!("{:?}", inst);
        assert!(debug.contains("Static"));
        assert!(debug.contains("Hello"));
    }

    #[test]
    fn instructions_dynamic_debug() {
        let inst =
            Instructions::Dynamic(Arc::new(|_ctx| Box::pin(async { "dynamic".to_string() })));
        let debug = format!("{:?}", inst);
        assert!(debug.contains("Dynamic"));
        assert!(debug.contains("<fn>"));
    }

    #[test]
    fn instructions_static_clone() {
        let inst = Instructions::Static("test".to_string());
        let cloned = inst.clone();
        assert!(matches!(cloned, Instructions::Static(ref s) if s == "test"));
    }

    #[test]
    fn instructions_dynamic_clone() {
        let inst = Instructions::Dynamic(Arc::new(|_ctx| Box::pin(async { "hi".to_string() })));
        let cloned = inst.clone();
        assert!(matches!(cloned, Instructions::Dynamic(_)));
    }

    #[test]
    fn instructions_default_is_empty_static() {
        let inst = Instructions::default();
        assert!(matches!(inst, Instructions::Static(ref s) if s.is_empty()));
    }

    #[test]
    fn agent_hooks_default() {
        let hooks = AgentHooks::default();
        assert!(hooks.on_turn_start.is_none());
        assert!(hooks.on_turn_end.is_none());
        assert!(hooks.on_tool_start.is_none());
        assert!(hooks.on_tool_end.is_none());
    }

    #[test]
    fn agent_hooks_debug() {
        let hooks = AgentHooks {
            on_turn_start: Some(Arc::new(|_ctx| Box::pin(async {}))),
            on_turn_end: None,
            on_tool_start: None,
            on_tool_end: Some(Arc::new(|_ctx| Box::pin(async {}))),
        };
        let debug = format!("{:?}", hooks);
        assert!(debug.contains("on_turn_start: true"));
        assert!(debug.contains("on_turn_end: false"));
        assert!(debug.contains("on_tool_start: false"));
        assert!(debug.contains("on_tool_end: true"));
    }

    #[test]
    fn agent_hooks_clone() {
        let hooks = AgentHooks {
            on_turn_start: Some(Arc::new(|_ctx| Box::pin(async {}))),
            on_turn_end: None,
            on_tool_start: None,
            on_tool_end: None,
        };
        let cloned = hooks.clone();
        assert!(cloned.on_turn_start.is_some());
        assert!(cloned.on_turn_end.is_none());
    }

    #[test]
    fn sub_agent_def_debug() {
        let sub = SubAgentDef {
            agent: Arc::new(Agent::builder("helper").build()),
            tool_name: Some("help".to_string()),
            tool_description: Some("Get help".to_string()),
            input_schema: None,
            max_turns: Some(5),
            background: true,
            allowed_tools: Some(vec!["shell".to_string()]),
        };
        let debug = format!("{:?}", sub);
        assert!(debug.contains("helper"));
        assert!(debug.contains("help"));
        assert!(debug.contains("true")); // background
    }

    #[test]
    fn sub_agent_def_clone() {
        let sub = SubAgentDef {
            agent: Arc::new(Agent::builder("cloneable").build()),
            tool_name: None,
            tool_description: None,
            input_schema: None,
            max_turns: None,
            background: false,
            allowed_tools: None,
        };
        let cloned = sub.clone();
        assert_eq!(cloned.agent.name, "cloneable");
        assert!(!cloned.background);
    }

    #[test]
    fn agent_debug() {
        let agent = Agent::builder("debug-test")
            .model("gpt-4")
            .max_turns(5)
            .build();
        let debug = format!("{:?}", agent);
        assert!(debug.contains("Agent"));
        assert!(debug.contains("debug-test"));
        assert!(debug.contains("gpt-4"));
        assert!(debug.contains("5"));
    }

    #[test]
    fn agent_clone() {
        let tool: Arc<dyn Tool> = Arc::new(MockTool {
            tool_name: "t".to_string(),
        });
        let agent = Agent::builder("cloneable-agent")
            .model("m")
            .tool(tool)
            .max_turns(3)
            .build();
        let cloned = agent.clone();
        assert_eq!(cloned.name, "cloneable-agent");
        assert_eq!(cloned.model, Some("m".to_string()));
        assert_eq!(cloned.tools.len(), 1);
        assert_eq!(cloned.max_turns, Some(3));
    }

    #[test]
    fn agent_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Agent>();
        assert_sync::<Agent>();
    }

    #[test]
    fn agent_builder_model_accepts_string_and_str() {
        let agent1 = Agent::builder("a").model("model-name").build();
        let agent2 = Agent::builder("b").model(String::from("model-name")).build();
        assert_eq!(agent1.model, agent2.model);
    }

    #[tokio::test]
    async fn instructions_dynamic_can_be_invoked() {
        let inst = Instructions::Dynamic(Arc::new(|ctx| {
            let run_id = ctx.state.run_id.clone();
            Box::pin(async move { format!("Instructions for run: {}", run_id) })
        }));

        if let Instructions::Dynamic(f) = &inst {
            let ctx = RunContext {
                state: RunState::new("test-run-42".into(), None, None),
            };
            let result = f(&ctx).await;
            assert_eq!(result, "Instructions for run: test-run-42");
        } else {
            panic!("expected Dynamic variant");
        }
    }
}
