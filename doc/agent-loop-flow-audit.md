# Agent Loop Flow — Ponytail Audit

Audit date: 2026-07-09. **Status: all findings applied on 2026-07-09** — see
"Applied fixes" at the end for what changed. Part 1 below describes the loop
as it was at audit time; the structural changes from the fixes are summarized
in that section.

Scope: the agent run loop and its direct collaborators in
`crates/agent-core` (`run_loop.rs`, `next_step.rs`, `recovery.rs`, `executor.rs`,
`event.rs`, `stream.rs`, `state.rs`, `agent.rs`) plus the two consumers in
`crates/agent-cli` (single-prompt mode in `main.rs`, TUI in `tui/mod.rs`).

Part 1 records how the loop actually flows today. Part 2 is the ranked
over-engineering findings (ponytail-audit format). Part 3 lists observations
that belong to a normal correctness review, not this audit.

---

## Part 1 — How the loop flows today

### Entry points

| Entry | Location | Caller | Notes |
|-------|----------|--------|-------|
| `run()` | `run_loop.rs:46` | `agent-cli/src/main.rs:321` (single-prompt mode) | Blocking; returns `RunResult` |
| `run_stream()` | `run_loop.rs:491` | `agent-cli/src/tui/mod.rs:146,193` (TUI) | Returns `RunStream` of `RunEvent` |

Both entry points contain a **full, independent copy** of the same turn logic.
`run()` is a 430-line loop; `run_stream()` drives `StreamState::execute_turn()`
(`run_loop.rs:610`), another ~370 lines that repeat the same phases nearly
verbatim. See finding 1.

### Phases of one turn (identical in both copies)

```
loop {
  Phase 0   Turn limit check            current_turn >= max_turns → MaxTurns
  Phase 1   Compaction                  CompactionPipeline::compact() (4-layer)
                                        └ returned CompactionEvent is discarded
  Phase 1.5 Input guardrails            first turn only; trip → error/terminal
  Phase 2   Prepare ModelRequest        system prompt (+ current datetime),
                                        tool definitions, messages clone
  Phase 3   Stream model                model.stream(); consume_stream() collects
                                        text deltas, tool_use blocks, stop reason,
                                        usage. Errors → RecoveryTracker strategy.
  Phase 4   Execute tools               StreamingToolExecutor: Safe tools parallel
                                        (semaphore, default 8), Exclusive tools
                                        alone, error cascade via CancellationToken.
                                        Results returned in enqueue order.
  Phase 4.5 Usage/budget                accumulate_usage(); budget_usd exceeded
                                        → Aborted("budget_exceeded")
  Phase 5   resolve_next_step()         run_loop.rs:1282 — pure decision function
  Phase 6   Apply NextStep transition   see table below
}
```

### `resolve_next_step` decision order (`run_loop.rs:1282`)

1. `StopReason::ContentFilter` → `Aborted("content_filter")`
2. `current_turn + 1 >= max_turns` → `MaxTurns`
3. `StopReason::ToolUse` with tools → per-tool permission check
   (`config.permissions.check`): any `NeedsApproval` → `Interruption { pending }`;
   any `Deny` → `Aborted("permission_denied…")`; all allowed → `Continue(ToolUse)`
4. `StopReason::ToolUse` with **no** tools (edge case) → `FinalOutput`
5. `StopReason::MaxTokens` → `Recovery`: `ContinueMessage` for attempts 1–2,
   then `EscalateOutputTokens { max: 8192 }`
6. `EndTurn` / `StopSequence` → `FinalOutput` (text parsed as JSON when
   `agent.output_schema` is set)

Note: tools have **already executed** in Phase 4 before permissions are checked
in Phase 5 — approval decides whether the result is *kept*, not whether the tool
*runs*. See Part 3.

### NextStep transition table (Phase 6)

| NextStep | `run()` behaviour | `run_stream()` behaviour |
|----------|-------------------|--------------------------|
| `Continue` | append assistant msg + tool results, `current_turn += 1`, loop | same, emits next `TurnStart` |
| `FinalOutput` | output guardrails → todo-aware continuation check (below) → return `RunResult` | same → terminal `AgentEnd` |
| `MaxTurns` | `RunResult` from last assistant text | terminal `MaxTurns` event |
| `Aborted` | `Err(RunError::Aborted)` | terminal `Aborted` event |
| `Interruption` | with `approval_handler`: Allow keeps executed result, Deny replaces it with an error ToolResult, AlwaysAllow grants session pattern; loop continues. Without handler: return with `state.pending_approvals` set | same; without handler emits terminal `Interruption` event |
| `Recovery` | `apply_recovery_run()` mutates state and retries **without** incrementing turn | same |
| `BudgetContinue` | append messages, continue | same — **never produced; dead** (finding 4) |

### Todo-aware continuation (both copies, `run_loop.rs:259` / `run_loop.rs:780`)

On `FinalOutput`, if `config.task_store` has incomplete todos and fewer than 3
consecutive continuations have fired, the loop appends the assistant message,
injects a synthetic user message listing the incomplete todos, and continues
instead of terminating. Counter resets on any normal `Continue`.

### Recovery system (`recovery.rs`)

`RecoveryTracker` counts attempts per error-variant key; after
`MAX_RECOVERY_ATTEMPTS` (3) any variant escalates to `GiveUp`. Mapping:
`PromptTooLong → CompactAndRetry` (forced Snip via the **deprecated**
`ContextCompactor` — finding 2), `MaxOutputTokens → ContinueMessage ×2 then
EscalateOutputTokens`, `StreamInterrupted → ContinueMessage`, everything else →
`GiveUp` immediately.

### Event stream reality vs. declaration

`RunEvent` (`event.rs:21`) declares 13 variants and `run_stream`'s doc comment
promises `StreamChunk`, `ToolStart`, `ToolEnd`, `StepResolved` per phase. The
loop actually emits only: `TurnStart` (one per completed turn, *after* the turn
runs) and the terminal events (`AgentEnd`, `MaxTurns`, `Aborted`, `Error`,
`Interruption`, `GuardrailTripped`). `StreamChunk`, `ToolStart`, `ToolEnd`,
`SubAgentStart/End`, `Compaction`, `StepResolved` have **no producer** — the TUI
(`tui/app.rs:705-853`) carries handler code for all of them that can never fire
from a real run. See findings 3 and Part 3.

### Tool execution (`executor.rs`)

`StreamingToolExecutor` batches `Safe` tools behind a semaphore
(`config.concurrency_limit`, min 1), runs `Exclusive` tools alone after
draining the batch, cancels remaining tools when a tool with
`error_cascades()` fails, and returns results sorted back to enqueue order.
Despite the name and module docstring ("tools start executing during model
streaming"), the loop only calls it **after** `consume_stream` has fully
drained the model stream — execution is post-stream, batch-style.

---

## Part 2 — Ranked findings (biggest cut first)

1. `shrink:` **`run()` and `StreamState::execute_turn()` are two hand-maintained copies of the same ~400-line turn.** All six phases, the recovery arms, the 45-line todo-continuation block, and the 75-line approval-processing block are duplicated nearly verbatim (`run_loop.rs:46-476` vs `run_loop.rs:610-975`). Replacement: implement `run()` as a thin drain of `run_stream()` (collect events, build `RunResult`), or extract a single `execute_turn(&mut LoopState) -> TurnOutcome` both call. [crates/agent-core/src/run_loop.rs] (~-450 lines)

2. `delete:` **Deprecated `ContextCompactor` (`compactor.rs`, 1,201 lines).** The 4-layer `CompactionPipeline` replaced it; the only live use is `CompactAndRetry` building a one-stage Snip (`run_loop.rs:1014`). Replacement: a small snip/truncate helper (or a pipeline call) inside `apply_recovery_run`, then delete `compactor.rs`, the never-used `_compactor` at `run_loop.rs:70`, the `#[allow(dead_code)] compactor` field at `run_loop.rs:521`, and the `lib.rs` re-exports. [crates/agent-core/src/compactor.rs] (~-1,150 lines)

3. `yagni:` **Seven `RunEvent` variants with no producer** — `StreamChunk`, `ToolStart`, `ToolEnd`, `SubAgentStart`, `SubAgentEnd`, `Compaction`, `StepResolved` are declared (`event.rs:31-78`), documented as emitted (`run_loop.rs:481`), and handled by the TUI (`tui/app.rs:705-853`), but the loop never yields them; the pipeline's `CompactionEvent` is assigned to `_compaction_event` and dropped in both copies. Replacement: decide once — either wire them into `execute_turn` (this is what makes the TUI actually stream) or delete the variants, the TUI dead handlers, and the false doc comment. [crates/agent-core/src/event.rs] (-100 lines if deleted)

4. `delete:` **`NextStep::BudgetContinue` is never produced.** `resolve_next_step` has no arm returning it; both loops carry a full handler for it (`run_loop.rs:456`, `run_loop.rs:955`). Replacement: nothing. [crates/agent-core/src/next_step.rs:30] (~-45 lines)

5. `delete:` **`next_step.rs` tests that test the compiler.** `next_step_continue_debug_clone_partialeq`, `next_step_final_output`, `next_step_interruption`, `next_step_recovery_variants`, `next_step_budget_continue`, `next_step_max_turns`, `next_step_aborted`, `next_step_inequality` assert that `#[derive(Clone, PartialEq, Debug)]` works. Replacement: nothing (keep the two serde round-trip tests). [crates/agent-core/src/next_step.rs:76-205] (~-110 lines)

6. `delete:` **`AgentHooks` / `HookCallback` are never invoked.** `on_turn_start`, `on_turn_end`, `on_tool_start`, `on_tool_end` exist on `Agent` and in the builder, but no code in the loop (or anywhere) calls them. Replacement: nothing; re-add when the loop actually fires hooks. [crates/agent-core/src/agent.rs:64-79] (~-45 lines)

7. `delete:` **`RecoveryStrategy::FallbackModel` is never produced** and its handler is an explicit no-op retry (`run_loop.rs:1048`). `recovery.rs` never maps any error to it. Replacement: nothing. [crates/agent-core/src/next_step.rs:71] (~-15 lines)

8. `delete:` **`StreamingToolExecutor::next_remaining()` and `default_concurrency()` have no callers** outside the executor's own tests. `next_remaining` is the only reason the `pending`-draining select machinery is public. Replacement: nothing. [crates/agent-core/src/executor.rs:75,164] (~-35 lines)

9. `shrink:` **`run_stream` unfold has an identical-branch `if` and a one-variant state enum.** `let is_terminal = …; if is_terminal { Some((evt, ss)) } else { Some((evt, ss)) }` (`run_loop.rs:500-507`) — both branches equal, making `is_terminal_event()` and its test dead too. `StreamPhase` has exactly one variant and `phase` is never reassigned (`run_loop.rs:533-537`). Replacement: `event.map(|evt| (evt, ss))`, delete `StreamPhase`, `phase`, `is_terminal_event`, and `test_is_terminal_event`. [crates/agent-core/src/run_loop.rs:495] (~-60 lines)

10. `delete:` **`ContinueReason::PartialResponse` and `ContinueReason::Handoff` are never produced** — only `ToolUse` is ever constructed, and no consumer matches on the reason. Replacement: nothing (or drop the `reason` payload entirely). [crates/agent-core/src/next_step.rs:44-47] (~-8 lines)

11. `shrink:` **`error_variant_key` allocates a `String` per call to name an enum variant.** Replacement: return `&'static str` from the same match. [crates/agent-core/src/recovery.rs:108] (~-2 lines, removes per-error allocs)

**net: -1,970 lines, -0 deps possible.**

---

## Part 3 — Out of scope for this audit (flag for a normal review)

These are correctness/UX issues noticed while tracing the flow. Ponytail-audit
only hunts complexity, so they are recorded here without fixes:

- **The TUI can never actually stream.** Because no `StreamChunk`/`ToolStart`/
  `ToolEnd` events are emitted (finding 3), the user sees nothing between
  `TurnStart` and the terminal event — text deltas and tool activity arrive
  only as the final `AgentEnd` output.
- **Tools execute before permission is resolved.** Phase 4 runs every tool;
  Phase 5's permission check only decides whether the already-produced result
  is kept or replaced with a denial message. A denied `shell` command has
  already run by the time the user denies it.
- **`TurnStart` is emitted after the turn completes**, not at the start, so
  its name and the doc guarantee ("emitted at the start of each turn") are
  both misleading.
- **`run()` clones the entire `RunConfig` and `run_stream` clones `Agent` +
  `RunConfig` per call**, and `state.messages.clone()` is taken every turn for
  the `ModelRequest` — fine now, worth revisiting when histories get large.
- **Recovery retries don't increment `current_turn`**, so a model that
  alternates errors with `MaxTokens` responses can loop up to 3× per variant
  key between real turns; combined with `recovery_tracker.reset()` on every
  successful turn this is bounded but subtle.

---

## Applied fixes (2026-07-09)

All 11 findings were applied. The loop now works as follows:

- **One implementation.** `run()` and `run_stream()` both delegate to a single
  `drive()` function in `run_loop.rs`. `run()` calls it directly; `run_stream()`
  spawns it on a background task with an mpsc channel and returns the receiver
  as the event stream. The duplicated todo-continuation and approval-processing
  blocks now exist once (`push_turn_messages`, `todo_continuation_prompt`).
- **Events are real (finding 3 resolved by wiring, not deleting).** `drive()`
  now emits `TurnStart` (at the actual start of each turn), `StreamChunk` for
  every model chunk (the TUI now genuinely streams text deltas), `ToolStart`/
  `ToolEnd` around tool execution, `Compaction` when the pipeline acts, and
  `StepResolved` after each resolution. `SubAgentStart`/`SubAgentEnd` were
  deleted — the loop has no way to produce them. The TUI no longer re-appends
  the final output on `AgentEnd` since it already streamed in as deltas.
- **Abort semantics.** Dropping the stream stops the run at the next
  `TurnStart` or `StreamChunk` emission (`Aborted("stream_dropped")`), instead
  of the old instant future-drop. In-flight tool executions finish first.
- **Deprecated `compactor.rs` deleted** (~1,200 lines). `CompactionEvent`
  moved to `compaction/mod.rs`; `CompactAndRetry` recovery now uses a small
  `snip_history()` helper in `run_loop.rs`.
- **Dead variants removed:** `NextStep::BudgetContinue`, `ContinueReason`
  (whole enum — `Continue` is a unit variant now), `RecoveryStrategy::FallbackModel`.
- **Dead API removed:** `AgentHooks`/`HookCallback` (never invoked),
  `StreamingToolExecutor::next_remaining()`/`default_concurrency()` (no callers).
- **Derive-tests deleted** in `next_step.rs` and `event.rs` (tests that only
  asserted `#[derive(Clone, PartialEq, Debug)]` works); serde round-trip tests
  kept.
- **`error_variant_key` returns `&'static str`** and `RecoveryTracker` keys on
  it, removing a per-error `String` allocation.

Of the Part 3 observations, the first one (TUI can never stream) is fixed as a
side effect of wiring the events. Tools still execute before permission is
resolved — that remains open for a correctness review.
