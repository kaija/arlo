//! Transition applier for the agent run loop.
//!
//! `apply_transition` owns every state change in the loop: turn increment,
//! turn-limit predicate, the three `FinalOutput` gates in documented order,
//! recovery attempt bookkeeping, and approval-response pairing.
//!
//! `drive()` calls this function after resolving `NextStep` and acts only on
//! the returned `LoopDecision` — it never mutates `RunState` directly.
//!
//! # Why this shape?
//!
//! The applier takes concrete values (`max_turns`, `context_window`, etc.) rather
//! than a `&Model`. This keeps tests free of the nine-method `Model` mock: tests
//! construct a `RunState`, call `apply_transition`, and assert on state + decision.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::compaction::tokens::estimate_tokens;
use crate::config::{ApprovalContext, ApprovalResponse, RunConfig, RunResult};
use crate::error::RunError;
use crate::event::RunEvent;
use crate::executor::ToolResult;
use crate::guardrail::OutputGuardrail;
use crate::message::{ContentBlock, Message, Usage};
use crate::next_step::{NextStep, PendingApproval, RecoveryStrategy};
use crate::recovery::{RecoveryKey, RecoveryTracker, MAX_RECOVERY_ATTEMPTS};
use crate::state::RunState;
use crate::task_store::TaskStatus;
use crate::tool::ToolOutput;

/// Maximum consecutive todo-aware continuations before a stuck agent terminates.
pub const TODO_CONTINUATION_CAP: u32 = 3;

// ── Public types ─────────────────────────────────────────────────────────────

/// The outcome returned by `apply_transition` to `drive()`.
///
/// `drive()` switches on this value; it never inspects `RunState` to decide
/// whether to loop or return — that decision lives here.
pub enum LoopDecision {
    /// Keep looping — `apply_transition` has already updated `RunState`.
    Continue,
    /// Return the successful result to the caller.
    Terminal(Box<RunResult>),
    /// Terminate the run with this error.
    Error(RunError),
}

/// All values that `apply_transition` reads from `drive()`'s local scope.
///
/// Using a struct avoids a ten-argument signature and makes call sites readable.
/// No `&Model` — the one value the recovery path needs (context window size) is
/// passed as a plain `usize` so tests never construct a mock model.
pub struct TransitionInput<'a> {
    /// The `NextStep` resolved this turn.
    pub next_step: NextStep,
    /// Content blocks produced by the model this turn.
    pub assistant_content: Vec<ContentBlock>,
    /// Token usage reported for this turn.
    pub usage: Usage,
    /// Tool results returned from the executor (may be empty).
    pub tool_results: Vec<ToolResult>,
    /// Effective max output tokens (may be escalated by recovery).
    pub effective_max_output_tokens: &'a mut Option<u32>,
    /// Recovery tracker shared with `drive()`.
    pub recovery_tracker: &'a mut RecoveryTracker,
    /// Consecutive todo-continuation counter shared with `drive()`.
    pub todo_continuation_count: &'a mut u32,
    /// The configured (or agent-overridden) max turns for this run.
    pub max_turns: u32,
    /// Model context-window size in tokens; only needed for CompactAndRetry.
    pub context_window: usize,
    /// Run configuration (task store, approval handler, permissions, agent name).
    pub config: &'a mut RunConfig,
    /// Agent name for terminal events.
    pub agent_name: String,
    /// Output guardrails to check before delivering a final output.
    pub output_guardrails: &'a [Arc<dyn OutputGuardrail>],
    /// Optional stream sender for emitting events.
    pub tx: Option<&'a mpsc::Sender<RunEvent>>,
}

// ── Main entry point ─────────────────────────────────────────────────────────

/// Apply a `NextStep` to `RunState`, returning the loop's control-flow decision.
///
/// This is the single authoritative place for:
/// - turn counter increment
/// - turn-limit predicate (one predicate, one exit)
/// - `FinalOutput` gates in documented order: guardrails → background await → todo
/// - recovery attempt bookkeeping (typed key, no string literals)
/// - approval-response pairing and denial injection
///
/// `drive()` calls this once per resolved step and acts only on the returned
/// `LoopDecision`; it does not touch `RunState` after this call.
pub async fn apply_transition(state: &mut RunState, input: TransitionInput<'_>) -> LoopDecision {
    let TransitionInput {
        next_step,
        assistant_content,
        usage,
        tool_results,
        effective_max_output_tokens,
        recovery_tracker,
        todo_continuation_count,
        max_turns: _,
        context_window,
        config,
        agent_name,
        output_guardrails,
        tx,
    } = input;

    match next_step {
        // ── Continue ─────────────────────────────────────────────────────────
        NextStep::Continue => {
            recovery_tracker.reset();
            *todo_continuation_count = 0;
            push_turn_messages(state, assistant_content, usage, &tool_results);
            state.current_turn += 1;
            LoopDecision::Continue
        }

        // ── FinalOutput ──────────────────────────────────────────────────────
        NextStep::FinalOutput { text, structured } => {
            apply_final_output(
                state,
                assistant_content,
                usage,
                text,
                structured,
                todo_continuation_count,
                config,
                output_guardrails,
                agent_name,
                tx,
            )
            .await
        }

        // ── MaxTurns ─────────────────────────────────────────────────────────
        NextStep::MaxTurns { count } => {
            emit(tx, RunEvent::MaxTurns { count }).await;
            LoopDecision::Terminal(Box::new(build_result_max_turns(state)))
        }

        // ── Aborted ──────────────────────────────────────────────────────────
        NextStep::Aborted { reason } => {
            emit(
                tx,
                RunEvent::Aborted {
                    reason: reason.clone(),
                },
            )
            .await;
            LoopDecision::Error(RunError::Aborted(reason))
        }

        // ── Interruption ─────────────────────────────────────────────────────
        NextStep::Interruption { pending } => {
            apply_interruption(
                state,
                assistant_content,
                usage,
                tool_results,
                pending,
                config,
                tx,
            )
            .await
        }

        // ── Recovery ─────────────────────────────────────────────────────────
        NextStep::Recovery { strategy } => {
            apply_recovery(
                state,
                &strategy,
                effective_max_output_tokens,
                context_window,
                recovery_tracker,
                tx,
            )
            .await
        }
    }
}

// ── FinalOutput gate logic ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn apply_final_output(
    state: &mut RunState,
    assistant_content: Vec<ContentBlock>,
    usage: Usage,
    text: String,
    structured: Option<serde_json::Value>,
    todo_continuation_count: &mut u32,
    config: &mut RunConfig,
    output_guardrails: &[Arc<dyn OutputGuardrail>],
    agent_name: String,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> LoopDecision {
    // Gate 1: output guardrails — failure short-circuits before all other gates.
    if let Some((name, reason)) =
        check_output_guardrails(output_guardrails, &text, structured.as_ref()).await
    {
        emit(
            tx,
            RunEvent::GuardrailTripped {
                name: name.clone(),
                reason: reason.clone(),
            },
        )
        .await;
        return LoopDecision::Error(RunError::Guardrail(format!("{}: {}", name, reason)));
    }

    // Gate 2: background-task await — never finish while sub-agents are running.
    if let Some(notification) = await_background_tasks(config, tx).await {
        push_and_continue(state, assistant_content, usage, notification);
        return LoopDecision::Continue;
    }

    // Gate 3: todo-aware continuation — max TODO_CONTINUATION_CAP consecutive times.
    if *todo_continuation_count < TODO_CONTINUATION_CAP {
        if let Some(continuation) = todo_continuation_prompt(config).await {
            push_and_continue(state, assistant_content, usage, continuation);
            *todo_continuation_count += 1;
            return LoopDecision::Continue;
        }
    }

    // All gates passed — deliver the final output.
    state.messages.push(Message::Assistant {
        content: assistant_content,
        usage: Some(usage),
    });
    state.current_turn += 1;

    let total_usage = state.total_usage.clone();
    emit(
        tx,
        RunEvent::AgentEnd {
            agent: agent_name,
            output: text.clone(),
            usage: total_usage.clone(),
        },
    )
    .await;

    LoopDecision::Terminal(Box::new(RunResult {
        output: text,
        structured,
        usage: total_usage,
        cost_usd: state.total_cost_usd,
        turns: state.current_turn,
        state: state.clone(),
    }))
}

/// Push assistant + user continuation message, then increment turn.
/// Written once to prevent the three copies in the original code from diverging.
fn push_and_continue(
    state: &mut RunState,
    assistant_content: Vec<ContentBlock>,
    usage: Usage,
    user_text: String,
) {
    state.messages.push(Message::Assistant {
        content: assistant_content,
        usage: Some(usage),
    });
    state.messages.push(Message::User {
        content: vec![ContentBlock::Text { text: user_text }],
    });
    state.current_turn += 1;
}

// ── Interruption logic ────────────────────────────────────────────────────────

async fn apply_interruption(
    state: &mut RunState,
    assistant_content: Vec<ContentBlock>,
    usage: Usage,
    tool_results: Vec<ToolResult>,
    pending: Vec<PendingApproval>,
    config: &mut RunConfig,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> LoopDecision {
    let Some(handler) = config.approval_handler.clone() else {
        // No handler: surface the interruption and pause the run.
        state.pending_approvals = pending.clone();
        emit(
            tx,
            RunEvent::Interruption {
                pending: pending.clone(),
            },
        )
        .await;
        let output = extract_text_from_content(&assistant_content);
        return LoopDecision::Terminal(Box::new(RunResult {
            output,
            structured: None,
            usage: state.total_usage.clone(),
            cost_usd: state.total_cost_usd,
            turns: state.current_turn,
            state: state.clone(),
        }));
    };

    let context = ApprovalContext {
        agent_name: config.agent_name.clone(),
        pending: pending.clone(),
    };
    let responses = handler.request_approval(&context).await;

    // Pair each pending approval with its response, then filter tool results.
    let (final_tool_results, denials) =
        pair_approvals(pending, responses, tool_results, &mut config.permissions);

    push_turn_messages(state, assistant_content, usage, &final_tool_results);

    // Inject denial results for each denied tool.
    for (tool_use_id, tool_name) in denials {
        state.messages.push(Message::ToolResult {
            tool_use_id,
            content: format!(
                "Permission denied: tool '{}' was not approved by the user.",
                tool_name
            ),
            is_error: true,
        });
    }

    state.current_turn += 1;
    LoopDecision::Continue
}

/// Pair pending approvals with responses, partition tool results into kept/denied.
///
/// Returns `(kept_results, denied_pairs)` where each denied pair is
/// `(tool_use_id, tool_name)` for denial injection. Extracted so it can be
/// tested independently against malformed or partial response lists.
pub fn pair_approvals(
    pending: Vec<PendingApproval>,
    responses: Vec<ApprovalResponse>,
    tool_results: Vec<ToolResult>,
    permissions: &mut crate::permission::PermissionEngine,
) -> (Vec<ToolResult>, Vec<(String, String)>) {
    // Build a lookup: tool_use_id → response
    let decisions: std::collections::HashMap<String, &ApprovalResponse> = pending
        .iter()
        .zip(responses.iter())
        .filter_map(|(pa, resp)| {
            pa.request_id
                .strip_prefix("approval-")
                .map(|id| (id.to_string(), resp))
        })
        .collect();

    let mut kept = Vec::new();
    let mut denied: Vec<(String, String)> = Vec::new();

    for tr in tool_results {
        match decisions.get(tr.tool_use_id.as_str()) {
            Some(ApprovalResponse::Deny) => {
                denied.push((tr.tool_use_id, tr.tool_name));
            }
            Some(ApprovalResponse::AlwaysAllow { pattern }) => {
                permissions.grant_session_allow(pattern);
                kept.push(tr);
            }
            // Allow or no matching decision (tool was not pending approval)
            _ => kept.push(tr),
        }
    }

    (kept, denied)
}

// ── Recovery logic ────────────────────────────────────────────────────────────

async fn apply_recovery(
    state: &mut RunState,
    strategy: &RecoveryStrategy,
    effective_max_output_tokens: &mut Option<u32>,
    context_window: usize,
    recovery_tracker: &mut RecoveryTracker,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> LoopDecision {
    match do_recovery(state, strategy, effective_max_output_tokens, context_window) {
        RecoveryOutcome::Retry => {
            // Track MaxOutputTokens-related attempts with a typed key.
            if matches!(
                strategy,
                RecoveryStrategy::ContinueMessage { .. }
                    | RecoveryStrategy::EscalateOutputTokens { .. }
            ) {
                recovery_tracker.increment_typed(RecoveryKey::MaxOutputTokens);
            }
            // Recovery retries deliberately do NOT increment the turn counter.
            LoopDecision::Continue
        }
        RecoveryOutcome::GiveUp(error) => {
            emit(tx, RunEvent::Error { error }).await;
            LoopDecision::Error(RunError::RecoveryExhausted(MAX_RECOVERY_ATTEMPTS))
        }
    }
}

enum RecoveryOutcome {
    Retry,
    GiveUp(String),
}

/// Pure (non-async) recovery mutation: applies the strategy to `state`.
fn do_recovery(
    state: &mut RunState,
    strategy: &RecoveryStrategy,
    effective_max_output_tokens: &mut Option<u32>,
    context_window: usize,
) -> RecoveryOutcome {
    match strategy {
        RecoveryStrategy::CompactAndRetry => {
            // ponytail: brute-force snip to half the context window; graceful
            // compaction is the pipeline's job — this only fires when the
            // provider still rejects the prompt as too long.
            snip_history(&mut state.messages, context_window / 2);
            RecoveryOutcome::Retry
        }
        RecoveryStrategy::ContinueMessage { .. } => {
            state.messages.push(Message::User {
                content: vec![ContentBlock::Text {
                    text: "Please continue from where you left off.".to_string(),
                }],
            });
            RecoveryOutcome::Retry
        }
        RecoveryStrategy::EscalateOutputTokens { max } => {
            // Double the current value, capped at the model-reported max.
            // context_window is used as a proxy when no model max is available.
            let ceiling = context_window as u32;
            let new_max = if *max > 0 {
                (*max).min(ceiling)
            } else {
                let current = effective_max_output_tokens.unwrap_or(4096);
                (current * 2).min(ceiling)
            };
            *effective_max_output_tokens = Some(new_max);
            RecoveryOutcome::Retry
        }
        RecoveryStrategy::GiveUp { error } => RecoveryOutcome::GiveUp(error.clone()),
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Append the turn's assistant message and tool result messages to state.
pub fn push_turn_messages(
    state: &mut RunState,
    assistant_content: Vec<ContentBlock>,
    usage: Usage,
    tool_results: &[ToolResult],
) {
    state.messages.push(Message::Assistant {
        content: assistant_content,
        usage: Some(usage),
    });
    for tr in tool_results {
        let (content, is_error) = match &tr.result {
            Ok(output) => (tool_output_to_string(output), false),
            Err(e) => (format!("{}", e), true),
        };
        state.messages.push(Message::ToolResult {
            tool_use_id: tr.tool_use_id.clone(),
            content,
            is_error,
        });
    }
}

/// Convert a ToolOutput to its string representation.
pub fn tool_output_to_string(output: &ToolOutput) -> String {
    match output {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Structured(v) => serde_json::to_string(v).unwrap_or_default(),
        ToolOutput::Error(s) => s.clone(),
    }
}

/// Extract text content from assistant content blocks.
pub fn extract_text_from_content(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Build a RunResult for a MaxTurns termination.
pub fn build_result_max_turns(state: &RunState) -> RunResult {
    let output = state
        .messages
        .iter()
        .rev()
        .find_map(|msg| match msg {
            Message::Assistant { content, .. } => Some(extract_text_from_content(content)),
            _ => None,
        })
        .unwrap_or_default();
    RunResult {
        output,
        structured: None,
        usage: state.total_usage.clone(),
        cost_usd: state.total_cost_usd,
        turns: state.current_turn,
        state: state.clone(),
    }
}

/// Remove oldest non-system messages until the token estimate fits `max_tokens`.
pub fn snip_history(messages: &mut Vec<Message>, max_tokens: usize) {
    while estimate_tokens(messages) > max_tokens {
        let last_user = messages
            .iter()
            .rposition(|m| matches!(m, Message::User { .. }));
        let removable = messages
            .iter()
            .enumerate()
            .position(|(idx, m)| !matches!(m, Message::System { .. }) && Some(idx) != last_user);
        match removable {
            Some(idx) => {
                messages.remove(idx);
            }
            None => break,
        }
    }
}

/// Send an event to the stream consumer; no-op when `tx` is `None`.
async fn emit(tx: Option<&mpsc::Sender<RunEvent>>, event: RunEvent) -> bool {
    match tx {
        Some(tx) => tx.send(event).await.is_ok(),
        None => true,
    }
}

// ── Async helpers (previously in run_loop.rs) ─────────────────────────────────

/// Check output guardrails sequentially; short-circuit at first failure.
async fn check_output_guardrails(
    guardrails: &[Arc<dyn OutputGuardrail>],
    output: &str,
    structured: Option<&serde_json::Value>,
) -> Option<(String, String)> {
    for guardrail in guardrails {
        let result = guardrail.check(output, structured).await;
        if !result.passed {
            let reason = result
                .reason
                .unwrap_or_else(|| "guardrail check failed".to_string());
            return Some((guardrail.name().to_string(), reason));
        }
    }
    None
}

/// Build the todo-aware continuation prompt if the task store has incomplete todos.
async fn todo_continuation_prompt(config: &RunConfig) -> Option<String> {
    let store = config.task_store.as_ref()?;
    let todos = store.list_todos().await.ok()?;
    let incomplete: Vec<_> = todos
        .iter()
        .filter(|t| t.status != crate::task_store::TodoStatus::Completed)
        .collect();
    if incomplete.is_empty() {
        return None;
    }
    let todo_summary: Vec<String> = incomplete
        .iter()
        .map(|t| {
            format!(
                "- [{}] {}",
                match t.status {
                    crate::task_store::TodoStatus::Pending => " ",
                    crate::task_store::TodoStatus::InProgress => "~",
                    crate::task_store::TodoStatus::Completed => "x",
                },
                t.content
            )
        })
        .collect();
    Some(format!(
        "You have {} incomplete todo item(s). Continue working through them:\n{}",
        incomplete.len(),
        todo_summary.join("\n")
    ))
}

/// Drain unacknowledged terminal background tasks into a notification string.
///
/// Acknowledges each entry immediately so results reach the model exactly once.
pub async fn drain_task_notifications(config: &RunConfig) -> Option<String> {
    let store = config.task_store.as_ref()?;
    let tasks = store.list_unacknowledged_terminal().await.ok()?;
    if tasks.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for task in &tasks {
        let line = match task.status {
            TaskStatus::Completed => format!(
                "[background task completed] {} (task_id={})\nResult: {}",
                task.description,
                task.id,
                task.output.as_deref().unwrap_or("(no output)")
            ),
            _ => format!(
                "[background task failed] {} (task_id={})\nError: {}",
                task.description,
                task.id,
                task.last_error
                    .as_deref()
                    .or(task.output.as_deref())
                    .unwrap_or("(no details)")
            ),
        };
        lines.push(line);
        let _ = store.acknowledge_task(&task.id).await;
    }
    Some(lines.join("\n\n"))
}

/// Block until at least one unfinished background task reaches a terminal state,
/// then drain and return its notification. Returns `None` when there is no task
/// store, no unfinished tasks, or the stream consumer dropped.
pub async fn await_background_tasks(
    config: &RunConfig,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Option<String> {
    let store = config.task_store.as_ref()?;
    // ponytail: 200ms polling with a 10-min ceiling; switch to a store-side
    // notify channel if sub-agents ever legitimately run longer.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if let Some(text) = drain_task_notifications(config).await {
            return Some(text);
        }
        let counts = store.count_by_status().await.ok()?;
        if counts.pending == 0 && counts.running == 0 {
            return None;
        }
        if Instant::now() >= deadline {
            return None;
        }
        if let Some(tx) = tx {
            if tx.is_closed() {
                return None;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use proptest::prelude::*;

    use super::*;
    use crate::config::{ApprovalResponse, RunConfig};
    use crate::error::{ModelError, RunError};
    use crate::executor::ToolResult;
    use crate::guardrail::{GuardrailResult, OutputGuardrail};
    use crate::in_memory_task_store::InMemoryTaskStore;
    use crate::message::{ContentBlock, Message, Usage};
    use crate::model::{Model, ModelProvider};
    use crate::next_step::{NextStep, PendingApproval, RecoveryStrategy};
    use crate::permission::{PermissionEngine, PermissionMode};
    use crate::recovery::RecoveryTracker;
    use crate::state::RunState;
    use crate::task_store::{CreateTaskParams, TaskStatus, TaskStore, TaskType};
    use crate::tool::ToolOutput;

    // ── Test helpers ─────────────────────────────────────────────────────────

    /// Minimal mock provider to satisfy RunConfig::builder.
    struct MockProvider;

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn resolve(&self, _model_name: &str) -> Result<Arc<dyn Model>, ModelError> {
            Err(ModelError::Connection("mock".into()))
        }
        fn available_models(&self) -> Vec<String> {
            vec![]
        }
    }

    fn mock_config() -> RunConfig {
        RunConfig::builder(Arc::new(MockProvider), "mock")
            .max_turns(10)
            .build()
    }

    fn mock_config_with_store(store: Arc<InMemoryTaskStore>) -> RunConfig {
        RunConfig::builder(Arc::new(MockProvider), "mock")
            .max_turns(10)
            .task_store(store as Arc<dyn crate::task_store::TaskStore>)
            .build()
    }

    fn fresh_state() -> RunState {
        let mut s = RunState::new("run-test".into(), None, Some(10));
        s.messages.push(Message::User {
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        });
        s
    }

    fn empty_usage() -> Usage {
        Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: None,
        }
    }

    fn text_content(t: &str) -> Vec<ContentBlock> {
        vec![ContentBlock::Text { text: t.into() }]
    }

    fn no_tool_results() -> Vec<ToolResult> {
        Vec::new()
    }

    /// Build a TransitionInput with sensible defaults; caller overrides what matters.
    fn make_input<'a>(
        next_step: NextStep,
        recovery_tracker: &'a mut RecoveryTracker,
        todo_count: &'a mut u32,
        eff_tokens: &'a mut Option<u32>,
        config: &'a mut RunConfig,
        guardrails: &'a [Arc<dyn OutputGuardrail>],
    ) -> TransitionInput<'a> {
        TransitionInput {
            next_step,
            assistant_content: text_content("answer"),
            usage: empty_usage(),
            tool_results: no_tool_results(),
            effective_max_output_tokens: eff_tokens,
            recovery_tracker,
            todo_continuation_count: todo_count,
            max_turns: 10,
            context_window: 100_000,
            config,
            agent_name: "test-agent".into(),
            output_guardrails: guardrails,
            tx: None,
        }
    }

    // ── Continue ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn continue_increments_turn_and_resets_todo_count() {
        let mut state = fresh_state();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count: u32 = 2;
        let mut eff = None;
        let mut config = mock_config();

        let decision = apply_transition(
            &mut state,
            make_input(
                NextStep::Continue,
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        assert!(matches!(decision, LoopDecision::Continue));
        assert_eq!(state.current_turn, 1);
        assert_eq!(todo_count, 0, "todo_count must reset on Continue");
    }

    #[tokio::test]
    async fn continue_pushes_assistant_and_tool_messages() {
        let mut state = fresh_state();
        let initial_len = state.messages.len();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config();

        apply_transition(
            &mut state,
            make_input(
                NextStep::Continue,
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        // Should have added one assistant message (no tools)
        assert_eq!(state.messages.len(), initial_len + 1);
        assert!(matches!(
            state.messages.last(),
            Some(Message::Assistant { .. })
        ));
    }

    // ── Turn limit ────────────────────────────────────────────────────────────

    /// The turn-limit boundary: at exactly max_turns - 1 a Continue still works;
    /// at max_turns a Continue triggers MaxTurns (via resolve_next_step → MaxTurns
    /// variant, tested separately). This test pins that Continue at the limit
    /// returns MaxTurns when the applier evaluates it *before* the increment.
    ///
    /// Specifically: we feed NextStep::MaxTurns and assert Terminal is returned
    /// without touching the turn counter.
    #[tokio::test]
    async fn max_turns_step_returns_terminal_without_incrementing() {
        let mut state = fresh_state();
        state.current_turn = 9;
        let initial_turn = state.current_turn;
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config();

        let decision = apply_transition(
            &mut state,
            make_input(
                NextStep::MaxTurns { count: 10 },
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        assert!(matches!(decision, LoopDecision::Terminal(_)));
        assert_eq!(
            state.current_turn, initial_turn,
            "MaxTurns must not increment the turn counter"
        );
    }

    #[tokio::test]
    async fn max_turns_at_boundary_pins_exact_count() {
        let max = 5u32;
        let mut state = fresh_state();
        state.current_turn = max; // exactly at the limit
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = RunConfig::builder(Arc::new(MockProvider), "mock")
            .max_turns(max)
            .build();

        let decision = apply_transition(
            &mut state,
            TransitionInput {
                next_step: NextStep::MaxTurns { count: max },
                assistant_content: text_content("x"),
                usage: empty_usage(),
                tool_results: vec![],
                effective_max_output_tokens: &mut eff,
                recovery_tracker: &mut tracker,
                todo_continuation_count: &mut todo_count,
                max_turns: max,
                context_window: 100_000,
                config: &mut config,
                agent_name: "a".into(),
                output_guardrails: &[],
                tx: None,
            },
        )
        .await;

        // Result turns field should reflect state.current_turn (not incremented)
        if let LoopDecision::Terminal(r) = decision {
            assert_eq!(r.turns, max, "turns in result must equal max_turns");
        } else {
            panic!("expected Terminal");
        }
    }

    // ── FinalOutput gate ordering ─────────────────────────────────────────────

    /// Guardrail failure short-circuits before the background-task gate.
    #[tokio::test]
    async fn finaloutput_gate1_guardrail_failure_short_circuits() {
        struct FailGuardrail;
        #[async_trait]
        impl OutputGuardrail for FailGuardrail {
            fn name(&self) -> &str {
                "fail"
            }
            async fn check(&self, _: &str, _: Option<&serde_json::Value>) -> GuardrailResult {
                GuardrailResult::fail("blocked")
            }
        }

        let guardrails: Vec<Arc<dyn OutputGuardrail>> = vec![Arc::new(FailGuardrail)];
        let store = Arc::new(InMemoryTaskStore::new());
        // Register a pending background task — if Gate 2 fires, this would block.
        store
            .create_task(CreateTaskParams {
                description: "bg".into(),
                task_type: TaskType::Background,
                dependencies: vec![],
                max_retries: 0,
            })
            .await
            .unwrap();

        let mut state = fresh_state();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config_with_store(store);

        let decision = apply_transition(
            &mut state,
            TransitionInput {
                next_step: NextStep::FinalOutput {
                    text: "out".into(),
                    structured: None,
                },
                assistant_content: text_content("out"),
                usage: empty_usage(),
                tool_results: vec![],
                effective_max_output_tokens: &mut eff,
                recovery_tracker: &mut tracker,
                todo_continuation_count: &mut todo_count,
                max_turns: 10,
                context_window: 100_000,
                config: &mut config,
                agent_name: "a".into(),
                output_guardrails: &guardrails,
                tx: None,
            },
        )
        .await;

        // Must be an error, not a Continue (which would mean Gate 2 ran first)
        assert!(
            matches!(decision, LoopDecision::Error(RunError::Guardrail(_))),
            "guardrail failure must produce Error, not Continue"
        );
    }

    /// Background-task gate fires before todo-continuation gate.
    #[tokio::test]
    async fn finaloutput_gate2_bg_task_blocks_before_todo_gate() {
        let store = Arc::new(InMemoryTaskStore::new());

        // Complete a background task (unacknowledged) so drain_task_notifications fires.
        let task_id = store
            .create_task(CreateTaskParams {
                description: "bg-task".into(),
                task_type: TaskType::SubAgent,
                dependencies: vec![],
                max_retries: 0,
            })
            .await
            .unwrap();
        store
            .transition_task(&task_id, TaskStatus::Running, None)
            .await
            .unwrap();
        store
            .transition_task(&task_id, TaskStatus::Completed, Some("done".into()))
            .await
            .unwrap();

        // Also add an incomplete todo — if Gate 3 fired, it would inject a prompt.
        store
            .add_todo("unfinished work".into(), None)
            .await
            .unwrap();

        let mut state = fresh_state();
        let initial_turn = state.current_turn;
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config_with_store(store);

        let decision = apply_transition(
            &mut state,
            TransitionInput {
                next_step: NextStep::FinalOutput {
                    text: "out".into(),
                    structured: None,
                },
                assistant_content: text_content("out"),
                usage: empty_usage(),
                tool_results: vec![],
                effective_max_output_tokens: &mut eff,
                recovery_tracker: &mut tracker,
                todo_continuation_count: &mut todo_count,
                max_turns: 10,
                context_window: 100_000,
                config: &mut config,
                agent_name: "a".into(),
                output_guardrails: &[],
                tx: None,
            },
        )
        .await;

        // Gate 2 should have fired: result is Continue (not Terminal)
        assert!(
            matches!(decision, LoopDecision::Continue),
            "background task should produce Continue, not Terminal"
        );
        assert_eq!(state.current_turn, initial_turn + 1);
        // Todo counter must NOT have been incremented (Gate 3 never ran)
        assert_eq!(todo_count, 0);
        // The bg notification must have been injected as a user message
        let last = state.messages.last().unwrap();
        assert!(
            matches!(last, Message::User { .. }),
            "last message should be the background task notification"
        );
    }

    /// Todo continuation fires (up to cap) when no bg tasks are pending.
    #[tokio::test]
    async fn finaloutput_gate3_todo_continuation_increments_counter() {
        let store = Arc::new(InMemoryTaskStore::new());
        store.add_todo("todo item".into(), None).await.unwrap();

        let mut state = fresh_state();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config_with_store(store);

        let decision = apply_transition(
            &mut state,
            TransitionInput {
                next_step: NextStep::FinalOutput {
                    text: "done".into(),
                    structured: None,
                },
                assistant_content: text_content("done"),
                usage: empty_usage(),
                tool_results: vec![],
                effective_max_output_tokens: &mut eff,
                recovery_tracker: &mut tracker,
                todo_continuation_count: &mut todo_count,
                max_turns: 10,
                context_window: 100_000,
                config: &mut config,
                agent_name: "a".into(),
                output_guardrails: &[],
                tx: None,
            },
        )
        .await;

        assert!(matches!(decision, LoopDecision::Continue));
        assert_eq!(todo_count, 1);
    }

    /// The todo-continuation cap: after TODO_CONTINUATION_CAP consecutive
    /// continuations, the next FinalOutput terminates even with incomplete todos.
    #[tokio::test]
    async fn finaloutput_todo_cap_terminates_after_three() {
        let store = Arc::new(InMemoryTaskStore::new());
        store
            .add_todo("persistent todo".into(), None)
            .await
            .unwrap();

        let mut state = fresh_state();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = TODO_CONTINUATION_CAP; // already at the cap
        let mut eff = None;
        let mut config = mock_config_with_store(store);

        let decision = apply_transition(
            &mut state,
            TransitionInput {
                next_step: NextStep::FinalOutput {
                    text: "final".into(),
                    structured: None,
                },
                assistant_content: text_content("final"),
                usage: empty_usage(),
                tool_results: vec![],
                effective_max_output_tokens: &mut eff,
                recovery_tracker: &mut tracker,
                todo_continuation_count: &mut todo_count,
                max_turns: 10,
                context_window: 100_000,
                config: &mut config,
                agent_name: "a".into(),
                output_guardrails: &[],
                tx: None,
            },
        )
        .await;

        // Must terminate, not continue — the cap has been reached.
        assert!(
            matches!(decision, LoopDecision::Terminal(_)),
            "should terminate when todo_continuation_count == cap"
        );
    }

    /// A normal Continue resets the todo counter so a long successful run is not cut short.
    #[tokio::test]
    async fn continue_resets_todo_continuation_count() {
        let mut state = fresh_state();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 2u32;
        let mut eff = None;
        let mut config = mock_config();

        apply_transition(
            &mut state,
            make_input(
                NextStep::Continue,
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        assert_eq!(todo_count, 0, "todo_count must be reset by Continue");
    }

    // ── Background task exactly-once delivery ─────────────────────────────────

    /// A completed background task's result is injected exactly once.
    #[tokio::test]
    async fn bg_task_result_delivered_exactly_once() {
        let store = Arc::new(InMemoryTaskStore::new());
        let task_id = store
            .create_task(CreateTaskParams {
                description: "sub-agent result".into(),
                task_type: TaskType::SubAgent,
                dependencies: vec![],
                max_retries: 0,
            })
            .await
            .unwrap();
        store
            .transition_task(&task_id, TaskStatus::Running, None)
            .await
            .unwrap();
        store
            .transition_task(&task_id, TaskStatus::Completed, Some("the result".into()))
            .await
            .unwrap();

        let mut state = fresh_state();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config_with_store(store.clone());

        // First FinalOutput: should receive the notification
        apply_transition(
            &mut state,
            TransitionInput {
                next_step: NextStep::FinalOutput {
                    text: "x".into(),
                    structured: None,
                },
                assistant_content: text_content("x"),
                usage: empty_usage(),
                tool_results: vec![],
                effective_max_output_tokens: &mut eff,
                recovery_tracker: &mut tracker,
                todo_continuation_count: &mut todo_count,
                max_turns: 10,
                context_window: 100_000,
                config: &mut config,
                agent_name: "a".into(),
                output_guardrails: &[],
                tx: None,
            },
        )
        .await;

        // Task should now be acknowledged
        let entry = store.get_task(&task_id).await.unwrap();
        assert!(
            entry.acknowledged,
            "task must be acknowledged after first delivery"
        );

        // Second call: no more notifications for the same task
        let notification = drain_task_notifications(&config).await;
        assert!(
            notification.is_none(),
            "task result must not be delivered a second time"
        );
    }

    // ── Recovery ─────────────────────────────────────────────────────────────

    /// Recovery retries do NOT increment the turn counter.
    #[tokio::test]
    async fn recovery_does_not_increment_turn() {
        let mut state = fresh_state();
        let initial_turn = state.current_turn;
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config();

        let decision = apply_transition(
            &mut state,
            make_input(
                NextStep::Recovery {
                    strategy: RecoveryStrategy::ContinueMessage { attempt: 1 },
                },
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        assert!(matches!(decision, LoopDecision::Continue));
        assert_eq!(
            state.current_turn, initial_turn,
            "recovery must not increment the turn counter"
        );
    }

    /// ContinueMessage/EscalateOutputTokens recoveries increment the typed key.
    #[tokio::test]
    async fn recovery_increments_typed_key_for_max_output_tokens_strategies() {
        use crate::recovery::RecoveryKey;

        let mut state = fresh_state();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config();

        apply_transition(
            &mut state,
            make_input(
                NextStep::Recovery {
                    strategy: RecoveryStrategy::ContinueMessage { attempt: 1 },
                },
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        assert_eq!(tracker.attempts_for_typed(RecoveryKey::MaxOutputTokens), 1);

        apply_transition(
            &mut state,
            make_input(
                NextStep::Recovery {
                    strategy: RecoveryStrategy::EscalateOutputTokens { max: 8192 },
                },
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        assert_eq!(tracker.attempts_for_typed(RecoveryKey::MaxOutputTokens), 2);
    }

    /// CompactAndRetry does NOT increment the MaxOutputTokens counter.
    #[tokio::test]
    async fn recovery_compact_and_retry_does_not_increment_max_output_tokens_key() {
        use crate::recovery::RecoveryKey;

        let mut state = fresh_state();
        // Give it some messages to snip
        state.messages.push(Message::Assistant {
            content: text_content("old"),
            usage: Some(empty_usage()),
        });
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config();

        apply_transition(
            &mut state,
            make_input(
                NextStep::Recovery {
                    strategy: RecoveryStrategy::CompactAndRetry,
                },
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        assert_eq!(
            tracker.attempts_for_typed(RecoveryKey::MaxOutputTokens),
            0,
            "CompactAndRetry must not count against the MaxOutputTokens budget"
        );
    }

    /// GiveUp recovery returns an Error decision.
    #[tokio::test]
    async fn recovery_give_up_returns_error() {
        let mut state = fresh_state();
        let mut tracker = RecoveryTracker::new();
        let mut todo_count = 0u32;
        let mut eff = None;
        let mut config = mock_config();

        let decision = apply_transition(
            &mut state,
            make_input(
                NextStep::Recovery {
                    strategy: RecoveryStrategy::GiveUp {
                        error: "too many retries".into(),
                    },
                },
                &mut tracker,
                &mut todo_count,
                &mut eff,
                &mut config,
                &[],
            ),
        )
        .await;

        assert!(matches!(
            decision,
            LoopDecision::Error(RunError::RecoveryExhausted(_))
        ));
    }

    // ── pair_approvals ────────────────────────────────────────────────────────

    #[test]
    fn pair_approvals_deny_removes_result_and_returns_denial() {
        let pending = vec![PendingApproval {
            tool_name: "shell".into(),
            tool_input: serde_json::json!({}),
            request_id: "approval-tool-1".into(),
        }];
        let responses = vec![ApprovalResponse::Deny];
        let tool_results = vec![ToolResult {
            tool_use_id: "tool-1".into(),
            tool_name: "shell".into(),
            result: Ok(ToolOutput::Text("done".into())),
        }];
        let mut perms = PermissionEngine::new(PermissionMode::Bypass);

        let (kept, denied) = pair_approvals(pending, responses, tool_results, &mut perms);

        assert!(kept.is_empty());
        assert_eq!(denied.len(), 1);
        assert_eq!(denied[0].0, "tool-1");
        assert_eq!(denied[0].1, "shell");
    }

    #[test]
    fn pair_approvals_allow_keeps_result() {
        let pending = vec![PendingApproval {
            tool_name: "shell".into(),
            tool_input: serde_json::json!({}),
            request_id: "approval-tool-1".into(),
        }];
        let responses = vec![ApprovalResponse::Allow];
        let tool_results = vec![ToolResult {
            tool_use_id: "tool-1".into(),
            tool_name: "shell".into(),
            result: Ok(ToolOutput::Text("ok".into())),
        }];
        let mut perms = PermissionEngine::new(PermissionMode::Bypass);

        let (kept, denied) = pair_approvals(pending, responses, tool_results, &mut perms);

        assert_eq!(kept.len(), 1);
        assert!(denied.is_empty());
    }

    #[test]
    fn pair_approvals_always_allow_grants_session_and_keeps() {
        let pending = vec![PendingApproval {
            tool_name: "shell".into(),
            tool_input: serde_json::json!({}),
            request_id: "approval-tool-1".into(),
        }];
        let responses = vec![ApprovalResponse::AlwaysAllow {
            pattern: "shell".into(),
        }];
        let tool_results = vec![ToolResult {
            tool_use_id: "tool-1".into(),
            tool_name: "shell".into(),
            result: Ok(ToolOutput::Text("ok".into())),
        }];
        let mut perms = PermissionEngine::new(PermissionMode::Bypass);

        let (kept, denied) = pair_approvals(pending, responses, tool_results, &mut perms);

        assert_eq!(kept.len(), 1);
        assert!(denied.is_empty());
        // Session grant was applied — check via session grants (no additional API needed)
    }

    #[test]
    fn pair_approvals_non_pending_tool_passes_through() {
        // A tool that was not in the pending list should pass through unchanged.
        let pending: Vec<PendingApproval> = vec![];
        let responses: Vec<ApprovalResponse> = vec![];
        let tool_results = vec![ToolResult {
            tool_use_id: "tool-99".into(),
            tool_name: "file_read".into(),
            result: Ok(ToolOutput::Text("content".into())),
        }];
        let mut perms = PermissionEngine::new(PermissionMode::Bypass);

        let (kept, denied) = pair_approvals(pending, responses, tool_results, &mut perms);

        assert_eq!(kept.len(), 1);
        assert!(denied.is_empty());
    }

    // ── Property tests ────────────────────────────────────────────────────────

    proptest! {
        /// Turn counter is monotonically non-decreasing after Continue.
        /// (Recovery is the only step that legitimately skips increment.)
        #[test]
        fn prop_continue_turn_monotone(start_turn in 0u32..50u32) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let mut state = fresh_state();
                state.current_turn = start_turn;
                let mut tracker = RecoveryTracker::new();
                let mut todo_count = 0u32;
                let mut eff = None;
                let mut config = mock_config();

                apply_transition(
                    &mut state,
                    make_input(NextStep::Continue, &mut tracker, &mut todo_count, &mut eff, &mut config, &[]),
                )
                .await;

                prop_assert_eq!(state.current_turn, start_turn + 1);
                Ok(())
            }).unwrap();
        }

        /// Recovery never increments the turn counter, regardless of strategy type.
        #[test]
        fn prop_recovery_never_increments_turn(start_turn in 0u32..50u32) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let mut state = fresh_state();
                state.current_turn = start_turn;
                // Give messages so CompactAndRetry has something to snip
                state.messages.push(Message::Assistant {
                    content: text_content("old"),
                    usage: Some(empty_usage()),
                });
                let mut tracker = RecoveryTracker::new();
                let mut todo_count = 0u32;
                let mut eff = None;
                let mut config = mock_config();

                apply_transition(
                    &mut state,
                    make_input(
                        NextStep::Recovery {
                            strategy: RecoveryStrategy::ContinueMessage { attempt: 1 },
                        },
                        &mut tracker,
                        &mut todo_count,
                        &mut eff,
                        &mut config,
                        &[],
                    ),
                )
                .await;

                prop_assert_eq!(state.current_turn, start_turn,
                    "recovery must never increment turn (was {}, now {})",
                    start_turn, state.current_turn);
                Ok(())
            }).unwrap();
        }
    }
} // mod tests
