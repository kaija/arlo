# Implementation Plan: Rust Agent Framework

## Overview

Build a Rust-native autonomous agent framework (`arlo-rust`) as a Cargo workspace with five crates: `agent-core`, `agent-llm`, `agent-tools`, `agent-mcp`, and `agent-cli`. Implementation proceeds from foundational types upward: workspace scaffolding → core types → traits → main loop → tool executor → compaction/permissions/guardrails → sub-agents → LLM providers → built-in tools → MCP → CLI. Each task produces compilable code that subsequent tasks build on.

## Tasks

- [x] 1. Set up Cargo workspace and core type foundations
  - [x] 1.1 Create Cargo workspace with five member crates
    - Create root `Cargo.toml` declaring workspace members: `crates/agent-core`, `crates/agent-llm`, `crates/agent-tools`, `crates/agent-mcp`, `crates/agent-cli`
    - Set up each crate with its own `Cargo.toml` and `src/lib.rs` (or `src/main.rs` for agent-cli)
    - Configure dependencies: agent-llm depends on agent-core, agent-tools depends on agent-core, agent-mcp depends on agent-core, agent-cli depends on agent-core and agent-llm
    - Add shared workspace dependencies in root Cargo.toml: serde, serde_json, tokio, thiserror, async-trait, futures, tracing, uuid, proptest (dev)
    - Verify `cargo build --workspace` compiles without errors
    - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7_

  - [x] 1.2 Define core Message types and ContentBlock in agent-core
    - Implement `Message` enum with variants: System, User, Assistant, ToolResult per design
    - Implement `ContentBlock` enum with variants: Text, Image, ToolUse
    - Implement `ToolUseBlock` struct with id, name, input fields
    - Implement `Usage` struct with input_tokens, output_tokens, cache_read_tokens
    - Derive Serialize, Deserialize, Debug, Clone, PartialEq on all types
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5_

  - [x] 1.3 Write property test for Message serialization round-trip
    - **Property 1: Core type serialization round-trip**
    - **Validates: Requirements 2.6, 3.9**
    - Use proptest to generate arbitrary Message and StreamChunk values
    - Assert serialize → deserialize produces equal value

  - [x] 1.4 Define StreamChunk and StopReason in agent-core
    - Implement `StreamChunk` enum with variants: TextDelta, ThinkingDelta, ToolUseStart, ToolUseInputDelta, ToolUseEnd, MessageStop
    - Implement `StopReason` enum with variants: EndTurn, ToolUse, MaxTokens, StopSequence, ContentFilter
    - Derive Serialize, Deserialize, Debug, Clone, PartialEq on both enums
    - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7, 3.8, 3.9_

  - [x] 1.5 Define NextStep state machine enum in agent-core
    - Implement `NextStep` enum with variants: Continue, FinalOutput, Interruption, Recovery, BudgetContinue, MaxTurns, Aborted
    - Implement `ContinueReason`, `PendingApproval`, and `RecoveryStrategy` enums
    - Derive Debug, Clone, PartialEq on NextStep
    - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5, 6.6, 6.7, 6.8, 6.9_

  - [x] 1.6 Define error hierarchy using thiserror in agent-core
    - Implement `RunError` enum with all variants per design (Model, Tool, MaxTurns, BudgetExceeded, Guardrail, Serialization, MCP, Aborted, RecoveryExhausted)
    - Implement `ModelError` enum (Api, RateLimited, PromptTooLong, MaxOutputTokens, Connection, StreamInterrupted)
    - Implement `ToolError` enum (InvalidInput, ExecutionFailed, Timeout, NotAvailable)
    - Implement `From<ModelError> for RunError` and `From<ToolError> for RunError`
    - _Requirements: 17.1, 17.2, 17.3, 17.4, 17.5_

  - [x] 1.7 Write property test for RunError Display context
    - **Property 16: RunError Display includes context**
    - **Validates: Requirements 17.5**
    - Generate RunError variants with specific field values, assert Display output contains those values

- [x] 2. Core traits and RunState
  - [x] 2.1 Define ModelProvider and Model traits in agent-core
    - Implement async `ModelProvider` trait with `resolve()` and `available_models()` methods
    - Implement async `Model` trait with `stream()`, `complete()`, and metadata methods (name, provider, context_window, etc.)
    - Define `ModelStream` type alias as `Pin<Box<dyn Stream<Item = Result<StreamChunk, ModelError>> + Send>>`
    - Define `ModelRequest` struct with system, messages, tools, max_tokens, temperature, output_schema
    - Define `ModelResponse` struct for the non-streaming path
    - Define `ToolDefinition` struct used in ModelRequest
    - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5_

  - [x] 2.2 Define Tool trait and related types in agent-core
    - Implement async `Tool` trait with name(), description(), parameters_schema(), concurrency(), execute()
    - Implement `Concurrency` enum (Safe, Exclusive)
    - Implement `ToolContext` struct with session_id and working_dir
    - Implement `ToolOutput` enum (Text, Structured, Error)
    - Implement `ApprovalRequirement` enum (Never, Always, Conditional)
    - Provide default implementations for timeout(), error_cascades(), is_enabled(), approval_requirement()
    - _Requirements: 5.1, 5.2, 5.3, 5.4, 5.5, 5.6, 5.7_

  - [x] 2.3 Define RunState and serialization in agent-core
    - Implement `RunState` struct with all fields per design (run_id, session_id, messages, current_turn, max_turns, total_cost_usd, total_usage, pending_approvals, compaction_state, trace_id, schema_version)
    - Derive Serialize, Deserialize, PartialEq
    - Implement `serialize()` returning `Result<Vec<u8>, ...>` via serde_json
    - Implement `deserialize(bytes: &[u8])` returning `Result<RunState, ...>` with typed errors for malformed or unrecognized schema
    - Define `CompactionState` struct
    - Set schema_version to "1.0.0" on construction
    - _Requirements: 7.1, 7.2, 7.3, 7.4, 7.5, 7.7_

  - [x] 2.4 Write property test for RunState serialization round-trip
    - **Property 2: RunState serialization round-trip**
    - **Validates: Requirements 7.6**
    - Generate arbitrary RunState instances, assert serialize → deserialize equality

  - [x] 2.5 Write property test for RunState deserialization robustness
    - **Property 3: RunState deserialization robustness**
    - **Validates: Requirements 7.5**
    - Generate arbitrary byte slices, assert RunState::deserialize returns Err without panicking

  - [x] 2.6 Define RunEvent enum and RunStream type in agent-core
    - Implement `RunEvent` enum with all variants per design (TurnStart, StreamChunk, ToolStart, ToolEnd, SubAgentStart, SubAgentEnd, Compaction, StepResolved, AgentEnd, Interruption, GuardrailTripped, MaxTurns, Aborted, Error)
    - Define `RunStream` type as `Pin<Box<dyn Stream<Item = RunEvent> + Send>>`
    - _Requirements: 21.1, 21.2_

  - [x] 2.7 Define RunConfig, Input, and RunResult in agent-core
    - Implement `RunConfig` struct with builder pattern (provider, permissions, compaction, max_output_tokens, temperature, concurrency_limit, approval_handler)
    - Validate temperature range 0.0–2.0 in builder
    - Implement `Input` enum (Fresh, Items, Resume)
    - Implement `RunResult` struct (output, structured, usage, cost_usd, turns, state)
    - Define `ApprovalHandler` trait for interactive permission prompts
    - _Requirements: 22.1, 22.2, 22.3, 22.4, 22.5_

  - [x] 2.8 Write property test for temperature validation
    - **Property 20: Temperature validation**
    - **Validates: Requirements 22.3**
    - Generate arbitrary f32 values, assert values outside [0.0, 2.0] are rejected by the RunConfig builder

- [x] 3. Checkpoint - Verify foundation compiles
  - Ensure all tests pass, ask the user if questions arise.

- [x] 4. Agent configuration and guardrail traits
  - [x] 4.1 Define Agent struct and AgentBuilder in agent-core
    - Implement `Agent` struct with fields: name, instructions, model, tools, sub_agents, input_guardrails, output_guardrails, output_schema, max_turns, hooks
    - Implement `AgentBuilder` with builder() requiring name, chainable setters for all fields
    - Implement `Instructions` enum (Static, Dynamic)
    - Implement `AgentHooks` struct with optional lifecycle callbacks
    - Implement `SubAgentDef` struct
    - Collection-typed fields use additive builder methods (e.g., `tool()`, `sub_agent()`)
    - Ensure build() defaults Options to None, Vecs to empty, instructions to Static("")
    - _Requirements: 8.1, 8.2, 8.3, 8.4, 8.5_

  - [x] 4.2 Define guardrail traits in agent-core
    - Implement async `InputGuardrail` trait with check() method
    - Implement async `OutputGuardrail` trait with check() method
    - Implement async `ToolGuardrail` trait with check_input() and check_output() methods
    - Implement `GuardrailResult` struct (passed, reason, metadata)
    - _Requirements: 13.1, 13.2, 13.3, 13.4_

  - [x] 4.3 Define PermissionEngine in agent-core
    - Implement `PermissionEngine` struct with 4-layer evaluation pipeline
    - Implement `PermissionDecision` enum (Allow, Deny, NeedsApproval)
    - Implement `PermissionMode` enum (Bypass, Normal, DenyAll)
    - Implement static allow/deny rule configuration
    - Implement `check()` async method evaluating layers in order, short-circuiting at first definitive decision
    - Implement `grant_session_allow()` for "always allow" responses
    - _Requirements: 12.1, 12.2, 12.3, 12.4, 12.5, 12.6, 12.7, 12.8, 12.9, 12.10_

  - [x] 4.4 Write property test for permission engine static rules
    - **Property 10: Permission engine static rules short-circuit**
    - **Validates: Requirements 12.3, 12.4, 12.5, 12.6, 12.7**
    - Generate tool names and rule configurations, assert static allow/deny rules short-circuit correctly and mode rules apply at Layer 3

  - [x] 4.5 Write property test for guardrail execution semantics
    - **Property 11: Guardrail execution semantics**
    - **Validates: Requirements 13.5, 13.8, 13.9**
    - Generate sequences of guardrail results, assert sequential execution with short-circuit at first failure and first-turn-only for input guardrails

- [x] 5. StreamingToolExecutor implementation
  - [x] 5.1 Implement StreamingToolExecutor in agent-core
    - Create `StreamingToolExecutor` struct with queue, completed results, max_concurrency (default 8, min 1)
    - Implement `enqueue()` method accepting ToolUseBlock, Arc<dyn Tool>, and RunContext
    - Implement concurrency enforcement: Safe tools run in parallel up to max_concurrency, Exclusive tools wait for all executing tools then run alone
    - Implement `drain_completed()` returning results in enqueue order
    - Implement `next_remaining()` async method for awaiting pending tools
    - Implement error cascading via CancellationToken when error_cascades() is true
    - Ensure no tool starts while an Exclusive tool is executing
    - _Requirements: 10.1, 10.2, 10.3, 10.4, 10.5, 10.6, 10.7, 10.8_

  - [x] 5.2 Write property test for concurrency classification enforcement
    - **Property 4: Concurrency classification enforcement**
    - **Validates: Requirements 10.2, 10.3, 10.8**
    - Generate sequences of Safe/Exclusive tool enqueue operations, assert Safe tools can run in parallel and Exclusive tools run alone

  - [x] 5.3 Write property test for tool result ordering preservation
    - **Property 5: Tool result ordering preservation**
    - **Validates: Requirements 10.4**
    - Generate tool sets completing in arbitrary order, assert drain_completed() returns in enqueue order

- [x] 6. Context Compactor implementation
  - [x] 6.1 Implement ContextCompactor in agent-core
    - Create `ContextCompactor` struct accepting `CompactionConfig`
    - Implement `CompactionConfig` with stages (Vec<CompactionStage>) and optional summary_model
    - Implement `CompactionStage` enum (Snip, TruncateToolResults, AutoSummarize, Custom)
    - Implement Snip stage: remove oldest non-system messages when exceeding max_history_tokens, preserving system messages and most recent user message
    - Implement TruncateToolResults stage: truncate tool results exceeding max_chars, append "[truncated]"
    - Implement AutoSummarize stage: replace old messages with summary (skip if no summary_model)
    - Execute stages sequentially in defined order
    - Return `Option<CompactionEvent>` with stage applied, messages affected, token counts before/after
    - Return None if no threshold is met
    - _Requirements: 11.1, 11.2, 11.3, 11.4, 11.5, 11.6, 11.7, 11.8, 11.9, 11.10_

  - [x] 6.2 Write property test for Snip compaction preserves critical messages
    - **Property 6: Snip compaction preserves critical messages**
    - **Validates: Requirements 11.4**
    - Generate message histories exceeding max_history_tokens, assert system messages and most recent user message are preserved

  - [x] 6.3 Write property test for tool result truncation
    - **Property 7: Tool result truncation**
    - **Validates: Requirements 11.5**
    - Generate tool result strings exceeding max_chars, assert truncated result ≤ max_chars and ends with "[truncated]"

  - [x] 6.4 Write property test for AutoSummarize preservation
    - **Property 8: AutoSummarize preserves system and recent messages**
    - **Validates: Requirements 11.6**
    - Generate histories exceeding threshold_tokens with summary model configured, assert system and recent messages preserved

  - [x] 6.5 Write property test for compaction no-op below thresholds
    - **Property 9: Compaction no-op below thresholds**
    - **Validates: Requirements 11.9**
    - Generate message histories below all thresholds, assert compactor returns None and messages unchanged

- [x] 7. Checkpoint - Verify executor and compactor
  - Ensure all tests pass, ask the user if questions arise.

- [x] 8. Main loop (RunLoop) implementation
  - [x] 8.1 Implement run() and run_stream() entry points in agent-core
    - Implement `run()` async function accepting Agent, Input, RunConfig → Result<RunResult, RunError>
    - Implement `run_stream()` async function returning RunStream
    - Implement the main loop phases: context compaction → prepare request → stream model + execute tools → drain remaining tools → resolve next step → apply state transition
    - Wire StreamingToolExecutor for concurrent tool execution during streaming
    - Wire ContextCompactor for compaction phase
    - _Requirements: 9.1, 9.2, 9.3_

  - [x] 8.2 Implement NextStep resolution and state transitions
    - Implement `resolve_next_step()` function inspecting model response, tool results, and agent config
    - Handle Continue: append assistant + tool result messages, loop back
    - Handle FinalOutput: run output guardrails, yield AgentEnd or GuardrailTripped
    - Handle Interruption: store pending approvals, yield Interruption event
    - Handle MaxTurns: yield MaxTurns event, return
    - Handle Aborted: yield Aborted event, return
    - Handle Recovery: dispatch to recovery strategies
    - Yield appropriate RunEvents at each transition
    - _Requirements: 9.4, 9.5, 9.6, 9.7, 9.8, 9.9, 9.10_

  - [x] 8.3 Implement recovery system in the RunLoop
    - Map ModelError::PromptTooLong → CompactAndRetry
    - Map ModelError::MaxOutputTokens → ContinueMessage
    - Implement CompactAndRetry: reduce messages, retry
    - Implement ContinueMessage: append continuation prompt, retry with incremented attempt
    - Implement EscalateOutputTokens: increase max_output_tokens, retry
    - Implement GiveUp: yield Error event, terminate
    - Track recovery attempts per error variant; escalate to GiveUp after 3 attempts
    - _Requirements: 18.1, 18.2, 18.3, 18.4, 18.5, 18.6, 18.7, 18.8, 18.9_

  - [x] 8.4 Write property test for recovery escalation
    - **Property 17: Recovery escalation**
    - **Validates: Requirements 18.6**
    - Simulate repeated recovery for same ModelError variant, assert escalation to GiveUp after 3 attempts

  - [x] 8.5 Implement guardrail invocation in the RunLoop
    - Invoke InputGuardrails on first turn only; yield GuardrailTripped and terminate if failed
    - Invoke OutputGuardrails when NextStep is FinalOutput; yield GuardrailTripped if failed
    - Execute guardrails sequentially in registration order, short-circuit at first failure
    - _Requirements: 13.5, 13.6, 13.7, 13.8, 13.9_

  - [x] 8.6 Implement usage tracking and budget enforcement
    - Accumulate turn Usage into RunState.total_usage (component-wise sum)
    - Compute turn cost from model pricing rates, add to total_cost_usd
    - After each turn, check if total_cost_usd exceeds configured budget
    - If budget exceeded, resolve NextStep::Aborted with reason "budget_exceeded"
    - Skip budget check if no budget configured
    - Treat missing usage data as zero
    - _Requirements: 26.1, 26.2, 26.3, 26.4, 26.5, 26.6_

  - [x] 8.7 Write property test for usage and cost accumulation
    - **Property 22: Usage and cost accumulation**
    - **Validates: Requirements 26.2, 26.3**
    - Generate sequences of Usage values with pricing rates, assert total_usage equals component-wise sum and total_cost_usd equals expected formula

  - [x] 8.8 Write property test for budget enforcement
    - **Property 23: Budget enforcement**
    - **Validates: Requirements 26.4**
    - Generate budget values and cost sequences, assert Aborted with "budget_exceeded" when cost exceeds budget

  - [x] 8.9 Write property test for event stream well-formedness
    - **Property 19: Event stream well-formedness**
    - **Validates: Requirements 21.8, 21.9**
    - Run various scenarios through the loop with mock provider, assert exactly one terminal event and ToolStart before ToolEnd for each tool id

- [x] 9. Checkpoint - Verify main loop
  - Ensure all tests pass, ask the user if questions arise.

- [x] 10. Sub-agent system
  - [x] 10.1 Implement SubAgentTool in agent-core
    - Implement `SubAgentDef` struct with all fields (agent, tool_name, tool_description, input_schema, max_turns, background, allowed_tools)
    - Implement `SubAgentTool` struct implementing the Tool trait
    - On invocation: spawn isolated RunLoop with empty message history, use tool call args as initial user message
    - For background=false: await sub-agent completion, return final output
    - For background=true: spawn as detached tokio task, return task identifier
    - Accumulate sub-agent token usage and cost into parent RunState
    - Handle sub-agent max_turns: terminate and return last output with indication
    - Handle sub-agent errors: return ToolOutput with error description
    - _Requirements: 14.1, 14.2, 14.3, 14.4, 14.5, 14.6, 14.7, 14.8, 14.9_

  - [x] 10.2 Write property test for sub-agent isolation
    - **Property 12: Sub-agent isolation**
    - **Validates: Requirements 14.3, 14.6**
    - Spawn sub-agents with various parent states, assert sub-agent starts with empty history

  - [x] 10.3 Write property test for sub-agent cost accumulation
    - **Property 13: Sub-agent cost accumulation**
    - **Validates: Requirements 14.7**
    - Run sub-agents producing usage/cost, assert parent RunState totals are incremented correctly

- [x] 11. Skill system
  - [x] 11.1 Implement SkillRegistry and SkillTool in agent-core
    - Implement `Skill` struct with all fields (name, description, when_to_use, allowed_tools, arguments, context, hooks, body, base_dir, source)
    - Implement `SkillContext` enum (Inline, Fork)
    - Implement `SkillSource` enum for tracking provenance
    - Implement `SkillRegistry` with load(), find(), activate_for_path(), system_prompt_section()
    - Load from project-level (.agent/skills/) and user-level (~/.agent/skills/), project takes precedence
    - Parse YAML frontmatter from SKILL.md files; skip files with invalid frontmatter and log warning
    - Implement `SkillTool` substituting template variables ($ARGUMENTS, $1, $2, ${SKILL_DIR})
    - For Inline context: return rendered body as ToolOutput::Text
    - For Fork context: spawn sub-agent with skill's allowed_tools
    - Leave unresolved positional variables as empty strings
    - _Requirements: 20.1, 20.2, 20.3, 20.4, 20.5, 20.6, 20.7, 20.8, 20.9_

  - [x] 11.2 Write property test for skill template variable substitution
    - **Property 18: Skill template variable substitution**
    - **Validates: Requirements 20.7**
    - Generate skill bodies with various template variables and argument strings, assert all recognized variables are substituted and unresolved positional vars become empty strings

  - [x] 11.3 Write property test for skill registry precedence
    - **Property 24: Skill registry precedence**
    - **Validates: Requirements 20.4**
    - Register skills at both project and user level with same name, assert find() returns project-level skill

- [x] 12. Tracing and observability
  - [x] 12.1 Add tracing instrumentation to RunLoop and components
    - Add `tracing` crate integration with OpenTelemetry-compatible spans
    - Create root span "agent.run" with run_id and agent name on run start
    - Create child span "model.stream" on each model call
    - Create child span "tool.execute" on each tool execution with tool name
    - Create child span "sub_agent" on sub-agent spawn with agent name
    - Set span status to error with description on failures
    - _Requirements: 19.1, 19.2, 19.3, 19.4, 19.5, 19.6_

- [x] 13. Checkpoint - Verify sub-agents, skills, and tracing
  - Ensure all tests pass, ask the user if questions arise.

- [x] 14. Unified LLM Provider (agent-llm)
  - [x] 14.1 Implement UnifiedProvider and feature-flag structure in agent-llm
    - Implement `UnifiedProvider` struct implementing ModelProvider trait
    - Configure feature flags: "openai" (default), "anthropic" (default), "ollama" (optional), "all-providers"
    - Implement `from_env()` reading OPENAI_API_KEY, ANTHROPIC_API_KEY, OLLAMA_HOST environment variables
    - Route model names with recognized prefix (e.g., "anthropic:...") to specified provider
    - Route unprefixed names to configured default_provider
    - Return errors for: no configured providers, unavailable provider prefix, no default configured
    - _Requirements: 16.1, 16.2, 16.3, 16.4, 16.5, 16.8, 16.9, 16.10_

  - [x] 14.2 Implement provider-specific message format converters in agent-llm
    - Implement per-provider convert modules mapping canonical Message types to/from wire format
    - OpenAI format converter (behind "openai" feature flag)
    - Anthropic format converter (behind "anthropic" feature flag)
    - Ollama format converter (behind "ollama" feature flag)
    - _Requirements: 16.6, 16.7_

  - [x] 14.3 Write property test for model name routing
    - **Property 14: Model name routing**
    - **Validates: Requirements 16.4, 16.5**
    - Generate model names with and without provider prefixes, assert correct routing

  - [x] 14.4 Write property test for message format conversion round-trip
    - **Property 15: Message format conversion round-trip**
    - **Validates: Requirements 16.7**
    - Generate canonical Messages, convert to provider format and back, assert equality

  - [x] 14.5 Implement retry and fallback logic in agent-llm
    - Implement `RetryConfig` struct with all fields and defaults (max_retries: 3, initial_backoff_ms: 1000, max_backoff_ms: 30000, backoff_multiplier: 2.0, retryable_statuses: [429, 500, 502, 503, 529])
    - Implement exponential backoff: delay = min(initial_backoff_ms × backoff_multiplier^(attempt-1), max_backoff_ms) with 0–25% random jitter
    - Implement fallback chain: on exhausted retries, attempt next model in chain
    - Respect Retry-After header, capped at max_backoff_ms
    - Return comprehensive error when all retries and fallbacks exhausted
    - _Requirements: 25.1, 25.2, 25.3, 25.4, 25.5_

  - [x] 14.6 Write property test for exponential backoff formula
    - **Property 21: Exponential backoff formula**
    - **Validates: Requirements 25.2**
    - Generate attempt counts and retry configs, assert computed delay matches formula within jitter bounds

- [x] 15. Built-in tools (agent-tools)
  - [x] 15.1 Implement built-in tools in agent-tools crate
    - Implement `ShellTool` (Concurrency::Exclusive, timeout 300s, returns ToolOutput::Error on non-zero exit)
    - Implement `FileReadTool` (Concurrency::Safe)
    - Implement `FileWriteTool` (Concurrency::Exclusive)
    - Implement `GlobTool` (Concurrency::Safe)
    - Implement `GrepTool` (Concurrency::Safe)
    - Each tool implements the Tool trait from agent-core with proper schema definitions
    - _Requirements: 23.1, 23.2, 23.3, 23.4, 23.5, 23.6, 23.7, 23.8, 23.9, 23.10_

- [x] 16. MCP client integration (agent-mcp)
  - [x] 16.1 Implement MCP server client in agent-mcp
    - Define async `MCPServer` trait with name(), connect(), list_tools(), call_tool(), close()
    - Define `MCPTransport` enum (Stdio, Http, Sse)
    - Implement function to convert MCP tool definitions into Arc<dyn Tool> objects
    - Implement JSON-RPC request/response handling for call_tool
    - Handle connection timeout (30 seconds), return MCPError with transport type and server name
    - Return MCPError if call_tool/list_tools invoked before successful connect
    - Return MCPError with server name and details on error response
    - _Requirements: 15.1, 15.2, 15.3, 15.4, 15.5, 15.6, 15.7_

- [x] 17. CLI binary (agent-cli)
  - [x] 17.1 Implement CLI binary in agent-cli
    - Compile to binary named "arlo"
    - Accept optional --model flag for model selection
    - Read API keys from environment variables
    - Implement interactive REPL mode (default when no prompt argument)
    - Implement single-prompt mode (prompt as argument, print response, exit)
    - Exit with non-zero code and descriptive error when: API key missing, unrecognized model
    - Wire together UnifiedProvider, PermissionEngine, Agent, and run() entry point
    - _Requirements: 24.1, 24.2, 24.3, 24.4, 24.5, 24.6, 24.7_

- [x] 18. Feature flags and conditional compilation
  - [x] 18.1 Configure feature flags for optional dependencies in agent-llm
    - Ensure "openai" and "anthropic" are default features
    - Ensure "ollama" is optional
    - Ensure "all-providers" enables all provider features
    - Verify: disabled feature excludes corresponding modules from compilation
    - Verify: no provider features enabled still compiles with core types only
    - _Requirements: 27.1, 27.2, 27.3, 27.4, 27.5_

- [x] 19. Final checkpoint - Full workspace verification
  - Ensure `cargo build --workspace` compiles without errors
  - Ensure `cargo test --workspace` passes all tests
  - Ensure `cargo build -p agent-core`, `cargo build -p agent-llm`, etc. each compile independently
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional and can be skipped for faster MVP
- Each task references specific requirements for traceability
- Checkpoints ensure incremental validation
- Property tests validate universal correctness properties using the `proptest` crate
- Unit tests validate specific examples and edge cases
- The implementation uses Rust with tokio async runtime, serde for serialization, thiserror for errors, and async-trait for async trait definitions
- All code should compile with `cargo build --workspace` after each non-optional task completes

## Task Dependency Graph

```json
{
  "waves": [
    { "id": 0, "tasks": ["1.1"] },
    { "id": 1, "tasks": ["1.2", "1.4", "1.5", "1.6"] },
    { "id": 2, "tasks": ["1.3", "1.7", "2.1", "2.2", "2.6"] },
    { "id": 3, "tasks": ["2.3", "2.7"] },
    { "id": 4, "tasks": ["2.4", "2.5", "2.8", "4.1", "4.2"] },
    { "id": 5, "tasks": ["4.3", "5.1", "6.1"] },
    { "id": 6, "tasks": ["4.4", "4.5", "5.2", "5.3", "6.2", "6.3", "6.4", "6.5"] },
    { "id": 7, "tasks": ["8.1"] },
    { "id": 8, "tasks": ["8.2", "8.3", "8.5", "8.6"] },
    { "id": 9, "tasks": ["8.4", "8.7", "8.8", "8.9"] },
    { "id": 10, "tasks": ["10.1", "11.1", "12.1"] },
    { "id": 11, "tasks": ["10.2", "10.3", "11.2", "11.3"] },
    { "id": 12, "tasks": ["14.1", "15.1", "16.1"] },
    { "id": 13, "tasks": ["14.2", "14.5"] },
    { "id": 14, "tasks": ["14.3", "14.4", "14.6"] },
    { "id": 15, "tasks": ["17.1", "18.1"] }
  ]
}
```
