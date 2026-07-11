# AGENTS.md — arlo-rust

Agent framework in Rust: an LLM run loop with tools, HITL permissions, sub-agents,
background tasks, context compaction, and a TUI. Cargo workspace, 5 crates.

## Commands

```bash
make check          # cargo check --workspace  (fast; run this first after edits)
make test           # cargo test --workspace
make lint           # cargo clippy --workspace --all-targets -- -D warnings  (warnings = errors)
make fmt            # cargo fmt --all
make run            # cargo run --bin agent-cli
cargo test -p agent-core run_loop        # scope tests to one crate/module while iterating
```

CLI usage: `arlo [--model M] [--profile NAME] [--dump-prompt] ["prompt"]` — no prompt = REPL.
Runtime config lives in `.arlo/settings.json` (permissions `allow`/`deny` patterns, provider
`profiles`); skills in `.arlo/skills/` (project) and `~/.arlo/skills/` (user).

## Map: where things live

All paths relative to `crates/`.

| Area | Files |
|---|---|
| **Main loop** (all phases, continuation logic, approval processing) | `agent-core/src/run_loop.rs` — `drive()` is the single implementation behind `run()` and `run_stream()` |
| Loop state machine | `agent-core/src/next_step.rs` (`NextStep`, `PendingApproval`, `RecoveryStrategy`) |
| Permissions (4-layer engine) | `agent-core/src/permission.rs`; pattern syntax in `pattern.rs` (`bash`, `fs_*`, `Bash(npm*)`); settings merge in `settings.rs` |
| Approval / HITL types | `agent-core/src/config.rs` (`ApprovalHandler`, `ApprovalResponse`, `RunConfig`) |
| Tool trait & classifications | `agent-core/src/tool.rs` (`Tool`, `ApprovalRequirement`, `Concurrency`) |
| Concurrent tool execution | `agent-core/src/executor.rs` (`StreamingToolExecutor`, Safe vs Exclusive) |
| Task/todo storage | `agent-core/src/task_store.rs` (trait + types), `in_memory_task_store.rs` (impl), `todolist_tool.rs` (LLM-facing tool) |
| Sub-agents | `agent-core/src/sub_agent.rs` (`SubAgentTool`, fg/bg modes), `agent.rs` (`SubAgentDef`, `Agent` builder) |
| Errors & recovery | `agent-core/src/error.rs` (`RunError`/`ModelError`/`ToolError`), `recovery.rs` (`RecoveryTracker`) |
| Context compaction | `agent-core/src/compaction/` — 3 layers: tools_compact → session_memory → full_summarize |
| LLM providers | `agent-llm/src/provider.rs` (`UnifiedProvider`), `openai_http.rs`, `retry.rs`, `model_override.rs`; profiles in `agent-core/src/profile.rs`, resolution in `config_resolver.rs` |
| Built-in tools | `agent-tools/src/` — file_read/write/edit, glob, grep, shell, web_fetch, web_search |
| MCP client | `agent-mcp/src/` |
| TUI / CLI | `agent-cli/src/main.rs` (wiring, agent builder), `agent-cli/src/tui/` (approval UI in `approval.rs`, event loop in `event_loop.rs`) |

Deep dives (read before touching the corresponding area):
`doc/agent-framework.md` (full architecture, in Chinese), `doc/sub-agent-task-coordination.md`
(sequence diagrams + known limits).

## Core model (read this before changing the loop)

Each turn: compaction → build request → stream model → execute tools → `resolve_next_step()`
→ apply transition. `NextStep` variants: `Continue`, `FinalOutput`, `Interruption`,
`Recovery`, `MaxTurns`, `Aborted`.

`FinalOutput` does NOT immediately end the run. Three gates run in order:
1. Output guardrails — failure kills the run (`RunError::Guardrail`).
2. `await_background_tasks` — if any TaskEntry is Pending/Running, block (200ms poll,
   10-min ceiling), inject the result as a user message, continue the loop.
3. `todo_continuation_prompt` — incomplete todos inject a "continue working" user message,
   max 3 consecutive times (`todo_continuation_count`, reset on any normal `Continue`).

Two entities in `TaskStore`, don't conflate them:
- `TodoItem` = the model's visible plan (Pending/InProgress/Completed), managed via the
  `todolist` tool.
- `TaskEntry` = a background execution unit (Pending/Running/Completed/Failed/Killed),
  auto-registered by `SubAgentTool` in background mode. Carries output, usage,
  dependencies, retries, `acknowledged`.

## Invariants — do not break

- **Exactly-once result delivery**: background task results reach the model only via
  `drain_task_notifications`, which sets `acknowledged` immediately after injecting.
  Never deliver a task result to the model through any other path.
- **Register before spawn**: `SubAgentTool::run_background` creates the TaskEntry *before*
  `tokio::spawn` so the parent model gets a correlatable `task_id` in the tool result.
- **Panic isolation**: the background sub-agent's `run()` is wrapped in an inner
  `tokio::spawn` so a panic still transitions the task to Failed. Removing this deadlocks
  `await_background_tasks`.
- **Exactly one terminal RunEvent** per streamed run (`AgentEnd` | `MaxTurns` | `Aborted` |
  `Error` | `Interruption` | `GuardrailTripped`). The TUI relies on this.
- **Permission semantics differ by layer**: static deny (Layer 2) aborts the whole run;
  an interactive user `Deny` only injects an `is_error` ToolResult and the run continues.
  Session grants (Layer 3) can never override static deny.
- **Tool errors are non-fatal**: they return to the model as `is_error: true` tool results.
  Don't convert them into `RunError`.
- **Recovery retries don't consume turns**; attempt counters are per error variant, capped
  at `MAX_RECOVERY_ATTEMPTS = 3`, and reset on any successful `Continue`.
- **Sub-agent isolation**: sub-agents start with an empty message history. They share the
  parent's `ApprovalHandler` (via Arc) and a shared session-grant store — approval UX must
  keep working across the agent tree.

## Recipes

**Add a built-in tool**: implement `Tool` in `agent-tools/src/<name>.rs` (declare
`parameters_schema`, `concurrency` — `Exclusive` if it mutates shared state — and
`approval_requirement` — `Always` for anything destructive), export from `lib.rs`,
register in the agent builder in `agent-cli/src/main.rs`.

**Add/adjust a provider**: `agent-llm/src/provider.rs` (`UnifiedProvider::from_profile`)
+ profile fields in `agent-core/src/profile.rs`. Model metadata (context window, pricing)
matters: cost tracking and compaction thresholds read it.

**Change when the loop stops / continues**: `resolve_next_step()` and the `NextStep`
match in `drive()` (`run_loop.rs`). Check the FinalOutput gates above before adding a new
exit path.

**Persist tasks/todos**: implement the `TaskStore` trait (`task_store.rs`); the in-memory
impl is the reference for legal status transitions and retry-reset behavior.

**New recovery behavior**: map the `ModelError` variant in `recovery.rs::map_error_to_strategy`
and handle the strategy in `run_loop.rs::apply_recovery_run`.

## Testing conventions

- Unit tests are colocated in each source file (`#[cfg(test)] mod tests`); integration
  tests in `crates/agent-core/tests/` (e.g. `background_subagent_repro.rs` guards the
  result-delivery pipeline).
- Property-based tests via `proptest` are the norm for state machines, stores, and the
  permission engine — extend the existing strategies rather than writing ad-hoc loops.
- Mock `Model`/`ModelProvider`/`Tool` implementations already exist in `run_loop.rs` and
  `sub_agent.rs` test modules; reuse them.
- Clippy runs with `-D warnings`; run `make lint` before considering a change done.

## Gotchas

- `run_loop.rs` is ~3.5k lines (half tests); read it in targeted slices, not whole.
- A comment in `run_loop.rs` says "4-layer compaction" — the actual pipeline is 3 layers
  (`compaction/mod.rs` is authoritative).
- `PermissionEngine.check()` is sync and uses `try_read()` on the shared grant store; under
  lock contention it silently falls through to Layer 4 (may re-prompt). Don't hold write
  locks across await points near it.
- Background-task waiting is poll-based (`ponytail:` comment marks the upgrade path to a
  notify channel). Don't add a second polling loop; extend the store instead.
- No task store configured ⇒ background sub-agents are fire-and-forget (results lost).
  Tests that assert on results must inject an `InMemoryTaskStore` via
  `RunConfig::builder(...).task_store(...)`.
