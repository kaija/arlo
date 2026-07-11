# Main Agent / Sub-Agent / Task & Todo Coordination

Last updated: 2026-07-10. Describes the architecture **after** the
sub-agent result-delivery fix (turn-boundary notifications + background-task
wait in the run loop).

Scope: `crates/agent-core` (`run_loop.rs`, `sub_agent.rs`, `task_store.rs`,
`in_memory_task_store.rs`, `todolist_tool.rs`) and the two consumers in
`crates/agent-cli` (single-prompt mode, TUI).

---

## 1. The three moving parts

```
┌────────────────────────────────────────────────────────────────────┐
│                          Main agent run                            │
│  run() / run_stream() → drive()                    run_loop.rs     │
│                                                                    │
│  tools: todolist, sub_agent, shell, file_*, …                      │
└──────────────┬────────────────────────────┬────────────────────────┘
               │ tool call                  │ tool call
               ▼                            ▼
      ┌────────────────┐          ┌──────────────────────┐
      │  TodoListTool  │          │     SubAgentTool     │
      │ todolist_tool  │          │     sub_agent.rs     │
      └───────┬────────┘          └──────────┬───────────┘
              │ CRUD TodoItem                │ create TaskEntry, then
              │                              │ tokio::spawn(fresh run)
              ▼                              ▼
      ┌──────────────────────────────────────────────────┐
      │              TaskStore (shared Arc)              │
      │  task_store.rs / in_memory_task_store.rs         │
      │                                                  │
      │  TodoItem   — the PLAN    (what should happen)   │
      │  TaskEntry  — EXECUTION   (what is happening)    │
      └──────────────────────────────────────────────────┘
```

One `InMemoryTaskStore` is created per process (`main.rs`) and shared by
everything via `Arc<dyn TaskStore>`. It holds two **independent** collections:

| Collection  | Written by                         | Read by                                   | Purpose |
|-------------|------------------------------------|-------------------------------------------|---------|
| `TodoItem`  | Main agent via `todolist` tool     | Main agent, run loop (todo continuation), TUI | Visible plan / progress checklist |
| `TaskEntry` | `SubAgentTool` (lifecycle machine) | Run loop (notifications), TUI (`/tasks`)   | Background execution tracking |

**Todos are not tasks.** Creating a todo does not run anything; spawning a
sub-agent does not create a todo. The main agent is the only component that
links them: it delegates work with `sub_agent`, and when the result comes
back it marks the corresponding todo `completed`. Sub-agents do **not** have
the `todolist` tool and never touch the plan.

---

## 2. Lifecycles

### TodoItem (plan)

```
pending ──► in_progress ──► completed        (todolist tool actions:
                                              add / update / remove /
                                              clear_completed)
```

### TaskEntry (background execution) — `in_memory_task_store.rs`

```
             ┌──────────► Killed (terminal)
             │
Pending ──► Running ──► Completed (terminal)   output = sub-agent result
             │
             └────────► Failed   (terminal)    last_error = error message
                          │ (retries remaining)
                          └──► back to Pending, retry_count += 1
```

Every terminal entry carries an `acknowledged` flag. A result is delivered
to the model **exactly once**: whoever drains it (see §4) acknowledges it.

---

## 3. Spawning: what happens on a `sub_agent` tool call

`SubAgentTool::run_background` (`sub_agent.rs`):

1. **Register first** — `create_task()` on the shared store (status
   `Pending`, description = agent name + first 80 chars of the prompt).
   Registration happens *before* spawning so the tool result can return the
   real `task_id` to the model for correlation.
2. **Spawn detached** — `tokio::spawn` a fresh `run()` with **empty message
   history** (isolation: the sub-agent never sees the parent's
   conversation). A nested spawn shields the store bookkeeping from panics
   inside the run.
3. **Return immediately** — the tool result the model sees is:

   > `Background task started: task_id=<uuid>, agent='sub-agent'. Its result
   > will be delivered to you in a later message as a [background task
   > completed] notification — do not conclude before receiving it.`

4. The spawned task transitions the entry `Pending → Running`, executes the
   sub-agent run, then transitions to `Completed` (storing
   `result.output`) or `Failed` (storing the error), plus token/cost usage.

Foreground mode (`background: false`) skips the store entirely: the parent
blocks on the sub-agent run and gets its output as the tool result directly.

Sub-agent config notes (`sub_agent_config()`):
- shares the parent's `ApprovalHandler` (same `Arc`) and a shared
  session-grant store, so permission decisions delegate to the parent;
- may override `max_turns`;
- does **not** inherit the parent's `task_store` in the CLI wiring, so
  sub-agents are not subject to todo continuation and cannot consume the
  parent's notifications.

---

## 4. Result delivery: how the main agent learns a sub-agent finished

This mirrors Claude Code's model: results are injected into the conversation
at **turn boundaries**, and the run refuses to finish while tracked work is
still running.

Two hooks in `drive()` (`run_loop.rs`), both feeding on
`list_unacknowledged_terminal()`:

**Hook A — turn-start drain (Phase 0.5).** At the top of every turn, any
unacknowledged terminal task is formatted and appended to the conversation
as a user message, then acknowledged:

```
[background task completed] Sub-agent 'sub-agent': 1 + 1 (task_id=…)
Result: The result of 1 + 1 is 2.
```

Failures arrive as `[background task failed] … Error: …`.

**Hook B — completion wait at FinalOutput.** When the model tries to end the
run (`FinalOutput`) while the store still has `Pending`/`Running` tasks, the
loop does **not** terminate. `await_background_tasks()` polls the store
(200 ms interval, 10-minute ceiling, aborts if the stream consumer is
dropped) until the next task reaches a terminal state, injects its
notification, and continues the loop so the model can react.

Ordering of the FinalOutput checks in `drive()`:

```
FinalOutput
  ├─ output guardrails
  ├─ B: unfinished background tasks?  → wait, inject result, continue
  ├─ todo continuation (≤3): incomplete todos? → nudge prompt, continue
  └─ terminate: AgentEnd / RunResult
```

Hook B is bounded naturally — each spawned task delivers exactly one
notification — so it cannot loop forever the way an unbounded todo nudge
could (hence the separate 3-nudge cap on todo continuation).

---

## 5. End-to-end sequence (the "1+1 / 2+2 / 3+3 / 4+4" scenario)

```
User        Main agent (drive loop)      TaskStore           Sub-agents (spawned)
 │  prompt        │                          │                       │
 │───────────────►│                          │                       │
 │                │ todolist add ×4          │ 4 TodoItems (pending)  │
 │                │ sub_agent ×4 ────────────┤ 4 TaskEntries Pending │
 │                │   (returns task_ids)     │  → Running ───────────►│ run "1+1" …
 │                │ "waiting…" (FinalOutput) │                       │
 │                │ ◄─ Hook B waits ─────────┤ Completed("2") ◄──────┤
 │                │ [bg task completed] inj. │  acknowledged          │
 │                │ todolist update →done    │ TodoItem completed     │
 │                │   … repeats as each      │                       │
 │                │   result lands …         │                       │
 │                │ FinalOutput: no tasks    │                       │
 │                │ left, no todos left      │                       │
 │ ◄──────────────│ "Sum: 2+4+6+8 = 20"      │                       │
```

Key invariant: the run cannot reach `AgentEnd` while a spawned task is
non-terminal (up to the 10-minute ceiling), and every terminal task is
injected into the conversation exactly once before the run ends.

---

## 6. Who else reads the store (CLI layer)

| Consumer | When | What it does |
|----------|------|--------------|
| TUI tick poller (`tui/notifications.rs`) | Only when `AppMode::Idle` | Prints `Task completed: …` to the output buffer, appends a history message for the *next* run, acknowledges. Gated to Idle so it never steals notifications from an in-flight run (the run loop is the consumer while running). |
| `/tasks` command (`tui/commands.rs`) | On demand | Lists/acknowledges tasks for the user. |
| Single-prompt mode (`main.rs`) | n/a | No poller; Hooks A/B in the run loop are the only delivery path, which also prevents the process from exiting with results stranded in detached tokio tasks. |

---

## 7. Known limits / upgrade paths

- **Polling, not push** — Hook B polls at 200 ms. Fine at this scale; switch
  to a store-side `tokio::sync::Notify` channel if sub-agents ever
  legitimately run long or task counts grow.
- **No `task_status` tool** — the model cannot query the registry on demand;
  it only receives push notifications. Turn-boundary injection makes this
  unnecessary for the current flows; add a read-only tool if the model ever
  needs to selectively wait.
- **10-minute wait ceiling** — a hung sub-agent releases the parent after
  10 min; the stranded entry is later picked up by the Idle-mode TUI poller.
- **In-memory only** — the store dies with the process; no persistence
  across sessions.
- **Todo↔task linkage is conventional** — nothing in the store ties a
  `TaskEntry` to a `TodoItem`; the model maintains the mapping. Add a
  `todo_id` field on `TaskEntry` only if automated reconciliation is ever
  needed.
