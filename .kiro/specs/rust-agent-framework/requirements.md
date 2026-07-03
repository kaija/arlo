# Requirements Document

## Introduction

This document specifies the requirements for a Rust-native autonomous agent framework (`arlo-rust`). The framework implements a streaming-first main loop inspired by Claude-Code's architecture combined with OpenAI Agents JS's composability patterns. It is designed as a skeleton/scaffold that establishes the crate structure, core traits, types, and main loop — compilable with placeholder implementations where needed.

The framework targets high-performance, memory-safe agent execution with serializable state for pause/resume, model-agnostic provider abstraction, concurrent tool execution during streaming, multi-stage context compaction, composable guardrails, and hierarchical sub-agent delegation.

## Glossary

- **Agent**: The top-level configuration struct defining an autonomous agent's behavior, tools, instructions, and sub-agents.
- **RunLoop**: The core async while-loop that streams model responses, executes tools concurrently, resolves next steps, and applies state transitions.
- **NextStep**: A discriminated enum controlling the RunLoop's state transitions (Continue, FinalOutput, Interruption, Recovery, BudgetContinue, MaxTurns, Aborted).
- **RunState**: The serializable snapshot of a run's full state, enabling pause/resume at any point.
- **StreamChunk**: The canonical enum representing a single piece of streaming model output (TextDelta, ToolUseStart, ToolUseEnd, etc.).
- **ModelProvider**: A trait for resolving model name strings to usable Model instances.
- **Model**: A trait representing a resolved LLM capable of streaming responses.
- **Tool**: A trait defining a callable tool with schema, concurrency classification, approval requirements, and execution logic.
- **StreamingToolExecutor**: The component that enqueues and executes tools concurrently during model streaming, respecting concurrency classifications.
- **ContextCompactor**: The component that manages multi-stage message history compaction to keep context within token limits.
- **PermissionEngine**: The 4-layer decision pipeline that evaluates whether a tool call is permitted before execution.
- **Guardrail**: Traits (InputGuardrail, OutputGuardrail, ToolGuardrail) for composable safety checks at input, output, and tool boundaries.
- **SubAgent**: An isolated agent spawned by a parent agent via a tool call, running its own RunLoop with fresh message history.
- **SkillRegistry**: A registry of reusable prompt templates (loaded from SKILL.md files) that the model can invoke via a SkillTool.
- **MCPServer**: A Model Context Protocol server connection (stdio, HTTP, or SSE transport) that exposes remote tools.
- **RunEvent**: Events yielded by the RunLoop stream (TurnStart, StreamChunk, ToolStart, ToolEnd, Compaction, Interruption, AgentEnd, etc.).
- **CompactionStage**: A single step in the multi-stage context compaction pipeline (Snip, TruncateToolResults, AutoSummarize, Custom).
- **ApprovalDecision**: The user's response to a permission prompt (Approve, Reject, AlwaysAllow).
- **RecoveryStrategy**: An enum of error recovery approaches (CompactAndRetry, EscalateOutputTokens, ContinueMessage, FallbackModel, GiveUp).
- **Workspace**: The Cargo workspace containing the crate structure (agent-core, agent-llm, agent-tools, agent-mcp, agent-cli).

## Requirements

### Requirement 1: Cargo Workspace Structure

**User Story:** As a developer, I want a well-organized Cargo workspace with separate crates, so that I can build, test, and depend on individual components independently.

#### Acceptance Criteria

1. THE Workspace SHALL define a Cargo workspace with five member crates: agent-core, agent-llm, agent-tools, agent-mcp, and agent-cli
2. THE agent-core crate SHALL compile as a library crate with no direct dependency on agent-llm, agent-tools, agent-mcp, or agent-cli
3. THE agent-llm crate SHALL depend on agent-core for trait definitions
4. THE agent-tools crate SHALL depend on agent-core for the Tool trait
5. THE agent-mcp crate SHALL depend on agent-core for MCP-related traits
6. THE agent-cli crate SHALL depend on agent-core and agent-llm and compile as a binary crate
7. WHEN `cargo build --workspace` is executed, THE Workspace SHALL compile without errors
8. WHEN `cargo test --workspace` is executed, THE Workspace SHALL run all tests without errors
9. WHEN `cargo build -p <crate-name>` is executed for any single member crate, THE Workspace SHALL compile that crate and its dependencies without errors

### Requirement 2: Core Message Types

**User Story:** As a framework developer, I want canonical message types that represent the conversation history, so that all components share a single message format.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a Message enum with variants: System (containing content: String), User (containing content: Vec<ContentBlock>), Assistant (containing content: Vec<ContentBlock> and usage: Option<Usage>), and ToolResult (containing tool_use_id: String, content: String, and is_error: bool)
2. THE Message enum SHALL derive Serialize, Deserialize, Debug, and Clone
3. THE agent-core crate SHALL define a ContentBlock enum with variants: Text (containing a String), Image (containing media_type: String, data: String, and source_type: String), and ToolUse (containing a ToolUseBlock)
4. THE agent-core crate SHALL define a ToolUseBlock struct containing id (String), name (String), and input (serde_json::Value)
5. THE agent-core crate SHALL define a Usage struct with fields: input_tokens (u64), output_tokens (u64), and cache_read_tokens (Option<u64>)
6. FOR ALL valid Message values, serializing to JSON then deserializing from JSON SHALL produce an equivalent Message (round-trip property)

### Requirement 3: StreamChunk Canonical Type

**User Story:** As a framework developer, I want a single canonical streaming chunk type, so that provider-specific streaming formats are normalized before reaching the main loop.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a StreamChunk enum with variants: TextDelta, ThinkingDelta, ToolUseStart, ToolUseInputDelta, ToolUseEnd, and MessageStop
2. THE TextDelta variant SHALL contain a text field of type String
3. THE ThinkingDelta variant SHALL contain a text field of type String
4. THE ToolUseStart variant SHALL contain id (String) and name (String) fields
5. THE ToolUseInputDelta variant SHALL contain id (String) and delta (String) fields
6. THE ToolUseEnd variant SHALL contain id (String) and input (serde_json::Value) fields
7. THE MessageStop variant SHALL contain stop_reason (StopReason) and usage (Usage) fields
8. THE StopReason enum SHALL define variants: EndTurn, ToolUse, MaxTokens, StopSequence, and ContentFilter
9. THE StreamChunk enum SHALL derive Serialize, Deserialize, Debug, and Clone

### Requirement 4: Model Provider Trait

**User Story:** As a framework developer, I want a model-agnostic provider abstraction, so that the main loop works with any LLM backend without code changes.

#### Acceptance Criteria

1. THE agent-core crate SHALL define an async ModelProvider trait with a `resolve` method that accepts a model name string and returns a Result<Arc<dyn Model>, ModelError>, where the trait requires Send + Sync bounds
2. THE agent-core crate SHALL define an async Model trait (requiring Send + Sync) with a `stream` method that accepts a ModelRequest and returns a Result<ModelStream, ModelError>
3. THE Model trait SHALL define methods: name() returning &str, provider() returning &str, context_window() returning usize, max_output_tokens() returning usize, supports_tools() returning bool, input_cost_per_million() returning f64, and output_cost_per_million() returning f64
4. THE ModelStream type SHALL be defined as Pin<Box<dyn Stream<Item = Result<StreamChunk, ModelError>> + Send>>
5. THE ModelRequest struct SHALL contain fields: system (String), messages (Vec<Message>), tools (Vec<ToolDefinition>), max_tokens (Option<u32>), temperature (Option<f32>), and output_schema (Option<serde_json::Value>)

### Requirement 5: Tool Trait

**User Story:** As a tool author, I want a well-defined Tool trait with concurrency classification and approval support, so that I can implement tools that the framework executes safely.

#### Acceptance Criteria

1. THE agent-core crate SHALL define an async Tool trait with methods: name(), description(), parameters_schema(), concurrency(), approval_requirement(), and execute()
2. THE execute method SHALL accept input (serde_json::Value) and ctx (&ToolContext) and return Result<ToolOutput, ToolError>, where ToolContext provides at minimum a session identifier and a working directory path
3. THE Tool trait SHALL define a concurrency() method that accepts input (&serde_json::Value) and returns a Concurrency enum value (Safe or Exclusive), where Safe permits parallel execution with other Safe tools and Exclusive requires sole execution
4. THE Tool trait SHALL provide default implementations for timeout() returning None (Option<Duration>), error_cascades() returning false, is_enabled() returning true, and approval_requirement() returning ApprovalRequirement::Never
5. THE ToolOutput enum SHALL define variants: Text(String), Structured(serde_json::Value), and Error(String)
6. THE agent-core crate SHALL define ApprovalRequirement enum with variants: Never, Always, and Conditional(String) where the String describes the condition
7. THE agent-core crate SHALL define ToolError enum with variants: InvalidInput(String), ExecutionFailed(String), Timeout, and NotAvailable(String)

### Requirement 6: NextStep State Machine

**User Story:** As a framework developer, I want an explicit state machine controlling the main loop, so that control flow is clear and all exit conditions are handled.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a NextStep enum with variants: Continue, FinalOutput, Interruption, Recovery, BudgetContinue, MaxTurns, and Aborted
2. THE Continue variant SHALL contain a reason field of type ContinueReason
3. THE FinalOutput variant SHALL contain text (String) and structured (Option<serde_json::Value>) fields
4. THE Interruption variant SHALL contain a pending field of type Vec<PendingApproval>
5. THE Recovery variant SHALL contain a strategy field of type RecoveryStrategy
6. THE MaxTurns variant SHALL contain a count field of type u32
7. THE Aborted variant SHALL contain a reason field of type String
8. THE NextStep enum SHALL derive Debug, Clone, and PartialEq
9. THE BudgetContinue variant SHALL contain a remaining_turns field of type u32 and a reason field of type String

### Requirement 7: RunState Serializable State

**User Story:** As a framework user, I want the run state to be fully serializable, so that I can pause a run, persist it, and resume later.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a RunState struct that derives Serialize, Deserialize, and PartialEq
2. THE RunState SHALL contain fields: run_id (String), session_id (Option<String>), messages (Vec<Message>), current_turn (u32), max_turns (Option<u32>), total_cost_usd (f64), total_usage (Usage), pending_approvals (Vec<PendingApproval>), compaction_state (CompactionState), trace_id (String), and schema_version (String)
3. THE RunState SHALL implement a serialize() method that returns a Result containing a byte vector (via serde_json) on success or a serialization error on failure
4. THE RunState SHALL implement a deserialize() method that returns a Result containing a reconstructed RunState on success or a deserialization error on failure when given a byte slice
5. IF deserialize() receives malformed bytes or bytes with an unrecognized schema_version, THEN THE agent-core crate SHALL return a typed error indicating the failure reason without panicking
6. FOR ALL valid RunState values, serializing then deserializing SHALL produce a RunState that is equal via the derived PartialEq implementation (round-trip property)
7. THE RunState schema_version field SHALL follow semantic versioning format (MAJOR.MINOR.PATCH) and SHALL be set to the crate's current serialization schema version upon construction

### Requirement 8: Agent Configuration

**User Story:** As a framework user, I want a builder-pattern Agent struct, so that I can declaratively configure agents with instructions, tools, and sub-agents.

#### Acceptance Criteria

1. THE agent-core crate SHALL define an Agent struct with fields: name (String), instructions (Instructions enum), model (Option<String>), tools (Vec<Arc<dyn Tool>>), sub_agents (Vec<SubAgentDef>), input_guardrails (Vec<Arc<dyn InputGuardrail>>), output_guardrails (Vec<Arc<dyn OutputGuardrail>>), output_schema (Option<serde_json::Value>), max_turns (Option<u32>), and hooks (AgentHooks)
2. THE Agent struct SHALL implement a builder() method returning an AgentBuilder that requires the agent name (String) as its sole parameter
3. THE AgentBuilder SHALL provide chainable setter methods for each Agent field, where collection-typed fields (tools, sub_agents, input_guardrails, output_guardrails) use additive methods that append to the existing collection
4. THE AgentBuilder SHALL implement a build() method that consumes the builder and returns an Agent, where all Option-typed fields default to None, all Vec-typed fields default to empty, and instructions defaults to Static with an empty string
5. THE Instructions enum SHALL define variants: Static(String) and Dynamic(Box<dyn Fn(&RunContext) -> BoxFuture<String> + Send + Sync>)

### Requirement 9: Main Loop (RunLoop)

**User Story:** As a framework developer, I want a streaming-first main loop that yields events as an async Stream, so that callers can process events as they arrive.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a `run` async function that accepts an Agent reference, input, and RunConfig and returns Result<RunResult, RunError>
2. THE agent-core crate SHALL define a `run_stream` async function that returns a RunStream (Pin<Box<dyn Stream<Item = RunEvent> + Send>>)
3. THE RunLoop SHALL execute phases in order: context compaction, request preparation, model streaming with tool execution, drain remaining tools, resolve NextStep, and apply state transition
4. WHEN the NextStep resolves to Continue, THE RunLoop SHALL append assistant and tool result messages to state and loop back
5. WHEN the NextStep resolves to FinalOutput and output guardrails pass, THE RunLoop SHALL yield AgentEnd before returning
6. IF output guardrails fail when NextStep resolves to FinalOutput, THEN THE RunLoop SHALL yield a GuardrailTripped event and return with a RunError
7. WHEN the NextStep resolves to Interruption, THE RunLoop SHALL store pending approvals in state and yield an Interruption event before returning
8. WHEN the NextStep resolves to MaxTurns, THE RunLoop SHALL yield a MaxTurns event and return
9. WHEN the NextStep resolves to Aborted, THE RunLoop SHALL yield an Aborted event and return
10. IF the model call or a tool execution returns an unrecoverable error during any turn, THEN THE RunLoop SHALL yield an error event and return with a RunError

### Requirement 10: Streaming Tool Executor

**User Story:** As a framework developer, I want tools to start executing during model streaming, so that latency is minimized by overlapping network and computation.

#### Acceptance Criteria

1. THE StreamingToolExecutor SHALL provide an enqueue() method that accepts a ToolUseBlock, an Arc<dyn Tool>, and a RunContext reference
2. WHEN a tool with Concurrency::Safe is enqueued and only Safe tools are currently executing and the number of executing tools is below max_concurrency, THE StreamingToolExecutor SHALL spawn the tool without awaiting completion of other tools
3. WHEN a tool with Concurrency::Exclusive is enqueued, THE StreamingToolExecutor SHALL wait until all currently executing tools complete before starting the Exclusive tool
4. THE StreamingToolExecutor SHALL provide a drain_completed() method that returns completed tool results in enqueue order
5. THE StreamingToolExecutor SHALL provide a next_remaining() async method that returns the next completed tool result when tools are pending, or returns None when no enqueued tools remain
6. WHEN a tool's error_cascades() returns true and the tool returns an Err result, THE StreamingToolExecutor SHALL cancel all sibling executing tools
7. THE StreamingToolExecutor SHALL accept a max_concurrency parameter with a minimum value of 1 and a default of 8
8. WHILE a Concurrency::Exclusive tool is executing, THE StreamingToolExecutor SHALL defer starting any other enqueued tools until the Exclusive tool completes

### Requirement 11: Context Compactor

**User Story:** As a framework developer, I want multi-stage context compaction, so that long-running agents can operate within model context window limits.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a ContextCompactor struct that accepts a CompactionConfig
2. THE CompactionConfig SHALL contain a stages field (Vec<CompactionStage>) and an optional summary_model field
3. THE CompactionStage enum SHALL define variants: Snip (with max_history_tokens), TruncateToolResults (with max_chars), AutoSummarize (with threshold_tokens and preserve_recent), and Custom (with a boxed trait object)
4. WHEN the Snip stage is active and message history exceeds max_history_tokens, THE ContextCompactor SHALL remove the oldest non-system messages until total token count is within the limit, preserving all system-role messages and the most recent user message
5. WHEN the TruncateToolResults stage is active and a tool result exceeds max_chars, THE ContextCompactor SHALL truncate the tool result content to max_chars and append a "[truncated]" suffix
6. WHEN the AutoSummarize stage is active and total token count exceeds threshold_tokens, THE ContextCompactor SHALL replace all messages older than the most recent preserve_recent messages with a single summary message, preserving system-role messages
7. THE ContextCompactor SHALL execute stages sequentially in the order defined in the stages Vec
8. THE ContextCompactor SHALL return an Option<CompactionEvent> where CompactionEvent contains the stage variant applied, messages affected, and token count before/after
9. IF no stage's activation threshold is met, THEN THE ContextCompactor SHALL return None without modifying the message history
10. IF the AutoSummarize stage is active and the summary_model field is None, THEN THE ContextCompactor SHALL skip the AutoSummarize stage

### Requirement 12: Permission Engine

**User Story:** As a framework user, I want a multi-layered permission system, so that dangerous tool calls require explicit approval before execution.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a PermissionEngine with a check() async method that accepts a tool name and tool call arguments and returns a PermissionDecision
2. THE PermissionDecision enum SHALL define variants: Allow (with optional reason), Deny (with message and reason), and NeedsApproval (with description, call_id, and context)
3. THE PermissionEngine SHALL evaluate Layer 1 (static allow/deny rules) before Layer 2 (tool-specific check) before Layer 3 (permission mode) before Layer 4 (interactive approval), stopping at the first definitive decision
4. WHEN a tool call's tool name matches a static allow rule, THE PermissionEngine SHALL return Allow without evaluating further layers
5. WHEN a tool call's tool name matches a static deny rule, THE PermissionEngine SHALL return Deny without evaluating further layers
6. IF PermissionMode is Bypass, THEN THE PermissionEngine SHALL return Allow for all tool calls that reach Layer 3
7. IF PermissionMode is DenyAll, THEN THE PermissionEngine SHALL return Deny for any tool call that reaches Layer 3
8. WHEN the user approves a tool call with an "always allow" option, THE PermissionEngine SHALL store the approved tool name as a session-scoped allow rule for the duration of the current run
9. IF interactive approval receives no response within 300 seconds, THEN THE PermissionEngine SHALL return Deny with a timeout indication
10. WHEN the user denies an interactive approval request, THE PermissionEngine SHALL return Deny and SHALL NOT re-prompt for the same tool call

### Requirement 13: Guardrail System

**User Story:** As a framework user, I want composable input, output, and tool guardrails, so that I can enforce safety policies at multiple boundaries.

#### Acceptance Criteria

1. THE agent-core crate SHALL define an async InputGuardrail trait with a check() method accepting a slice of messages and a RunContext reference, returning a GuardrailResult
2. THE agent-core crate SHALL define an async OutputGuardrail trait with a check() method accepting output text and a RunContext reference, returning a GuardrailResult
3. THE agent-core crate SHALL define an async ToolGuardrail trait with a check_input() method accepting tool name, arguments, and RunContext, and a check_output() method accepting tool name, result, and RunContext, each returning GuardrailResult
4. THE GuardrailResult struct SHALL contain fields: passed (bool), reason (Option<String>), and metadata (Option<serde_json::Value>)
5. WHEN an InputGuardrail returns passed=false on the first turn, THE RunLoop SHALL yield a GuardrailTripped event containing the guardrail name and reason, and terminate without invoking the model
6. WHEN an OutputGuardrail returns passed=false, THE RunLoop SHALL yield a GuardrailTripped event and terminate without delivering the output
7. WHEN a ToolGuardrail check_input() returns passed=false, THE RunLoop SHALL skip execution of that tool call and yield a GuardrailTripped event
8. WHEN multiple guardrails are registered, THE RunLoop SHALL execute them sequentially in registration order, stopping at the first that returns passed=false
9. THE RunLoop SHALL invoke InputGuardrails only on the first turn and SHALL NOT re-invoke them on subsequent turns

### Requirement 14: Sub-Agent System

**User Story:** As a framework user, I want to spawn isolated sub-agents via tool calls, so that complex tasks can be delegated to specialized agents without context leakage.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a SubAgentDef struct with fields: agent (Arc<Agent>), tool_name (Option<String>), tool_description (Option<String>), input_schema (Option<serde_json::Value>), max_turns (Option<u32>), background (bool), and allowed_tools (Option<Vec<String>>)
2. THE agent-core crate SHALL implement SubAgentTool as a struct implementing the Tool trait
3. WHEN the parent model invokes a SubAgentTool, THE SubAgentTool SHALL spawn an isolated RunLoop with fresh (empty) message history, using the tool call arguments as the sub-agent's initial user message
4. WHEN background is false, THE SubAgentTool SHALL await the sub-agent's completion and return the sub-agent's final output as ToolOutput
5. WHEN background is true, THE SubAgentTool SHALL spawn the sub-agent as a background Tokio task and return immediately with a ToolOutput containing the task identifier
6. THE sub-agent SHALL NOT have access to the parent's message history
7. THE sub-agent's token usage and cost SHALL be accumulated into the parent RunState's totals
8. IF the sub-agent reaches its configured max_turns limit, THEN THE SubAgentTool SHALL terminate the sub-agent's RunLoop and return the last output with an indication that the turn limit was reached
9. IF the sub-agent's RunLoop encounters an unrecoverable error, THEN THE SubAgentTool SHALL return a ToolOutput containing an error description

### Requirement 15: MCP Client Integration

**User Story:** As a framework user, I want to connect to MCP servers and use their tools, so that I can extend the agent's capabilities with remote tool providers.

#### Acceptance Criteria

1. THE agent-mcp crate SHALL define an async MCPServer trait with methods: name(), connect(), list_tools(), call_tool(), and close()
2. THE agent-mcp crate SHALL define an MCPTransport enum with variants: Stdio (command, args, env), Http (url, headers), and Sse (url)
3. THE agent-mcp crate SHALL provide a function to convert MCP tool definitions into Arc<dyn Tool> objects compatible with the agent-core Tool trait
4. WHEN an MCP server connection fails or does not establish within 30 seconds, THE MCPServer implementation SHALL return an MCPError including transport type, server name, and underlying cause
5. WHEN call_tool is invoked, THE MCPServer implementation SHALL send a JSON-RPC request and return the result as a serde_json::Value
6. IF call_tool receives an error response from the MCP server, THEN THE MCPServer implementation SHALL return an MCPError with the server name and error details
7. IF call_tool or list_tools is invoked before connect has succeeded, THEN THE MCPServer implementation SHALL return an MCPError indicating the server is not connected

### Requirement 16: Unified LLM Provider

**User Story:** As a framework user, I want a unified provider crate with feature-flag-gated backends, so that I can use OpenAI, Anthropic, or Ollama through a single interface.

#### Acceptance Criteria

1. THE agent-llm crate SHALL implement the ModelProvider trait as a UnifiedProvider struct
2. THE agent-llm crate SHALL gate provider backends behind feature flags: "openai", "anthropic", and "ollama"
3. THE UnifiedProvider SHALL implement a from_env() constructor that reads environment variables (OPENAI_API_KEY, ANTHROPIC_API_KEY, OLLAMA_HOST) and registers only those providers whose corresponding key or host is present
4. WHEN a model name contains a recognized provider prefix (e.g., "anthropic:claude-sonnet-4-20250514"), THE UnifiedProvider SHALL route to the specified provider
5. WHEN a model name has no prefix, THE UnifiedProvider SHALL route to the configured default_provider
6. THE agent-llm crate SHALL define per-provider convert modules that map canonical Message types to/from provider-specific wire formats
7. FOR ALL valid canonical Messages, converting to provider format then back SHALL preserve all content, role assignments, and tool-call structures (round-trip property)
8. IF from_env() finds no recognized API keys or host variables, THEN THE UnifiedProvider SHALL return an error indicating no provider could be configured
9. IF a model name prefix references an unavailable or disabled provider, THEN THE UnifiedProvider SHALL return an error identifying the unavailable provider
10. IF a model name has no prefix and no default_provider is configured, THEN THE UnifiedProvider SHALL return an error indicating no default provider is set

### Requirement 17: Error Hierarchy

**User Story:** As a framework developer, I want a structured error hierarchy using thiserror, so that errors are descriptive, typed, and ergonomic to handle.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a RunError enum deriving thiserror::Error with variants: Model(ModelError), Tool(ToolError), MaxTurns(u32), BudgetExceeded(f64), Guardrail(String), Serialization(String), MCP(String), Aborted(String), and RecoveryExhausted(u32)
2. THE agent-core crate SHALL define a ModelError enum with variants: Api (status, body), RateLimited (retry_after_ms), PromptTooLong (tokens), MaxOutputTokens, Connection(String), and StreamInterrupted(String)
3. THE agent-core crate SHALL define a ToolError enum with variants: Execution(tool_name, message), Timeout(tool_name), Validation(tool_name, message), Rejected(tool_name), and Cancelled(tool_name)
4. THE agent-core crate SHALL implement From<ModelError> for RunError and From<ToolError> for RunError to enable the ? operator
5. WHEN a RunError is displayed, THE error output SHALL include variant-specific context fields for identification without downcasting

### Requirement 18: Recovery System

**User Story:** As a framework developer, I want structured error recovery strategies, so that the main loop can automatically recover from transient errors without user intervention.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a RecoveryStrategy enum with variants: CompactAndRetry, EscalateOutputTokens (max), ContinueMessage (attempt), FallbackModel (model), and GiveUp (error)
2. WHEN a ModelError::PromptTooLong is encountered, THE RunLoop SHALL resolve a CompactAndRetry recovery strategy
3. WHEN a ModelError::MaxOutputTokens is encountered, THE RunLoop SHALL resolve a ContinueMessage recovery strategy
4. WHEN a Recovery NextStep is resolved and the strategy is CompactAndRetry, THE RunLoop SHALL reduce the message history to fit within the model's context limit and retry
5. WHEN a Recovery NextStep is resolved and the strategy is GiveUp, THE RunLoop SHALL yield an Error event and terminate
6. IF recovery has been attempted more than 3 times for the same ModelError variant within a single run, THEN THE RunLoop SHALL escalate to GiveUp
7. WHEN a Recovery NextStep is resolved and the strategy is ContinueMessage, THE RunLoop SHALL append a continuation prompt and retry with the attempt count incremented
8. WHEN a Recovery NextStep is resolved and the strategy is EscalateOutputTokens, THE RunLoop SHALL increase the max output token parameter to the specified max and retry
9. IF a ModelError has no configured recovery strategy mapping, THEN THE RunLoop SHALL resolve a GiveUp strategy

### Requirement 19: Tracing and Observability

**User Story:** As a framework user, I want OpenTelemetry-compatible tracing, so that I can observe agent execution with standard observability tools.

#### Acceptance Criteria

1. THE agent-core crate SHALL use the `tracing` crate for structured logging and span creation, producing spans conformant with OpenTelemetry trace semantics
2. WHEN a run is initiated, THE RunLoop SHALL create a root span named "agent.run" with fields: run_id and agent name
3. WHEN a model call is made, THE RunLoop SHALL create a child span named "model.call" with the model name as a field
4. WHEN a tool is executed, THE StreamingToolExecutor SHALL create a child span named "tool.execute" with the tool name as a field
5. WHEN a sub-agent is spawned, THE SubAgentTool SHALL create a child span named "sub_agent" with the sub-agent name as a field
6. IF an operation within a span results in an error, THEN the span status SHALL be set to error with the error description as an attribute

### Requirement 20: Skill System

**User Story:** As a framework user, I want a skill registry that loads reusable prompt templates from SKILL.md files, so that common workflows can be packaged and invoked by the model.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a Skill struct with fields: name, description, when_to_use (Option), allowed_tools (Option), arguments (Option), context (SkillContext enum), hooks (Option), body (String), base_dir (PathBuf), and source (SkillSource enum)
2. THE SkillContext enum SHALL define variants: Inline and Fork (with optional max_turns)
3. THE agent-core crate SHALL define a SkillRegistry struct with methods: load(), find(name: &str) -> Option<&Skill>, activate_for_path(path: &Path), and system_prompt_section() -> String
4. THE SkillRegistry SHALL load skills from project-level (.agent/skills/) and user-level (~/.agent/skills/) directories, where project-level skills take precedence when names conflict
5. WHEN a skill with SkillContext::Inline is invoked, THE SkillTool SHALL return the rendered prompt body as a ToolOutput::Text
6. WHEN a skill with SkillContext::Fork is invoked, THE SkillTool SHALL spawn a sub-agent constrained to the skill's allowed_tools with separate conversation history
7. THE SkillTool SHALL substitute template variables ($ARGUMENTS, $1, $2, ${SKILL_DIR}) in the skill body, leaving unresolved positional variables as empty strings
8. WHEN loading a SKILL.md file, THE registry SHALL parse YAML frontmatter (delimited by ---) for metadata and treat remaining content as the skill body
9. IF a SKILL.md file contains invalid frontmatter or is missing the required name field, THEN THE SkillRegistry SHALL skip that file and log a warning

### Requirement 21: Run Events and Streaming API

**User Story:** As a framework user, I want a rich event stream from the main loop, so that I can build responsive UIs and logging on top of the framework.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a RunEvent enum with variants: TurnStart, StreamChunk, ToolStart, ToolEnd, SubAgentStart, SubAgentEnd, Compaction, StepResolved, AgentEnd, Interruption, GuardrailTripped, MaxTurns, Aborted, and Error
2. THE RunStream type SHALL be defined as Pin<Box<dyn Stream<Item = RunEvent> + Send>>
3. WHEN the RunLoop begins a new turn, THE RunLoop SHALL yield a TurnStart event with the turn number (starting at 1) and agent name
4. WHEN a StreamChunk is received from the model, THE RunLoop SHALL yield a StreamChunk event containing the text delta
5. WHEN a tool begins execution, THE RunLoop SHALL yield a ToolStart event with the tool id and name
6. WHEN a tool completes execution, THE RunLoop SHALL yield a ToolEnd event with the tool id, name, and output
7. WHEN a sub-agent starts, THE RunLoop SHALL yield a SubAgentStart event; when complete, a SubAgentEnd event
8. THE RunLoop SHALL emit exactly one terminal event (AgentEnd, MaxTurns, Aborted, or Error) to close the stream
9. THE RunLoop SHALL guarantee ToolStart is always emitted before the corresponding ToolEnd for the same tool id

### Requirement 22: RunConfig and Entry Points

**User Story:** As a framework user, I want clear entry point functions with a configuration struct, so that I can start runs with sensible defaults while customizing behavior.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a RunConfig struct with fields: provider (Arc<dyn ModelProvider>), permissions (PermissionEngine), compaction (CompactionConfig), max_output_tokens (Option<u32>), temperature (Option<f32>), concurrency_limit (usize), and approval_handler (Option<Arc<dyn ApprovalHandler>>)
2. THE RunConfig SHALL provide a builder that requires provider and permissions, defaulting concurrency_limit to 10, max_output_tokens to None, temperature to None, compaction to CompactionConfig::default(), and approval_handler to None
3. IF temperature is provided, THEN THE RunConfig builder SHALL reject values outside the range 0.0 to 2.0
4. THE agent-core crate SHALL define an Input enum with variants: Fresh(String), Items(Vec<Message>), and Resume(Vec<u8>)
5. THE agent-core crate SHALL define a RunResult struct containing: output (String), structured (Option<serde_json::Value>), usage (Usage), cost_usd (f64), turns (u32), and state (RunState)

### Requirement 23: Built-in Tools

**User Story:** As a framework user, I want built-in tools for shell execution, file operations, and search, so that coding agents have essential capabilities out of the box.

#### Acceptance Criteria

1. THE agent-tools crate SHALL implement a ShellTool struct that implements the Tool trait and executes shell commands
2. THE agent-tools crate SHALL implement a FileReadTool struct that reads file contents
3. THE agent-tools crate SHALL implement a FileWriteTool struct that writes content to files
4. THE agent-tools crate SHALL implement a GlobTool struct that finds files matching glob patterns and classifies its concurrency as Safe
5. THE agent-tools crate SHALL implement a GrepTool struct that searches file contents with regex patterns and classifies its concurrency as Safe
6. THE ShellTool SHALL classify its concurrency as Exclusive
7. THE FileReadTool SHALL classify its concurrency as Safe
8. THE FileWriteTool SHALL classify its concurrency as Exclusive
9. IF a shell command exits with a non-zero exit code, THEN THE ShellTool SHALL return a ToolOutput::Error containing the combined stdout and stderr
10. THE ShellTool SHALL define a default timeout of 300 seconds

### Requirement 24: CLI Binary

**User Story:** As an end user, I want a CLI binary that starts an interactive agent session, so that I can use the framework from my terminal.

#### Acceptance Criteria

1. THE agent-cli crate SHALL compile to a single binary named "arlo"
2. THE agent-cli crate SHALL accept an optional --model flag to specify the LLM model
3. THE agent-cli crate SHALL read API keys from environment variables (OPENAI_API_KEY, ANTHROPIC_API_KEY)
4. WHEN launched without arguments, THE agent-cli SHALL start an interactive REPL-style session reading from stdin and printing to stdout
5. WHEN launched with a prompt argument, THE agent-cli SHALL execute that prompt, print the response, and exit with code 0 on success or non-zero on failure
6. IF the required API key for the selected model is not set, THEN THE agent-cli SHALL exit with a non-zero code and display which variable is missing
7. IF --model specifies an unrecognized model, THEN THE agent-cli SHALL exit with a non-zero code and display an error

### Requirement 25: Retry and Fallback Logic

**User Story:** As a framework developer, I want configurable retry with exponential backoff and provider fallback, so that transient API errors are handled gracefully.

#### Acceptance Criteria

1. THE agent-llm crate SHALL define a RetryConfig struct with fields: max_retries (u32, default 3), initial_backoff_ms (u64, default 1000), max_backoff_ms (u64, default 30000), backoff_multiplier (f64, default 2.0), and retryable_statuses (Vec<u16>, default [429, 500, 502, 503, 529])
2. WHEN a model API returns a retryable HTTP status, THE agent-llm provider SHALL retry using backoff delay = min(initial_backoff_ms × backoff_multiplier^(attempt-1), max_backoff_ms) with random jitter of 0–25%, up to max_retries attempts
3. WHEN all retries are exhausted and a fallback_chain is configured, THE UnifiedProvider SHALL attempt the request with the next model in the chain
4. WHEN a rate-limit response includes a Retry-After header, THE provider SHALL wait at least that duration, capped at max_backoff_ms
5. IF all retries and fallback models are exhausted, THEN THE UnifiedProvider SHALL return an error indicating which providers were attempted and the final failure reason

### Requirement 26: Token Counting and Cost Calculation

**User Story:** As a framework user, I want accurate token counting and cost tracking, so that I can monitor and budget agent execution costs.

#### Acceptance Criteria

1. THE agent-core crate SHALL define a Usage struct with fields: input_tokens, output_tokens, and cache_read_tokens, each as unsigned integers
2. WHEN a turn completes, THE RunState SHALL add the turn's Usage values to the total_usage fields
3. WHEN a turn completes, THE RunState SHALL compute the turn's monetary cost using the model's pricing rates and add it to total_cost_usd
4. WHEN the total_cost_usd exceeds the configured budget after a turn, THE RunLoop SHALL resolve a NextStep::Aborted with reason "budget_exceeded" before starting the next turn
5. IF no budget is configured, THEN THE RunLoop SHALL skip budget enforcement
6. IF the model provider does not return usage data, THEN THE RunState SHALL treat all token counts as zero for that turn

### Requirement 27: Feature Flags for Optional Dependencies

**User Story:** As a framework user, I want optional dependencies gated behind feature flags, so that I only compile what I need and keep binary size minimal.

#### Acceptance Criteria

1. THE agent-llm crate SHALL define feature flags: "openai" (default), "anthropic" (default), "ollama" (optional), and "all-providers" which enables all provider features
2. IF the "openai" feature is disabled, THEN THE agent-llm crate SHALL exclude OpenAI modules from compilation and exclude provider-specific dependencies not needed by other enabled features
3. IF the "anthropic" feature is disabled, THEN THE agent-llm crate SHALL exclude Anthropic modules from compilation
4. IF no provider features are enabled, THEN THE agent-llm crate SHALL compile successfully exposing only core types and traits
5. IF "all-providers" is enabled, THEN THE agent-llm crate SHALL compile and expose all provider modules
