# Arlo-Rust Agent Framework Architecture

> 繁體中文版：[agent-framework.zh-TW.md](agent-framework.zh-TW.md)

This document describes the core execution architecture of arlo-rust: the main loop
(RunLoop) and its autonomous decision-making, HITL permission control, Task/Todo
management, sub-agent operation and coordination, and the error-handling flow. It closes
with a complete example that exercises every mechanism.

Code locations are relative to `crates/agent-core/src/`; e.g. `run_loop.rs` means
`crates/agent-core/src/run_loop.rs`.

---

## Table of contents

1. [Architecture overview](#1-architecture-overview)
2. [The main loop (RunLoop) and autonomous decisions](#2-the-main-loop-runloop-and-autonomous-decisions)
3. [HITL permission control](#3-hitl-permission-control-human-in-the-loop)
4. [Task management and the Todo tool](#4-task-management-and-the-todo-tool)
5. [How sub-agents work](#5-how-sub-agents-work)
6. [Coordinating multiple sub-agents from the main agent](#6-coordinating-multiple-sub-agents-from-the-main-agent)
7. [Error handling and recovery](#7-error-handling-and-recovery)
8. [End-to-end example: the full lifecycle of a long task](#8-end-to-end-example-the-full-lifecycle-of-a-long-task)

---

## 1. Architecture overview

The workspace is split into five crates:

| Crate | Responsibility |
|---|---|
| `agent-core` | RunLoop, NextStep state machine, permission engine, TaskStore, sub-agents, compaction, recovery |
| `agent-llm` | Model / ModelProvider implementations (Anthropic, OpenAI, etc.) |
| `agent-tools` | Built-in tools (file_read / file_edit / bash …) |
| `agent-mcp` | MCP client and transport |
| `agent-cli` | TUI, approval UI, event rendering |

Core data flow:

```
User prompt
    │
    ▼
run() / run_stream()  ──────────► RunEvent stream (subscribed by the TUI)
    │
    ▼
┌─────────────────── drive() (main loop) ────────────────────┐
│ Phase 0   turn limit check                                  │
│ Phase 0.5 inject background task results (TaskStore)        │
│ Phase 1   context compaction (3-layer CompactionPipeline)   │
│ Phase 1.5 input guardrails (first turn only)                │
│ Phase 2   build ModelRequest (system + messages + tools)    │
│ Phase 3   stream model response (errors → RecoveryTracker)  │
│ Phase 4   StreamingToolExecutor runs tools concurrently     │
│ Phase 5   resolve_next_step() → NextStep                    │
│ Phase 6   apply transition (continue / end / interrupt /    │
│           recover)                                          │
└──────────────────────────────────────────────────────────────┘
    │                         │
    ▼                         ▼
PermissionEngine          TaskStore (TaskEntry + TodoItem)
(HITL decisions)          (background tasks + plan list)
                              ▲
                              │
                        SubAgentTool (spawns fg/bg sub-agents)
```

`run()` (non-streaming) and `run_stream()` (streaming) share the single `drive()`
implementation (`run_loop.rs`). In streaming mode every phase emits `RunEvent`s over an
mpsc channel (`TurnStart`, `StreamChunk`, `ToolStart`/`ToolEnd`, `StepResolved`, and
exactly one terminal event).

---

## 2. The main loop (RunLoop) and autonomous decisions

### 2.1 The NextStep state machine

At the end of every turn, `resolve_next_step()` (`run_loop.rs`) resolves a `NextStep`
(`next_step.rs`) from the model's stop reason, its tool calls, and permission decisions:

```rust
pub enum NextStep {
    Continue,                                  // tool calls, all allowed → next turn
    FinalOutput { text, structured },          // model ended its turn → candidate exit
    Interruption { pending: Vec<PendingApproval> }, // tools need user approval
    Recovery { strategy: RecoveryStrategy },   // recoverable error
    MaxTurns { count },                        // turn limit reached
    Aborted { reason },                        // abort (content filter, deny, budget)
}
```

Resolution order (`resolve_next_step`):

1. `StopReason::ContentFilter` → `Aborted`
2. `current_turn + 1 >= max_turns` → `MaxTurns`
3. `StopReason::ToolUse` with tool calls → each goes through `PermissionEngine.check()`:
   - any `Deny` → `Aborted` (safety first — abort immediately)
   - any `NeedsApproval` → `Interruption` (collect all pending approvals)
   - all `Allow` → `Continue`
4. `StopReason::MaxTokens` → `Recovery` (ContinueMessage first, escalating to
   EscalateOutputTokens after two attempts)
5. `EndTurn` / `StopSequence` → `FinalOutput`

### 2.2 Deciding autonomously whether to keep going

Key design point: **the model saying "I'm done" (`FinalOutput`) does not mean the loop
actually ends.** Before replying to the user, `drive()` runs three "keep-going checks"
in order:

**Check 1 — Output guardrails**: validate the final output first; a failure terminates
immediately with `RunError::Guardrail`.

**Check 2 — Unfinished background tasks block exit** (`await_background_tasks`):
If the TaskStore still holds `Pending` / `Running` background tasks (usually background
sub-agents), the loop blocks (200 ms polling, 10-minute ceiling) until at least one task
reaches a terminal state, wraps the result as a `[background task completed/failed]` user
message injected into the conversation, then `continue`s back into the main loop so the
model can react. This guarantees the main agent never tells the user "done" while a
sub-agent is still running.

**Check 3 — Incomplete todos inject a continuation prompt** (`todo_continuation_prompt`):
If the TodoList still has non-`Completed` items, a user message listing them is injected
(`You have N incomplete todo item(s). Continue working through them: …`) so the model
keeps working. To avoid an infinite loop if the model gets stuck, **consecutive todo
continuations are capped at 3** (`todo_continuation_count`, reset by any normal
`Continue`).

Only after all three checks pass does the loop emit `AgentEnd` and return a `RunResult`.

### 2.3 When the loop yields to the user

The loop hands control back to the user in only a few situations:

| Situation | Behavior |
|---|---|
| `FinalOutput` with no unfinished tasks/todos | Return the final answer; run ends |
| `Interruption` **with** an `approval_handler` | Doesn't return — waits inline on the handler (TUI shows the approval UI), then continues the loop with the decision |
| `Interruption` **without** a handler | Records `pending_approvals` in `RunState` and returns a `RunResult`; the caller can later resume with `Input::Resume { state }` |
| `MaxTurns` / `Aborted` / recovery exhausted | Return with current state or an error |

So "waiting for the user" comes in two forms: **synchronous HITL** (the approval handler
blocks on the UI) and **asynchronous pause** (no handler — state is serialized and
returned, resumed later).

### 2.4 Per-turn resource guardrails

- **Turn limit**: `agent.max_turns` overrides `config.max_turns`; checked in Phase 0.
- **Budget**: usage is accumulated into cost each turn (`accumulate_usage`); exceeding
  `config.budget_usd` immediately yields `Aborted("budget_exceeded")`.
- **Context compaction**: Phase 1 runs the 3-layer `CompactionPipeline`
  (`compaction/mod.rs`) each turn, lightest first: `tools_compact` (drop stale tool
  results, zero cost) → `session_memory` (inject session memory, zero cost) →
  `full_summarize` (one LLM call producing a structured summary). The pipeline stops at
  the first layer that gets tokens under the threshold; 3 consecutive failures trip a
  circuit breaker that disables compaction.
- **Dropped stream consumer**: `TurnStart` is emitted at the start of every turn; if the
  send fails (the stream was dropped) the run ends with `Aborted("stream_dropped")` so an
  orphaned run doesn't keep burning money.

---

## 3. HITL permission control (Human-in-the-Loop)

### 3.1 Two levels: tool declaration + engine verdict

Every tool declares its own risk level via `Tool::approval_requirement()` (`tool.rs`):

```rust
pub enum ApprovalRequirement {
    Never,                 // never needs approval (default; e.g. read-only tools)
    Always,                // approval every time (e.g. bash, file_write)
    Conditional(String),   // conditional, with a reason (e.g. "when writing to /etc")
}
```

The actual verdict comes from the `PermissionEngine` (`permission.rs`) via **4-layer
short-circuit evaluation**:

```
Layer 1  Mode           Bypass → allow all; DenyAll → deny all; Normal → fall through
Layer 2  Static rules   static_deny match → Deny (deny wins over allow)
                        static_allow match → Allow
Layer 3  Session rules  local session_allows or shared shared_session_grants match → Allow
Layer 4  Tool declaration  Never → Allow; Always/Conditional → NeedsApproval
```

Rules support patterns (`ToolPattern` in `pattern.rs`): bare names (`bash`), globs
(`fs_*`), and compound forms (`Bash(npm*)` — matching both tool name and argument
content). Static rules can be loaded from settings files (`MergedPolicy` in
`settings.rs`).

### 3.2 The approval flow (Interruption)

When `resolve_next_step` collects tool calls that `NeedsApproval`, it returns
`NextStep::Interruption { pending }` and the main loop hands off to the
`ApprovalHandler` (`config.rs`):

```rust
pub trait ApprovalHandler: Send + Sync {
    async fn request_approval(&self, context: &ApprovalContext) -> Vec<ApprovalResponse>;
}

pub enum ApprovalResponse {
    Allow,                            // allow this one call
    Deny,                             // deny this one call
    AlwaysAllow { pattern: String },  // allow and register a session-level pattern grant
}
```

`ApprovalContext.agent_name` identifies which (sub-)agent is asking, so the TUI can show
the source. Response handling (the `Interruption` arm in `drive()`):

- **Allow**: keep the tool result and write it into the conversation normally.
- **AlwaysAllow**: call `permissions.grant_session_allow(pattern)` — subsequent calls
  matching the pattern pass at Layer 3 without prompting; the current result is kept.
- **Deny**: discard the tool result and inject an `is_error: true` ToolResult
  (`Permission denied: tool 'x' was not approved by the user.`) — **the run does not
  stop**; the model sees the denial next turn and adjusts its approach.

Note the contrast with a Layer 2 `Deny`: **a static deny `Abort`s the entire run** (a
hard policy rule); **an interactive user Deny only rejects that single call** and the
conversation continues.

Without a handler (CI, non-interactive mode) there are two options: set no handler → the
run pauses and returns (see 2.3); or attach a `DenyAllApprovalHandler` → everything is
auto-denied with a warn log and the loop never blocks.

### 3.3 Sharing session grants across the agent tree

The `PermissionEngine` can attach an `Arc<RwLock<Vec<ToolPattern>>>` shared grant store
(`with_shared_session_grants`). Sub-agents connect to the shared store when spawned
(`sub_agent.rs::sub_agent_config`), so **an AlwaysAllow the user grants during a
delegation is visible to the whole agent tree** — no sub-agent asks the same question
twice. The safety floor is unchanged: session grants live at Layer 3 and can never
override a Layer 2 static_deny.

---

## 4. Task management and the Todo tool

### 4.1 Two entities: TaskEntry vs TodoItem

`task_store.rs` defines two entities with **different purposes that coexist in the same
`TaskStore`**:

| | `TodoItem` (planning layer) | `TaskEntry` (execution layer) |
|---|---|---|
| Represents | An item in the model's **work plan** (a user-visible checklist) | A **background execution unit** (usually a background sub-agent) |
| Created by | The model, via the `todolist` tool | `SubAgentTool`, automatically when spawning a background task |
| States | `Pending → InProgress → Completed` | `Pending → Running → Completed / Failed / Killed` |
| Effect on the main loop | Incomplete → triggers the todo continuation prompt (max 3 consecutive) | Non-terminal → `FinalOutput` is intercepted; the loop waits for results |
| Extra fields | `active_form` (for display) | `output`, `usage`, `dependencies`, `max_retries`, `acknowledged` |

The relationship between them is **indirect and bridged by model semantics**: the
framework does not force a one-to-one todo↔task mapping. The typical pattern: the model
first breaks the plan down with todolist ("1. analyze module A, 2. analyze module B,
3. synthesize"), then spawns a background sub-agent (creating a TaskEntry) for each
parallelizable item, and on receiving each result notification checks the corresponding
todo off as completed. The main loop's two keep-going checks (background tasks + todos)
together guarantee this feedback loop doesn't break before the plan is fully done.

### 4.2 TodoListTool (the model-facing planning tool)

`todolist_tool.rs` implements the `Tool` trait with actions: `add` (content ≤ 1000
chars, active_form ≤ 200 chars), `update` (change status), `list`, `remove`,
`clear_completed`. `Concurrency::Safe`, `ApprovalRequirement::Never` (default) —
maintaining the plan needs no user approval.

### 4.3 The TaskStore trait and lifecycle

`TaskStore` (an async trait; in-memory implementation in `in_memory_task_store.rs`)
provides:

- **CRUD and the state machine**: `create_task` (enters `Pending`), `transition_task`
  (validates legal transitions; records `completed_at` on entering a terminal state; a
  `Failed` task with retry budget left is reset to `Pending` with `retry_count += 1`).
- **Queries**: `list_tasks`, `count_by_status`, `list_ready_tasks` (Pending with all
  dependencies satisfied), `list_blocked_tasks` (dependencies unsatisfied).
- **Notification protocol**: `list_unacknowledged_terminal` + `acknowledge_task` — the
  key to exactly-once result delivery back to the model (see section 6).
- **GC**: `evict_acknowledged`, `evict_older_than`.

`TaskEntry.dependencies` supports inter-task dependencies (B becomes ready only after A
completes); a failed dependency surfaces as `TaskStoreError::DependencyFailed`.

### 4.4 How an agent manages long tasks

The standard pattern for a long task:

1. **Decompose**: the model builds a visible plan with `todolist add`, updating each item
   `in_progress` → `completed` as it goes.
2. **Delegate**: time-consuming, independent sub-work goes to background sub-agents so
   the main conversation isn't blocked on long tool calls and the model can keep working
   through other todos.
3. **No early exit**: `FinalOutput` is double-gated by the background-task check and the
   todo check — if the model "forgets" it has work left, the framework pulls it back.
4. **Resumable**: when a run pauses on an `Interruption` (no handler), the `RunState`
   (messages, pending_approvals) is serializable and can be resumed later with
   `Input::Resume { state }`.

---

## 5. How sub-agents work

### 5.1 Definition and registration

A sub-agent hangs off its parent as a `SubAgentDef` (`agent.rs`) — in essence a complete
`Agent` (with its own instructions, tools, max_turns) wrapped as one of the parent's
tools (`SubAgentTool`, `sub_agent.rs`):

```rust
pub struct SubAgentDef {
    pub agent: Arc<Agent>,          // the full child agent definition
    pub tool_name: Option<String>,  // tool name exposed to the model
    pub max_turns: Option<u32>,     // overrides the parent config's turn limit
    pub background: bool,           // foreground or background mode
}
```

The model calls this tool with `{"task": "..."}`; the `task` string becomes the
sub-agent's initial prompt.

### 5.2 The isolation model (Claude-Code-style isolation)

A sub-agent starts in a **fresh RunLoop with an empty message history** — it cannot see
the parent conversation, which prevents context pollution and spares the parent from
carrying the sub-task's voluminous intermediate steps (it only gets the final result).
Config inheritance rules (`sub_agent_config()`):

- Copy the parent `RunConfig`; `max_turns` may be overridden by `def.max_turns`.
- `agent_name` is set to the child's name → approval requests identify their source.
- The `approval_handler` is shared via `Arc` → **the child's HITL approvals pop up
  directly in the parent's UI**.
- The shared session grant store is attached → AlwaysAllow works across agents (see 3.3).

### 5.3 Foreground mode (`background: false`)

`run_foreground()`: synchronously `run()`s the child to completion; the final output goes
back to the parent model as `ToolOutput::Text`. If the child hits max_turns, the output
is annotated with `[Sub-agent reached turn limit of N]`; a child error returns
`ToolOutput::Error` (**non-fatal to the parent run** — the parent model sees the error
string and decides to retry or change course).

### 5.4 Background mode (`background: true`)

`run_background()`, the full flow when a TaskStore is present:

1. **Register before spawn**: `create_task` first (description includes the agent name
   and the first 80 chars of the prompt) to get a `task_id`, so the parent model has a
   correlatable ID in the tool reply.
2. Spawn a detached tokio task: transition to `Running` → execute `run()` → on success
   `transition_task(Completed, output)` + `update_task_usage`; on failure
   `transition_task(Failed, error)`.
3. **Panic isolation**: the actual `run()` is wrapped in an inner `tokio::spawn`, so if
   the child panics the outer bookkeeping task can still record `Failed` — otherwise the
   store would be stuck at `Running` forever and the main loop's wait logic would
   deadlock.
4. Reply to the parent model immediately: `Background task started: task_id=…`, with an
   explicit note that the result will arrive as a `[background task completed]`
   notification and **not to draw conclusions before it does**.

Without a TaskStore this degrades to fire-and-forget (results are lost; only an
incrementing sequence number is returned) — suitable only for side-channel tasks whose
results don't matter.

---

## 6. Coordinating multiple sub-agents from the main agent

### 6.1 Concurrent execution

`SubAgentTool::concurrency()` returns `Concurrency::Safe` — sub-agents are isolated from
each other and share no mutable state, so multiple sub-agent calls issued by the model
**in the same turn** run in parallel under the `StreamingToolExecutor` (`executor.rs`;
default concurrency cap 8, `Exclusive` tools run exclusively). Combined with background
mode, the parent model can fan out N sub-tasks at once and immediately move on.

### 6.2 How results get back into the model's conversation (notification protocol)

Background task results **must enter the conversation the model can read**, not just the
UI. Two paths share one deduplication mechanism:

**Path A — injection at turn boundaries** (`drain_task_notifications`, Phase 0.5):
At the start of every turn, `list_unacknowledged_terminal()` is queried and each
terminated-but-unacknowledged task is wrapped as:

```
[background task completed] Sub-agent 'researcher': … (task_id=…)
Result: <output>
```

(or `[background task failed]` + error on failure), injected as a user message, then
immediately `acknowledge_task`ed — **the acknowledged flag guarantees each result reaches
the model exactly once**, no duplicates, no losses.

**Path B — waiting before exit** (`await_background_tasks`, see check 2 in 2.2):
When the model wants to wrap up while tasks are still running, the loop blocks until the
next task terminates, injects the notification, and keeps going.

### 6.3 Coordination guarantees

| Guarantee | Mechanism |
|---|---|
| Results reach the model exactly once | `acknowledged` flag + acknowledge immediately after injection |
| No wrapping up before sub-tasks finish | `await_background_tasks` gate before `FinalOutput` |
| Parent can correlate delegation → result | task created before spawn to obtain `task_id`; the same ID comes back in the notification |
| A child panic can't cause an infinite wait | two-level spawn panic isolation → recorded as Failed → notified as usual |
| Waiting is bounded | 10-minute deadline + per-poll check that the stream hasn't been dropped |
| HITL consistency | shared approval handler + shared session grants (uniform across the agent tree) |
| Cost attribution | child usage/cost recorded in TaskEntry.usage; the parent can aggregate |

The CLI layer (`agent-cli`) also reads the TaskStore to render background task status,
but that is UI only; the model's knowledge always flows through the conversation
injection paths above. For deeper sequence diagrams and known limits see
[sub-agent-task-coordination.md](sub-agent-task-coordination.md).

---

## 7. Error handling and recovery

### 7.1 The error hierarchy (`error.rs`)

```
RunError (run level)
├── Model(ModelError)      ← API error / RateLimited / PromptTooLong /
│                             MaxOutputTokens / Connection / StreamInterrupted
├── Tool(ToolError)        ← InvalidInput / ExecutionFailed / Timeout / NotAvailable
├── MaxTurns / BudgetExceeded / Guardrail
├── Aborted                ← content filter, permission deny, dropped stream, budget
└── RecoveryExhausted      ← recovery retries exhausted
```

### 7.2 Layered handling principles

**Tool errors: non-fatal, fed back to the model.** A failed tool execution does not
terminate the run — the error string is written back into the conversation as an
`is_error: true` ToolResult, and the model corrects itself next turn (retry, change
arguments, change approach). The same applies when the model calls a nonexistent tool
(the `NotFoundTool` placeholder reports `NotAvailable`), and to foreground sub-agent
failures, which surface as `ToolOutput::Error`.

**Model errors: into the recovery system.** The `RecoveryTracker` (`recovery.rs`) maps
each `ModelError` to a strategy, counting attempts per error variant:

| Error | Strategy |
|---|---|
| `PromptTooLong` | `CompactAndRetry` — brute-force trim the oldest non-system messages down to half the context window (normal compaction is Phase 1's job; this is the last resort when the provider still refuses) |
| `MaxOutputTokens` | First 2 attempts: `ContinueMessage` (inject "please continue from where you left off"); 3rd: `EscalateOutputTokens` (double max_output_tokens, capped at the model's hard limit) |
| `StreamInterrupted` | `ContinueMessage` |
| Others (Api / RateLimited / Connection) | `GiveUp` |

Each error variant is **counted independently**; exceeding `MAX_RECOVERY_ATTEMPTS = 3`
means `GiveUp` → the run ends with `RunError::RecoveryExhausted`. Recovery retries
**do not consume turns**; any successful `Continue` `reset()`s all counters.

**Guardrails: hard termination.** Input guardrails check the input on the first turn;
output guardrails check the output before `FinalOutput`. They evaluate in registration
order with short-circuiting; any failure emits a `GuardrailTripped` event and returns
`RunError::Guardrail`.

**Background task errors: recorded as state + notified.** A sub-agent failure/panic is
recorded as `TaskEntry::Failed` (with `last_error`) and reported to the model as a
`[background task failed]` notification; the model decides how to remediate. With
`max_retries > 0` the store automatically resets the task to Pending for a retry.

### 7.3 Uniqueness of the terminal event

Streaming mode guarantees exactly one terminal event — one of `AgentEnd`, `MaxTurns`,
`Aborted`, `Error`, `Interruption`, `GuardrailTripped` — which the TUI relies on to
settle its UI state.

---

## 8. End-to-end example: the full lifecycle of a long task

**Scenario**: the user asks — "Analyze the performance bottlenecks of modules A and B in
this repo, then produce a combined report written to report.md." The main agent has
`todolist`, `file_write` (`ApprovalRequirement::Always`), and an `analyzer` sub-agent
with `background: true`.

**Turn 1 — Decompose the plan (Todo tool)**
The model calls `todolist add` ×3: "analyze module A", "analyze module B", "synthesize
report into report.md". All three tool calls are approval level `Never`; PermissionEngine
Layer 4 allows them → `NextStep::Continue`.

**Turn 2 — Parallel delegation (Sub-Agent + Task)**
The model issues two `analyzer` calls in the same turn (tasks for modules A and B) and
marks the first two todos `in_progress`. `SubAgentTool` is `Concurrency::Safe`, so the
executor runs both calls in parallel: each first `create_task`s to get `task_id=T1`, `T2`
(`TaskEntry::Pending`), spawns a detached task, and immediately replies "Background task
started: task_id=T1… don't draw conclusions until the notification arrives". Both
sub-agents start running (`Running`) in their own blank RunLoops, unable to see the
parent conversation.

**Turn 3 — A sub-agent triggers HITL**
Sub-agent A needs to run `bash` for profiling (`Always`). Its own loop resolves an
`Interruption`, which surfaces in the parent's TUI via the **shared approval handler**,
with `ApprovalContext.agent_name = "analyzer"` identifying the source. The user picks
**AlwaysAllow(`Bash(cargo*)`)** → written to the **shared session grant store**. When
sub-agent B later runs the same command, Layer 3 allows it directly — the user is never
asked twice.

**Turn 4 — The model tries to wrap up early and is intercepted**
With nothing left to do, the model outputs "both analyses are running; I'll synthesize
when they finish" and `EndTurn` → `FinalOutput`. But `await_background_tasks` finds T1
and T2 still `Running`, **intercepts the exit**, and blocks. Meanwhile sub-agent A hits
`MaxOutputTokens` internally — its own `RecoveryTracker` injects a ContinueMessage and it
keeps writing, without affecting the parent.

**Turn 5 — Results flow back**
Sub-agent A finishes: `transition_task(T1, Completed, output)` + usage recorded. The
waiting parent loop drains `[background task completed] … (task_id=T1) Result: <module A
analysis>`, acknowledges T1 (exactly once), injects the user message, and continues. The
model checks the "analyze module A" todo off as `completed`. Same for B (if B panics, a
`[background task failed]` arrives instead and the model can retry in the foreground).

**Turn 6 — Writing the file triggers HITL (main level)**
The model synthesizes both results and calls `file_write` for report.md. `Always` →
`Interruption`; the TUI shows a diff and the user presses **Allow** (one-time). The
result is kept and the third todo is checked off as `completed`.

**Turn 7 — A legitimate wrap-up**
The model outputs a summary and `EndTurn` → `FinalOutput`. All three checks pass in
order: output guardrails OK; no Pending/Running in the TaskStore (T1 and T2 both
acknowledged); the TodoList fully `Completed` (had the model forgotten the third item, a
continuation prompt would pull it back here, up to 3 times). `AgentEnd` is emitted and
the `RunResult` carries the output, accumulated usage/cost, and the full `RunState`.

This example exercised: the NextStep state machine (Continue / Interruption /
FinalOutput), the three pre-exit checks, the 4-layer permission engine and cross-agent
session grants, the division between the Todo planning layer and the Task execution
layer, the background sub-agent's register-execute-notify-acknowledge loop, parallel
coordination, and the independent error recovery at the model layer and the task layer.
