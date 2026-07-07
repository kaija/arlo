# Arlo-Rust Feature Roadmap

Autonomous agent framework capabilities with implementation status.
Features are generic/open-source agent capabilities — no proprietary features included.

---

## Legend

| Symbol | Meaning |
|--------|---------|
| ✅ | Implemented (all tasks complete) |
| 🚧 | In Progress (partially complete) |
| 📋 | Planned (spec written, not started) |

---

## 1. Core Agent Loop & Runtime

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | Streaming-First RunLoop | ✅ | Async main loop yielding `RunEvent` stream, phases: compaction → request → stream + tool exec → drain → resolve → transition |
| 2 | NextStep State Machine | ✅ | Explicit control flow: Continue, FinalOutput, Interruption, Recovery, BudgetContinue, MaxTurns, Aborted |
| 3 | Serializable RunState | ✅ | Full state snapshot (Serialize/Deserialize/PartialEq), pause/resume at any point |
| 4 | Concurrent Tool Execution During Streaming | ✅ | StreamingToolExecutor — tools start during model generation, Safe tools in parallel, Exclusive tools serialized |
| 5 | Multi-Stage Context Compaction | ✅ | Pipeline: Snip (token limit), TruncateToolResults (char limit), AutoSummarize (summarization model), Custom stages |
| 6 | Error Recovery System | ✅ | CompactAndRetry, ContinueMessage, EscalateOutputTokens, FallbackModel, GiveUp — auto-escalation after 3 retries |
| 7 | Budget Enforcement | ✅ | Track token usage/cost per turn, abort when budget exceeded |
| 8 | Run Events & Streaming API | ✅ | Rich event stream (TurnStart, StreamChunk, ToolStart/End, SubAgentStart/End, Compaction, AgentEnd, Error, etc.) |

---

## 2. Model Provider Abstraction

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | Model-Agnostic Provider Trait | ✅ | `ModelProvider` + `Model` traits — Send + Sync, async stream interface |
| 2 | Unified Provider with Feature Flags | ✅ | Single `UnifiedProvider` routing by prefix (e.g., `anthropic:claude-sonnet-4-20250514`), feature flags: openai (default), anthropic (default), ollama (optional) |
| 3 | Provider Format Converters | ✅ | Per-provider message format mapping with round-trip correctness |
| 4 | Retry with Exponential Backoff | ✅ | Configurable RetryConfig, jitter, Retry-After header support, retryable status codes |
| 5 | Fallback Chain | ✅ | Multi-provider fallback when retries exhausted |
| 6 | Token Counting & Cost Calculation | ✅ | Per-turn usage tracking, model pricing rates, cumulative cost |

---

## 3. Tool System

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | Tool Trait with Concurrency Classification | ✅ | `Tool` trait: name, description, schema, concurrency (Safe/Exclusive), approval, execute |
| 2 | Dynamic Concurrency Classification | ✅ | `concurrency()` accepts input — classification can vary per invocation |
| 3 | Tool Timeout Support | ✅ | Per-tool configurable timeout |
| 4 | Error Cascading | ✅ | Tools with `error_cascades()` cancel sibling tools on failure |
| 5 | Approval Requirements | ✅ | ApprovalRequirement: Never, Always, Conditional — integrated with PermissionEngine |
| 6 | Built-in Shell Tool | ✅ | Exclusive concurrency, 300s timeout, error on non-zero exit |
| 7 | Built-in File Read Tool | ✅ | Safe concurrency, reads file content |
| 8 | Built-in File Write Tool | ✅ | Exclusive concurrency, writes content to files |
| 9 | Built-in Glob Tool | ✅ | Safe concurrency, glob pattern file search |
| 10 | Built-in Grep Tool | ✅ | Safe concurrency, regex content search |

---

## 4. Web Tools

**Spec:** `autonomous-web-tools` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | WebFetch Tool | ✅ | Fetch URL, convert HTML→markdown, return truncated content |
| 2 | URL Validation & Security | ✅ | Length limit, no credentials, hostname validation, auto HTTP→HTTPS upgrade |
| 3 | Redirect Handling | ✅ | Same-host auto-follow, cross-host report, max 10 hops |
| 4 | Timeout & Size Limits | ✅ | 60s timeout, 10MB body limit |
| 5 | Native HTML-to-Markdown Engine | ✅ | Rule-based turndown.js-inspired converter: CommonMark rules, whitespace collapsing, smart join, extensible ConversionRule trait |
| 6 | Content Truncation | ✅ | 100K char limit with truncation notice |
| 7 | URL Response Cache | ✅ | In-memory TTL cache (15 min) |
| 8 | WebSearch Tool | ✅ | Trait-based SearchProvider interface, returns structured results |
| 9 | Brave Search Provider | ✅ | Default SearchProvider implementation using Brave Search API |
| 10 | Extensible Conversion Rules | ✅ | add_rule(), remove(), keep(), use_plugin() — custom rules take priority |

---

## 5. Permission & Safety System

**Spec:** `rust-agent-framework` + `hitl-policy-permissions` — Status: ✅ Core / 📋 Enhanced

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | 4-Layer Permission Engine | ✅ | Static rules → Tool-specific check → Permission mode → Interactive approval |
| 2 | Static Allow/Deny Rules | ✅ | Layer 1 short-circuit on exact match |
| 3 | Permission Modes (Bypass/Normal/DenyAll) | ✅ | Layer 3 mode-based decision |
| 4 | Interactive Approval with Timeout | ✅ | 300s timeout, approve/reject/always-allow responses |
| 5 | Session-Scoped "Always Allow" | ✅ | `grant_session_allow()` persists for run duration |
| 6 | Input Guardrails | ✅ | Check messages before first model call, short-circuit on failure |
| 7 | Output Guardrails | ✅ | Check final output before delivery |
| 8 | Tool Guardrails | ✅ | check_input() + check_output() before/after each tool call |
| 9 | Settings File Loading (.arlo/settings.json) | 📋 | Project-level + user-level JSON policy files |
| 10 | Glob & Argument-Prefix Pattern Matching | 📋 | `fs_*`, `Bash(npm*)`, `fs_write(/tmp/*)` pattern rules |
| 11 | Policy Merge Semantics | 📋 | User → Project → Runtime precedence, deny wins at same level |
| 12 | Pattern-Based Session Grants | 📋 | "Always allow" with pattern scope (not just exact tool name) |
| 13 | Sub-Agent Permission Propagation | 📋 | Clone-at-spawn semantics: child inherits parent's merged policy + session grants |
| 14 | Enhanced TUI Permission Prompt | 📋 | Show context, pattern suggestion, `p` key for pattern-based approval |

---

## 6. Sub-Agent & Delegation

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | SubAgentTool (Isolated Sub-Agents) | ✅ | Spawn isolated RunLoop via tool call, fresh message history, no parent context leakage |
| 2 | Foreground Sub-Agents | ✅ | Await completion, return final output |
| 3 | Background Sub-Agents | ✅ | Spawn as detached tokio task, return task ID immediately |
| 4 | Cost Accumulation to Parent | ✅ | Sub-agent usage/cost rolls up to parent RunState |
| 5 | Sub-Agent Turn Limit | ✅ | Configurable max_turns per sub-agent |
| 6 | Sub-Agent Error Handling | ✅ | Returns error description as ToolOutput on failure |

---

## 7. Skill System

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | Skill Registry | ✅ | Load SKILL.md files from project (.agent/skills/) and user (~/.agent/skills/) directories |
| 2 | YAML Frontmatter Parsing | ✅ | Metadata: name, description, when_to_use, allowed_tools, arguments, context, hooks |
| 3 | Template Variable Substitution | ✅ | $ARGUMENTS, $1, $2, ${SKILL_DIR} — unresolved positional → empty string |
| 4 | Inline Skills | ✅ | SkillContext::Inline returns rendered body as ToolOutput |
| 5 | Fork Skills | ✅ | SkillContext::Fork spawns sub-agent with skill's allowed_tools |
| 6 | Project-Level Precedence | ✅ | Project skills override user-level skills on name conflict |

---

## 8. Interactive CLI & HITL

**Spec:** `interactive-cli-repl-hitl` — Status: 🚧 In Progress

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | Ratatui TUI REPL | 🚧 | Full-screen terminal UI with ratatui/crossterm backend |
| 2 | Real-Time Streaming Output | 🚧 | TextDelta rendered per-frame, ThinkingDelta in distinct style |
| 3 | Tool Execution Display | 🚧 | Show tool name/args on start, result on end, progress indicator |
| 4 | HITL Permission Prompting | 🚧 | Inline y/a/n prompt on Interruption event |
| 5 | Session-Scoped Permission Memory | 🚧 | "Always allow" persists across prompts within session |
| 6 | Conversation History (Multi-Turn) | 🚧 | In-memory history passed to each run_stream() call |
| 7 | Double Ctrl-C Exit Pattern | 🚧 | First Ctrl-C aborts run, second within 2s exits REPL |
| 8 | Terminal Event Handling | 🚧 | AgentEnd/Error/Aborted/MaxTurns/GuardrailTripped → idle with feedback |
| 9 | Interface-Agnostic Session Layer | 🚧 | SessionCommand/SessionEvent architecture — reusable across TUI, HTTP, tests |
| 10 | Single-Prompt Mode | ✅ | CLI argument → execute → print → exit (pre-existing) |

---

## 9. Agent Configuration & Composability

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | Builder-Pattern Agent Configuration | ✅ | AgentBuilder with chainable setters, additive collection methods |
| 2 | Static & Dynamic Instructions | ✅ | Instructions::Static(String) or Instructions::Dynamic(async closure) |
| 3 | Output Schema Validation | ✅ | Optional JSON schema for structured output |
| 4 | Agent Lifecycle Hooks | ✅ | AgentHooks struct with optional callbacks |
| 5 | Configurable Max Turns | ✅ | Per-agent turn limit |
| 6 | RunConfig Builder | ✅ | Provider, permissions, compaction, temperature (validated 0.0–2.0), concurrency limit |

---

## 10. MCP Integration

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | MCP Server Client Trait | ✅ | Async MCPServer trait: connect, list_tools, call_tool, close |
| 2 | Multiple Transports | ✅ | Stdio, HTTP, SSE transport variants |
| 3 | MCP-to-Tool Conversion | ✅ | Convert MCP tool definitions into `Arc<dyn Tool>` objects |
| 4 | JSON-RPC Protocol | ✅ | Standard JSON-RPC request/response for tool calls |
| 5 | Connection Timeout | ✅ | 30s connection timeout with typed MCPError |
| 6 | Pre-Connection Error Handling | ✅ | Error if call_tool/list_tools invoked before connect succeeds |

---

## 11. Observability & Tracing

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | OpenTelemetry-Compatible Tracing | ✅ | `tracing` crate with OTel-conformant spans |
| 2 | Root Span (agent.run) | ✅ | Per-run span with run_id and agent name |
| 3 | Model Call Span (model.stream) | ✅ | Per-model-call child span |
| 4 | Tool Execution Span (tool.execute) | ✅ | Per-tool child span with tool name |
| 5 | Sub-Agent Span (sub_agent) | ✅ | Per-sub-agent child span |
| 6 | Error Status on Spans | ✅ | Span status set to error with description on failure |

---

## 12. Architecture & Infrastructure

**Spec:** `rust-agent-framework` — Status: ✅ Implemented

| # | Feature | Status | Description |
|---|---------|--------|-------------|
| 1 | Cargo Workspace (5 Crates) | ✅ | agent-core, agent-llm, agent-tools, agent-mcp, agent-cli |
| 2 | Clean Dependency DAG | ✅ | Core has no sibling deps; LLM/Tools/MCP depend on Core; CLI depends on Core + LLM |
| 3 | Feature-Flag Conditional Compilation | ✅ | Provider backends gated behind feature flags |
| 4 | Structured Error Hierarchy (thiserror) | ✅ | RunError, ModelError, ToolError with From conversions |
| 5 | Property-Based Testing (proptest) | ✅ | 24+ correctness properties validated across randomized inputs |
| 6 | Canonical Message Types | ✅ | Message, ContentBlock, ToolUseBlock, Usage — Serialize/Deserialize round-trip |
| 7 | Canonical StreamChunk Type | ✅ | Normalized streaming format: TextDelta, ThinkingDelta, ToolUseStart/InputDelta/End, MessageStop |

---

## Summary

| Category | Total Features | ✅ Done | 🚧 In Progress | 📋 Planned |
|----------|---------------|---------|-----------------|------------|
| Core Agent Loop & Runtime | 8 | 8 | 0 | 0 |
| Model Provider Abstraction | 6 | 6 | 0 | 0 |
| Tool System | 10 | 10 | 0 | 0 |
| Web Tools | 10 | 10 | 0 | 0 |
| Permission & Safety System | 14 | 8 | 0 | 6 |
| Sub-Agent & Delegation | 6 | 6 | 0 | 0 |
| Skill System | 6 | 6 | 0 | 0 |
| Interactive CLI & HITL | 10 | 1 | 9 | 0 |
| Agent Configuration & Composability | 6 | 6 | 0 | 0 |
| MCP Integration | 6 | 6 | 0 | 0 |
| Observability & Tracing | 6 | 6 | 0 | 0 |
| Architecture & Infrastructure | 7 | 7 | 0 | 0 |
| **Total** | **95** | **80** | **9** | **6** |

---

## Feature Gap Analysis vs. Generic Autonomous Agent

The following table identifies capabilities common to open-source autonomous agents (like OpenAI Agents SDK, LangGraph, AutoGen) that are NOT yet in arlo-rust specs:

| # | Capability | Priority | Notes |
|---|-----------|----------|-------|
| 1 | Git Integration Tool | High | Read diffs, stage, commit, create branches — essential for coding agents |
| 2 | File Edit/Patch Tool | High | Apply targeted edits (not full-file writes) — sed-like or diff-based |
| 3 | Memory / Persistent Context | Medium | Cross-session memory (vector store, knowledge base, or conversation summaries) |
| 4 | Multi-Agent Orchestration Patterns | Medium | Sequential, parallel, router patterns beyond simple sub-agent delegation |
| 5 | Structured Output / JSON Mode | Medium | Force model to output JSON conforming to a schema (partially covered by output_schema) |
| 6 | Tool Result Streaming | Low | Stream partial tool results back to model (e.g., long shell output) |
| 7 | Agent-to-Agent Communication | Low | Message passing between peer agents (not just parent→child) |
| 8 | Workspace/Project Context Loading | Medium | Auto-load project structure, README, configs as initial context |
| 9 | Code Execution Sandbox | Medium | Safe execution environment (Docker/WASM) for running generated code |
| 10 | File Watcher / Event-Driven Triggers | Low | React to filesystem changes, start runs on file save |
| 11 | HTTP/REST API Server Mode | Medium | Expose agent as HTTP service (AG-UI, REST endpoints) |
| 12 | Session Persistence & Resume | Medium | Save/restore full sessions (partially covered by RunState serialization) |
| 13 | Rate Limit Pooling | Low | Shared rate-limit tracking across concurrent agent instances |
| 14 | Prompt Caching Optimization | Medium | Leverage provider cache APIs (Anthropic prompt caching, OpenAI cached prompts) |
