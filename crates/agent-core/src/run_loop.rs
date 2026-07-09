//! Main loop (RunLoop) implementation: `run()` and `run_stream()` entry points.
//!
//! Both entry points share a single implementation (`drive`) that executes
//! phases in order: context compaction → prepare request → stream model →
//! execute tools → resolve next step → apply state transition. When driven
//! through `run_stream`, `drive` emits `RunEvent`s over a channel as each
//! phase happens (including per-chunk `StreamChunk` events).

use std::sync::Arc;

use futures::{stream, StreamExt};
use tokio::sync::mpsc;
use tracing::Instrument;
use uuid::Uuid;

use crate::agent::{Agent, Instructions, RunContext};
use crate::compaction::config::CompactionLayerConfig;
use crate::compaction::tokens::{compute_token_count, estimate_tokens};
use crate::compaction::CompactionPipeline;
use crate::config::{ApprovalContext, ApprovalResponse, Input, RunConfig, RunResult};
use crate::error::RunError;
use crate::event::{RunEvent, RunStream};
use crate::executor::StreamingToolExecutor;
use crate::guardrail::{InputGuardrail, OutputGuardrail};
use crate::message::{ContentBlock, Message, ToolUseBlock, Usage};
use crate::model::{ModelRequest, ToolDefinition};
use crate::next_step::{NextStep, PendingApproval, RecoveryStrategy};
use crate::permission::PermissionDecision;
use crate::recovery::RecoveryTracker;
use crate::state::RunState;
use crate::stream::{StopReason, StreamChunk};
use crate::tool::{ToolContext, ToolOutput};

/// Run an agent to completion, returning the final result.
///
/// This is the primary non-streaming entry point. It drives the RunLoop
/// through all phases until a terminal NextStep is reached (FinalOutput,
/// MaxTurns, Aborted, or an unrecoverable error).
///
/// # Arguments
/// * `agent` — The agent configuration defining tools, instructions, etc.
/// * `input` — How to initialize the run (Fresh prompt, existing Items, or Resume).
/// * `config` — Run configuration including provider, model, limits.
///
/// # Returns
/// `Ok(RunResult)` on successful completion, `Err(RunError)` on failure.
pub async fn run(agent: &Agent, input: Input, config: &RunConfig) -> Result<RunResult, RunError> {
    drive(agent, input, config, None).await
}

/// Run an agent and return a stream of `RunEvent`s.
///
/// This is the streaming entry point. The run executes on a background task
/// and yields events as each phase happens: `TurnStart`, `StreamChunk` (per
/// model chunk), `Compaction`, `ToolStart`/`ToolEnd`, `StepResolved`, and
/// exactly one terminal event (`AgentEnd`, `MaxTurns`, `Aborted`, `Error`,
/// `Interruption`, or `GuardrailTripped`).
///
/// Dropping the returned stream stops the run at the next event emission.
pub fn run_stream(agent: &Agent, input: Input, config: &RunConfig) -> RunStream {
    let agent = agent.clone();
    let config = config.clone();
    let (tx, rx) = mpsc::channel(256);

    tokio::spawn(async move {
        // Terminal outcome is reported via events; the result is redundant here.
        let _ = drive(&agent, input, &config, Some(&tx)).await;
    });

    Box::pin(stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (event, rx))
    }))
}

/// Send an event to the stream consumer, if one is attached.
///
/// Returns `false` when the consumer dropped the stream (channel closed).
async fn emit(tx: Option<&mpsc::Sender<RunEvent>>, event: RunEvent) -> bool {
    match tx {
        Some(tx) => tx.send(event).await.is_ok(),
        None => true,
    }
}

/// The single RunLoop implementation backing both `run` and `run_stream`.
///
/// When `tx` is `Some`, phase events are emitted as they happen and the
/// terminal outcome is emitted as exactly one terminal `RunEvent` before
/// returning.
async fn drive(
    agent: &Agent,
    input: Input,
    config: &RunConfig,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<RunResult, RunError> {
    // Clone config to allow mutable access for session grants (AlwaysAllow responses)
    let mut config = config.clone();

    // Initialize state from input
    let mut state = initialize_state(agent, &input);

    // Create the root tracing span for this agent run.
    // The span is stored to keep it alive for the duration of the run.
    // Child spans (model.call, tool.execute) are created within this context.
    let _run_span = tracing::info_span!(
        "agent.run",
        run_id = %state.run_id,
        agent_name = %agent.name,
    );

    // Resolve the model
    let model = match config.provider.resolve(&config.model).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "model_resolution_error");
            emit(tx, RunEvent::Error { error: format!("{}", e) }).await;
            return Err(RunError::Model(e));
        }
    };

    // Determine effective max_turns (agent override takes precedence)
    let max_turns = agent.max_turns.unwrap_or(config.max_turns);

    // 4-layer compaction pipeline
    let mut pipeline = CompactionPipeline::new(CompactionLayerConfig::default());

    // Recovery tracker for error-to-strategy mapping and attempt counting
    let mut recovery_tracker = RecoveryTracker::new();

    // Effective max_output_tokens that can be escalated during recovery
    let mut effective_max_output_tokens = config.max_output_tokens;

    // Counter for consecutive todo-aware continuations (prevents infinite loops)
    let mut todo_continuation_count: u32 = 0;

    // Main loop
    loop {
        // Phase 0: Check turn limit
        if state.current_turn >= max_turns {
            emit(tx, RunEvent::MaxTurns { count: max_turns }).await;
            return Ok(build_result_max_turns(&state));
        }

        tracing::info!(turn = state.current_turn, "turn_start");
        // The only place we check for a dropped stream consumer — once per turn
        // is enough to stop abandoned runs without threading the check everywhere.
        if !emit(
            tx,
            RunEvent::TurnStart {
                turn: state.current_turn + 1,
                agent: agent.name.clone(),
            },
        )
        .await
        {
            return Err(RunError::Aborted("stream_dropped".to_string()));
        }

        // Phase 1: Context compaction (4-layer pipeline)
        let token_count = compute_token_count(
            &state.messages,
            state.messages.iter().rev().find_map(|m| match m {
                Message::Assistant { usage, .. } => usage.as_ref(),
                _ => None,
            }),
        );
        let compaction_event = pipeline
            .compact(
                &mut state.messages,
                &mut state.compaction_state,
                token_count,
                model.context_window(),
                model.max_output_tokens(),
                state.current_turn,
                Some(model.as_ref()),
            )
            .await;
        if let Some(ce) = compaction_event {
            emit(
                tx,
                RunEvent::Compaction {
                    stage: ce.stage,
                    messages_removed: ce.messages_affected,
                },
            )
            .await;
        }

        // Phase 1.5: Input guardrails (first turn only)
        if state.current_turn == 0 {
            if let Some((guardrail_name, reason)) =
                check_input_guardrails(&agent.input_guardrails, &state.messages).await
            {
                tracing::error!(guardrail = %guardrail_name, reason = %reason, "input_guardrail_tripped");
                emit(
                    tx,
                    RunEvent::GuardrailTripped {
                        name: guardrail_name.clone(),
                        reason: reason.clone(),
                    },
                )
                .await;
                return Err(RunError::Guardrail(format!(
                    "{}: {}",
                    guardrail_name, reason
                )));
            }
        }

        // Phase 2: Prepare model request
        let system = resolve_instructions(agent, &state).await;
        let tool_defs = build_tool_definitions(agent);
        let request = ModelRequest {
            system,
            messages: state.messages.clone(),
            tools: tool_defs,
            max_tokens: effective_max_output_tokens,
            temperature: config.temperature,
            output_schema: agent.output_schema.clone(),
        };

        // Phase 3: Stream model response, handling errors via the recovery system
        let model_call_span = tracing::info_span!("model.call", model = %model.name());
        let model_stream = match model.stream(request).instrument(model_call_span).await {
            Ok(s) => s,
            Err(model_error) => {
                tracing::error!(error = %model_error, "model_error");
                let strategy = recovery_tracker.resolve_strategy(&model_error);
                match apply_recovery_run(
                    &strategy,
                    &mut state,
                    &mut effective_max_output_tokens,
                    model.as_ref(),
                ) {
                    RecoveryOutcome::Retry => continue,
                    RecoveryOutcome::GiveUp(error) => {
                        emit(tx, RunEvent::Error { error }).await;
                        return Err(RunError::RecoveryExhausted(
                            recovery_tracker.attempts_for(&model_error),
                        ));
                    }
                }
            }
        };

        let (assistant_content, stop_reason, usage, tool_uses) =
            match consume_stream(model_stream, tx).await {
                Ok(r) => r,
                Err(RunError::Model(model_error)) => {
                    tracing::error!(error = %model_error, "model_stream_error");
                    let strategy = recovery_tracker.resolve_strategy(&model_error);
                    match apply_recovery_run(
                        &strategy,
                        &mut state,
                        &mut effective_max_output_tokens,
                        model.as_ref(),
                    ) {
                        RecoveryOutcome::Retry => continue,
                        RecoveryOutcome::GiveUp(error) => {
                            emit(tx, RunEvent::Error { error }).await;
                            return Err(RunError::RecoveryExhausted(
                                recovery_tracker.attempts_for(&model_error),
                            ));
                        }
                    }
                }
                // Non-model errors (e.g. dropped stream consumer) end the run as-is.
                Err(other) => return Err(other),
            };

        // Phase 4: Execute tools via StreamingToolExecutor
        let tool_results = if !tool_uses.is_empty() {
            for tu in &tool_uses {
                emit(
                    tx,
                    RunEvent::ToolStart {
                        id: tu.id.clone(),
                        name: tu.name.clone(),
                    },
                )
                .await;
            }
            let results = execute_tools(agent, &tool_uses, &config, &state).await;
            for tr in &results {
                let (output, is_error) = match &tr.result {
                    Ok(o) => (tool_output_to_string(o), false),
                    Err(e) => (format!("{}", e), true),
                };
                emit(
                    tx,
                    RunEvent::ToolEnd {
                        id: tr.tool_use_id.clone(),
                        name: tr.tool_name.clone(),
                        output,
                        is_error,
                    },
                )
                .await;
            }
            results
        } else {
            Vec::new()
        };

        // Accumulate usage
        accumulate_usage(&mut state, &usage, model.as_ref());

        // Budget enforcement: check if cost exceeds configured budget
        if let Some(budget) = config.budget_usd {
            if state.total_cost_usd > budget {
                tracing::error!(budget = budget, cost = state.total_cost_usd, "budget_exceeded");
                emit(
                    tx,
                    RunEvent::Aborted {
                        reason: "budget_exceeded".to_string(),
                    },
                )
                .await;
                return Err(RunError::Aborted("budget_exceeded".to_string()));
            }
        }

        // Phase 5: Resolve NextStep
        let next_step = resolve_next_step(
            &stop_reason,
            &assistant_content,
            &tool_uses,
            &state,
            max_turns,
            agent,
            &config,
            recovery_tracker.attempts_for_key("MaxOutputTokens"),
        );
        emit(tx, RunEvent::StepResolved(next_step.clone())).await;

        // Phase 6: Apply state transition
        match next_step {
            NextStep::Continue => {
                // Reset recovery tracker on successful continuation
                recovery_tracker.reset();
                todo_continuation_count = 0;
                push_turn_messages(&mut state, assistant_content, usage, &tool_results);
                state.current_turn += 1;
                // Loop back
            }

            NextStep::FinalOutput { text, structured } => {
                // Check output guardrails before delivering the final output
                if let Some((guardrail_name, reason)) =
                    check_output_guardrails(
                        &agent.output_guardrails,
                        &text,
                        structured.as_ref(),
                    )
                    .await
                {
                    emit(
                        tx,
                        RunEvent::GuardrailTripped {
                            name: guardrail_name.clone(),
                            reason: reason.clone(),
                        },
                    )
                    .await;
                    return Err(RunError::Guardrail(format!(
                        "{}: {}",
                        guardrail_name, reason
                    )));
                }

                // Todo-aware continuation: if there are incomplete todos, inject a
                // continuation prompt instead of terminating (max 3 consecutive continuations).
                if todo_continuation_count < 3 {
                    if let Some(continuation) = todo_continuation_prompt(&config).await {
                        state.messages.push(Message::Assistant {
                            content: assistant_content,
                            usage: Some(usage),
                        });
                        state.messages.push(Message::User {
                            content: vec![ContentBlock::Text { text: continuation }],
                        });
                        state.current_turn += 1;
                        todo_continuation_count += 1;
                        continue;
                    }
                }

                // Append the final assistant message to state
                state.messages.push(Message::Assistant {
                    content: assistant_content,
                    usage: Some(usage),
                });
                state.current_turn += 1;
                emit(
                    tx,
                    RunEvent::AgentEnd {
                        agent: agent.name.clone(),
                        output: text.clone(),
                        usage: state.total_usage.clone(),
                    },
                )
                .await;
                return Ok(RunResult {
                    output: text,
                    structured,
                    usage: state.total_usage.clone(),
                    cost_usd: state.total_cost_usd,
                    turns: state.current_turn,
                    state,
                });
            }

            NextStep::MaxTurns { count } => {
                emit(tx, RunEvent::MaxTurns { count }).await;
                return Ok(build_result_max_turns(&state));
            }

            NextStep::Aborted { reason } => {
                emit(tx, RunEvent::Aborted { reason: reason.clone() }).await;
                return Err(RunError::Aborted(reason));
            }

            NextStep::Interruption { pending } => {
                if let Some(handler) = config.approval_handler.clone() {
                    // Inline approval: delegate to handler and process responses
                    let context = ApprovalContext {
                        agent_name: config.agent_name.clone(),
                        pending: pending.clone(),
                    };
                    let responses = handler.request_approval(&context).await;

                    // Pair each pending approval with its response.
                    // The request_id format is "approval-{tool_use_id}"
                    let approval_decisions: Vec<(&PendingApproval, &ApprovalResponse)> =
                        pending.iter().zip(responses.iter()).collect();

                    // Process each tool result: keep allowed ones, drop denied ones
                    let mut final_tool_results = Vec::new();
                    for tr in tool_results {
                        let pending_match = approval_decisions.iter().find(|(pa, _)| {
                            pa.request_id == format!("approval-{}", tr.tool_use_id)
                        });

                        match pending_match {
                            Some((_pa, ApprovalResponse::Deny)) => {
                                // Tool denied — we'll inject a denial result below
                            }
                            Some((_pa, ApprovalResponse::AlwaysAllow { pattern })) => {
                                // Grant session-wide permission then keep result
                                config.permissions.grant_session_allow(pattern);
                                final_tool_results.push(tr);
                            }
                            // Allowed, or not a pending-approval tool: keep as-is
                            _ => final_tool_results.push(tr),
                        }
                    }

                    push_turn_messages(&mut state, assistant_content, usage, &final_tool_results);

                    // Inject denial results for denied tools
                    for (pa, response) in &approval_decisions {
                        if matches!(response, ApprovalResponse::Deny) {
                            let tool_use_id = pa.request_id
                                .strip_prefix("approval-")
                                .unwrap_or(&pa.request_id)
                                .to_string();
                            state.messages.push(Message::ToolResult {
                                tool_use_id,
                                content: format!(
                                    "Permission denied: tool '{}' was not approved by the user.",
                                    pa.tool_name
                                ),
                                is_error: true,
                            });
                        }
                    }

                    state.current_turn += 1;
                    // Continue the loop — the model will see the results on next turn
                } else {
                    // No handler: return with the pending approvals recorded in state
                    state.pending_approvals = pending.clone();
                    emit(tx, RunEvent::Interruption { pending }).await;
                    let output = extract_text_from_content(&assistant_content);
                    return Ok(RunResult {
                        output,
                        structured: None,
                        usage: state.total_usage.clone(),
                        cost_usd: state.total_cost_usd,
                        turns: state.current_turn,
                        state,
                    });
                }
            }

            NextStep::Recovery { strategy } => {
                match apply_recovery_run(
                    &strategy,
                    &mut state,
                    &mut effective_max_output_tokens,
                    model.as_ref(),
                ) {
                    RecoveryOutcome::Retry => {
                        // Track the attempt for MaxTokens-related recoveries
                        if matches!(
                            strategy,
                            RecoveryStrategy::ContinueMessage { .. }
                                | RecoveryStrategy::EscalateOutputTokens { .. }
                        ) {
                            recovery_tracker.increment_key("MaxOutputTokens");
                        }
                        // Don't increment turn count for recovery retries
                        continue;
                    }
                    RecoveryOutcome::GiveUp(error) => {
                        emit(tx, RunEvent::Error { error }).await;
                        return Err(RunError::RecoveryExhausted(
                            crate::recovery::MAX_RECOVERY_ATTEMPTS,
                        ));
                    }
                }
            }
        }
    }
}

/// Append the turn's assistant message and tool result messages to state.
fn push_turn_messages(
    state: &mut RunState,
    assistant_content: Vec<ContentBlock>,
    usage: Usage,
    tool_results: &[crate::executor::ToolResult],
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

// --- Recovery helpers ---

/// Outcome of applying a recovery strategy.
enum RecoveryOutcome {
    /// The recovery was applied successfully; the loop should retry.
    Retry,
    /// Recovery is exhausted; terminate with this error message.
    GiveUp(String),
}

/// Apply a recovery strategy to the run state.
///
/// - CompactAndRetry: Force-snip the message history, then retry.
/// - ContinueMessage: Append a continuation prompt to messages, retry.
/// - EscalateOutputTokens: Increase effective_max_output_tokens, retry.
/// - GiveUp: Return GiveUp with the error message.
fn apply_recovery_run(
    strategy: &RecoveryStrategy,
    state: &mut RunState,
    effective_max_output_tokens: &mut Option<u32>,
    model: &dyn crate::model::Model,
) -> RecoveryOutcome {
    match strategy {
        RecoveryStrategy::CompactAndRetry => {
            // ponytail: brute-force snip to half the context window; graceful
            // compaction is the pipeline's job — this only fires when the
            // provider still rejects the prompt as too long.
            snip_history(&mut state.messages, model.context_window() / 2);
            RecoveryOutcome::Retry
        }

        RecoveryStrategy::ContinueMessage { attempt: _ } => {
            // Append a continuation prompt as a user message
            state.messages.push(Message::User {
                content: vec![ContentBlock::Text {
                    text: "Please continue from where you left off.".to_string(),
                }],
            });
            RecoveryOutcome::Retry
        }

        RecoveryStrategy::EscalateOutputTokens { max } => {
            // Increase max_output_tokens, capped at model's maximum
            let model_max = model.max_output_tokens() as u32;
            let new_max = if *max > 0 {
                (*max).min(model_max)
            } else {
                // Default: double the current value or use model max
                let current = effective_max_output_tokens.unwrap_or(4096);
                (current * 2).min(model_max)
            };
            *effective_max_output_tokens = Some(new_max);
            RecoveryOutcome::Retry
        }

        RecoveryStrategy::GiveUp { error } => {
            RecoveryOutcome::GiveUp(error.clone())
        }
    }
}

/// Remove oldest non-system messages (preserving the most recent user message)
/// until the chars/4 token estimate fits within `max_tokens`.
fn snip_history(messages: &mut Vec<Message>, max_tokens: usize) {
    while estimate_tokens(messages) > max_tokens {
        let last_user = messages
            .iter()
            .rposition(|m| matches!(m, Message::User { .. }));
        let removable = messages.iter().enumerate().position(|(idx, m)| {
            !matches!(m, Message::System { .. }) && Some(idx) != last_user
        });
        match removable {
            Some(idx) => {
                messages.remove(idx);
            }
            None => break,
        }
    }
}

// --- Helper functions ---

/// Initialize RunState from the given Input.
fn initialize_state(agent: &Agent, input: &Input) -> RunState {
    match input {
        Input::Fresh { prompt } => {
            let mut state = RunState::new(
                Uuid::new_v4().to_string(),
                None,
                agent.max_turns,
            );
            state.trace_id = Uuid::new_v4().to_string();
            state.messages.push(Message::User {
                content: vec![ContentBlock::Text {
                    text: prompt.clone(),
                }],
            });
            state
        }
        Input::Items { messages } => {
            let mut state = RunState::new(
                Uuid::new_v4().to_string(),
                None,
                agent.max_turns,
            );
            state.trace_id = Uuid::new_v4().to_string();
            state.messages = messages.clone();
            state
        }
        Input::Resume { state } => state.clone(),
    }
}

/// Resolve the agent's instructions to a string.
async fn resolve_instructions(agent: &Agent, state: &RunState) -> String {
    let mut instructions = match &agent.instructions {
        Instructions::Static(s) => s.clone(),
        Instructions::Dynamic(f) => {
            let ctx = RunContext {
                state: state.clone(),
            };
            f(&ctx).await
        }
    };

    // Append the current date and time
    let now = chrono::Local::now().to_rfc3339();
    if !instructions.is_empty() {
        instructions.push_str("\n\n");
    }
    instructions.push_str(&format!("Current date and time: {}", now));

    instructions
}

/// Build tool definitions from the agent's registered tools.
fn build_tool_definitions(agent: &Agent) -> Vec<ToolDefinition> {
    agent
        .tools
        .iter()
        .filter(|t| t.is_enabled())
        .map(|tool| ToolDefinition {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters_schema(),
        })
        .collect()
}

/// Consume a model stream, collecting content blocks, stop reason, usage, and tool uses.
///
/// When `tx` is attached, each chunk is forwarded as a `RunEvent::StreamChunk`.
async fn consume_stream(
    model_stream: crate::model::ModelStream,
    tx: Option<&mpsc::Sender<RunEvent>>,
) -> Result<(Vec<ContentBlock>, StopReason, Usage, Vec<ToolUseBlock>), RunError> {
    use futures::pin_mut;

    pin_mut!(model_stream);

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_uses: Vec<ToolUseBlock> = Vec::new();
    let mut current_tool_name: Option<String> = None;
    let mut stop_reason = StopReason::EndTurn;
    let mut usage = Usage::default();

    while let Some(chunk_result) = model_stream.next().await {
        let chunk = chunk_result.map_err(RunError::Model)?;
        if let Some(tx) = tx {
            if tx.send(RunEvent::StreamChunk(chunk.clone())).await.is_err() {
                return Err(RunError::Aborted("stream_dropped".to_string()));
            }
        }
        match chunk {
            StreamChunk::TextDelta { text } => {
                text_parts.push(text);
            }
            StreamChunk::ThinkingDelta { .. } => {
                // Thinking deltas are not included in final content
            }
            StreamChunk::ToolUseStart { name, .. } => {
                current_tool_name = Some(name);
            }
            StreamChunk::ToolUseInputDelta { .. } => {
                // Input arrives fully parsed in ToolUseEnd
            }
            StreamChunk::ToolUseEnd { id, input } => {
                tool_uses.push(ToolUseBlock {
                    id,
                    name: current_tool_name.take().unwrap_or_default(),
                    input,
                });
            }
            StreamChunk::MessageStop {
                stop_reason: sr,
                usage: u,
            } => {
                stop_reason = sr;
                usage = u;
            }
        }
    }

    // Build content blocks
    let mut content: Vec<ContentBlock> = Vec::new();
    let full_text: String = text_parts.join("");
    if !full_text.is_empty() {
        content.push(ContentBlock::Text { text: full_text });
    }
    for tu in &tool_uses {
        content.push(ContentBlock::ToolUse {
            block: tu.clone(),
        });
    }

    Ok((content, stop_reason, usage, tool_uses))
}

/// Execute tool calls using the StreamingToolExecutor.
async fn execute_tools(
    agent: &Agent,
    tool_uses: &[ToolUseBlock],
    config: &RunConfig,
    state: &RunState,
) -> Vec<crate::executor::ToolResult> {
    let mut executor =
        StreamingToolExecutor::new(config.concurrency_limit as usize);

    let ctx = ToolContext {
        session_id: state.session_id.clone().unwrap_or_default(),
        working_dir: std::path::PathBuf::from("."),
    };

    for tu in tool_uses {
        // Find the tool by name in the agent's tool registry
        if let Some(tool) = agent.tools.iter().find(|t| t.name() == tu.name) {
            executor.enqueue(tu.clone(), Arc::clone(tool), ctx.clone());
        } else {
            // Tool not found — enqueue a placeholder that reports NotAvailable.
            executor.enqueue(tu.clone(), Arc::new(NotFoundTool(tu.name.clone())), ctx.clone());
        }
    }

    executor.execute_all().await;
    executor.drain_completed()
}

/// A placeholder tool used when the model requests a tool that isn't registered.
struct NotFoundTool(String);

#[async_trait::async_trait]
impl crate::tool::Tool for NotFoundTool {
    fn name(&self) -> &str {
        &self.0
    }
    fn description(&self) -> &str {
        "Tool not found"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn concurrency(&self, _input: &serde_json::Value) -> crate::tool::Concurrency {
        crate::tool::Concurrency::Safe
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, crate::error::ToolError> {
        Err(crate::error::ToolError::NotAvailable(self.0.clone()))
    }
}

/// Resolve the next step based on model response and tool results.
///
/// This function inspects the model's stop reason, tool calls, permission decisions,
/// agent configuration (output_schema), and the current state to determine what
/// the run loop should do next.
///
/// # Decision Logic
///
/// 1. **ContentFilter** → `Aborted` (content was blocked)
/// 2. **MaxTurns check** → if current_turn + 1 >= max_turns → `MaxTurns`
/// 3. **Permission-based interruption** → if any tool call requires approval → `Interruption`
/// 4. **ToolUse** → `Continue` (tools were called, continue loop)
/// 5. **MaxTokens** → `Recovery` (ContinueMessage or EscalateOutputTokens based on attempt)
/// 6. **EndTurn / StopSequence** → `FinalOutput` (with optional structured output)
#[allow(clippy::too_many_arguments)]
fn resolve_next_step(
    stop_reason: &StopReason,
    assistant_content: &[ContentBlock],
    tool_uses: &[ToolUseBlock],
    state: &RunState,
    max_turns: u32,
    agent: &Agent,
    config: &RunConfig,
    max_tokens_attempts: u32,
) -> NextStep {
    // ContentFilter always aborts immediately regardless of other state
    if *stop_reason == StopReason::ContentFilter {
        return NextStep::Aborted {
            reason: "content_filter".to_string(),
        };
    }

    // Check turn limit (will be incremented after this step)
    if state.current_turn + 1 >= max_turns {
        return NextStep::MaxTurns { count: max_turns };
    }

    // For ToolUse stop reason, check permissions before continuing
    if *stop_reason == StopReason::ToolUse && !tool_uses.is_empty() {
        // Check each tool call against the permission engine
        let mut pending_approvals: Vec<PendingApproval> = Vec::new();

        for tu in tool_uses {
            // Find the tool's approval requirement
            let approval_req = agent
                .tools
                .iter()
                .find(|t| t.name() == tu.name)
                .map(|t| t.approval_requirement())
                .unwrap_or(crate::tool::ApprovalRequirement::Never);

            let decision = config.permissions.check(&tu.name, &approval_req, Some(&tu.input));

            match decision {
                PermissionDecision::NeedsApproval { .. } => {
                    pending_approvals.push(PendingApproval {
                        tool_name: tu.name.clone(),
                        tool_input: tu.input.clone(),
                        request_id: format!("approval-{}", tu.id),
                    });
                }
                PermissionDecision::Deny { message, reason } => {
                    // If a tool is denied, we abort for safety
                    return NextStep::Aborted {
                        reason: format!("permission_denied: {} ({})", message, reason),
                    };
                }
                PermissionDecision::Allow { .. } => {
                    // Tool is allowed, continue
                }
            }
        }

        if !pending_approvals.is_empty() {
            return NextStep::Interruption {
                pending: pending_approvals,
            };
        }

        // All tools allowed — continue
        return NextStep::Continue;
    }

    // ToolUse stop reason but no actual tools (edge case)
    if *stop_reason == StopReason::ToolUse && tool_uses.is_empty() {
        let text = extract_text_from_content(assistant_content);
        return NextStep::FinalOutput {
            text,
            structured: None,
        };
    }

    // MaxTokens → recovery strategy based on attempt count
    if *stop_reason == StopReason::MaxTokens {
        if max_tokens_attempts >= 2 {
            // After 2 attempts of ContinueMessage, escalate to increasing output tokens
            return NextStep::Recovery {
                strategy: RecoveryStrategy::EscalateOutputTokens { max: 8192 },
            };
        }
        return NextStep::Recovery {
            strategy: RecoveryStrategy::ContinueMessage {
                attempt: max_tokens_attempts + 1,
            },
        };
    }

    // EndTurn or StopSequence → produce final output
    let text = extract_text_from_content(assistant_content);

    // If agent has output_schema, try to parse the text as structured JSON
    let structured = if agent.output_schema.is_some() {
        // Attempt to parse the output text as JSON
        serde_json::from_str::<serde_json::Value>(&text).ok()
    } else {
        None
    };

    NextStep::FinalOutput { text, structured }
}

/// Extract text content from assistant content blocks.
fn extract_text_from_content(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Convert a ToolOutput to its string representation.
fn tool_output_to_string(output: &ToolOutput) -> String {
    match output {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Structured(v) => serde_json::to_string(v).unwrap_or_default(),
        ToolOutput::Error(s) => s.clone(),
    }
}

/// Accumulate usage from a turn into the RunState totals.
fn accumulate_usage(state: &mut RunState, usage: &Usage, model: &dyn crate::model::Model) {
    state.total_usage.input_tokens += usage.input_tokens;
    state.total_usage.output_tokens += usage.output_tokens;
    if let Some(cache) = usage.cache_read_tokens {
        let current = state.total_usage.cache_read_tokens.unwrap_or(0);
        state.total_usage.cache_read_tokens = Some(current + cache);
    }

    // Calculate cost for this turn
    let input_cost =
        (usage.input_tokens as f64) * model.input_cost_per_million() / 1_000_000.0;
    let output_cost =
        (usage.output_tokens as f64) * model.output_cost_per_million() / 1_000_000.0;
    state.total_cost_usd += input_cost + output_cost;
}

/// Check input guardrails sequentially in registration order.
///
/// Returns `Some((guardrail_name, reason))` if any guardrail fails, `None` if all pass.
/// Short-circuits at the first failure — subsequent guardrails are not checked.
async fn check_input_guardrails(
    guardrails: &[Arc<dyn InputGuardrail>],
    messages: &[Message],
) -> Option<(String, String)> {
    for guardrail in guardrails {
        let result = guardrail.check(messages).await;
        if !result.passed {
            let reason = result.reason.unwrap_or_else(|| "guardrail check failed".to_string());
            return Some((guardrail.name().to_string(), reason));
        }
    }
    None
}

/// Check output guardrails sequentially in registration order.
///
/// Returns `Some((guardrail_name, reason))` if any guardrail fails, `None` if all pass.
/// Short-circuits at the first failure — subsequent guardrails are not checked.
async fn check_output_guardrails(
    guardrails: &[Arc<dyn OutputGuardrail>],
    output: &str,
    structured: Option<&serde_json::Value>,
) -> Option<(String, String)> {
    for guardrail in guardrails {
        let result = guardrail.check(output, structured).await;
        if !result.passed {
            let reason = result.reason.unwrap_or_else(|| "guardrail check failed".to_string());
            return Some((guardrail.name().to_string(), reason));
        }
    }
    None
}

/// Build a RunResult for a MaxTurns termination.
fn build_result_max_turns(state: &RunState) -> RunResult {
    let output = extract_text_from_last_assistant(&state.messages);
    RunResult {
        output,
        structured: None,
        usage: state.total_usage.clone(),
        cost_usd: state.total_cost_usd,
        turns: state.current_turn,
        state: state.clone(),
    }
}

/// Extract text from the last assistant message in history.
fn extract_text_from_last_assistant(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find_map(|msg| match msg {
            Message::Assistant { content, .. } => {
                Some(extract_text_from_content(content))
            }
            _ => None,
        })
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ModelError;
    use crate::model::{Model, ModelProvider, ModelRequest, ModelResponse, ModelStream};
    use crate::stream::{StopReason, StreamChunk};
    use crate::tool::{Concurrency, Tool, ToolContext, ToolOutput};
    use async_trait::async_trait;
    use serde_json::json;

    /// A mock model that returns a single text response.
    struct MockModel {
        response_text: String,
    }

    #[async_trait]
    impl Model for MockModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            let text = self.response_text.clone();
            let chunks = vec![
                Ok(StreamChunk::TextDelta { text }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "mock-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    /// A mock model that invokes a tool.
    struct ToolCallingModel;

    #[async_trait]
    impl Model for ToolCallingModel {
        async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
            // If there are tool results in messages, respond with final text
            let has_tool_result = request.messages.iter().any(|m| {
                matches!(m, Message::ToolResult { .. })
            });

            if has_tool_result {
                let chunks = vec![
                    Ok(StreamChunk::TextDelta {
                        text: "Tool executed successfully.".to_string(),
                    }),
                    Ok(StreamChunk::MessageStop {
                        stop_reason: StopReason::EndTurn,
                        usage: Usage {
                            input_tokens: 20,
                            output_tokens: 10,
                            cache_read_tokens: None,
                        },
                    }),
                ];
                return Ok(Box::pin(futures::stream::iter(chunks)));
            }

            // First call: invoke a tool
            let chunks = vec![
                Ok(StreamChunk::ToolUseStart {
                    id: "tool_001".to_string(),
                    name: "echo".to_string(),
                }),
                Ok(StreamChunk::ToolUseInputDelta {
                    id: "tool_001".to_string(),
                    delta: r#"{"text":"hello"}"#.to_string(),
                }),
                Ok(StreamChunk::ToolUseEnd {
                    id: "tool_001".to_string(),
                    input: json!({"text": "hello"}),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 15,
                        output_tokens: 8,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "tool-calling-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    /// A mock provider that resolves to a specific model.
    struct MockProvider {
        model: Arc<dyn Model>,
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn resolve(&self, _model_name: &str) -> Result<Arc<dyn Model>, ModelError> {
            Ok(Arc::clone(&self.model))
        }
        fn available_models(&self) -> Vec<String> {
            vec!["mock-model".to_string()]
        }
    }

    /// A simple echo tool for testing.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "Echoes input text" }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, crate::error::ToolError> {
            let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Ok(ToolOutput::Text(text.to_string()))
        }
    }

    fn make_provider(model: Arc<dyn Model>) -> Arc<dyn ModelProvider> {
        Arc::new(MockProvider { model })
    }

    #[tokio::test]
    async fn test_run_simple_text_response() {
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Hello, world!".to_string(),
        });
        let provider = make_provider(model);
        let agent = Agent::builder("test-agent")
            .instructions(Instructions::Static("Be helpful.".into()))
            .build();
        let config = RunConfig::builder(provider, "mock-model").build();
        let input = Input::Fresh {
            prompt: "Hi there".to_string(),
        };

        let result = run(&agent, input, &config).await.unwrap();
        assert_eq!(result.output, "Hello, world!");
        assert_eq!(result.turns, 1);
        assert!(result.cost_usd > 0.0);
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.usage.output_tokens, 5);
    }

    #[tokio::test]
    async fn test_run_with_tool_calls() {
        let model: Arc<dyn Model> = Arc::new(ToolCallingModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(EchoTool);
        let agent = Agent::builder("tool-agent")
            .instructions(Instructions::Static("Use tools.".into()))
            .tool(tool)
            .build();
        let config = RunConfig::builder(provider, "tool-model").build();
        let input = Input::Fresh {
            prompt: "Echo hello".to_string(),
        };

        let result = run(&agent, input, &config).await.unwrap();
        assert_eq!(result.output, "Tool executed successfully.");
        assert_eq!(result.turns, 2); // turn 1: tool call, turn 2: final response
    }

    #[tokio::test]
    async fn test_run_max_turns_reached() {
        let model: Arc<dyn Model> = Arc::new(ToolCallingModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(EchoTool);
        let agent = Agent::builder("limited-agent")
            .tool(tool)
            .max_turns(1)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "Do something".to_string(),
        };

        let result = run(&agent, input, &config).await.unwrap();
        // Should terminate after max turns without error
        assert_eq!(result.turns, 0); // Never got past turn 0 since max_turns=1
    }

    #[tokio::test]
    async fn test_run_with_items_input() {
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Response to items.".to_string(),
        });
        let provider = make_provider(model);
        let agent = Agent::builder("items-agent").build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Items {
            messages: vec![
                Message::System {
                    content: "System prompt.".to_string(),
                },
                Message::User {
                    content: vec![ContentBlock::Text {
                        text: "Question?".to_string(),
                    }],
                },
            ],
        };

        let result = run(&agent, input, &config).await.unwrap();
        assert_eq!(result.output, "Response to items.");
    }

    #[tokio::test]
    async fn test_run_stream_emits_events() {
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Streamed output.".to_string(),
        });
        let provider = make_provider(model);
        let agent = Agent::builder("stream-agent").build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "Stream test".to_string(),
        };

        let stream = run_stream(&agent, input, &config);
        let events: Vec<RunEvent> = stream.collect().await;

        // Should have at least one event (terminal)
        assert!(!events.is_empty());

        // Last event should be terminal
        let last = events.last().unwrap();
        assert!(matches!(last, RunEvent::AgentEnd { .. }));
    }

    #[test]
    fn test_initialize_state_fresh() {
        let agent = Agent::builder("test").build();
        let input = Input::Fresh {
            prompt: "Hello".to_string(),
        };
        let state = initialize_state(&agent, &input);
        assert!(!state.run_id.is_empty());
        assert!(!state.trace_id.is_empty());
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.current_turn, 0);
    }

    #[test]
    fn test_initialize_state_items() {
        let agent = Agent::builder("test").build();
        let messages = vec![
            Message::System {
                content: "sys".to_string(),
            },
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "q".to_string(),
                }],
            },
        ];
        let input = Input::Items {
            messages: messages.clone(),
        };
        let state = initialize_state(&agent, &input);
        assert_eq!(state.messages.len(), 2);
    }

    #[test]
    fn test_initialize_state_resume() {
        let agent = Agent::builder("test").build();
        let original = RunState::new("run-42".into(), Some("sess-1".into()), Some(10));
        let input = Input::Resume {
            state: original.clone(),
        };
        let state = initialize_state(&agent, &input);
        assert_eq!(state.run_id, "run-42");
        assert_eq!(state.session_id, Some("sess-1".to_string()));
    }

    #[test]
    fn test_extract_text_from_content() {
        let content = vec![
            ContentBlock::Text {
                text: "Hello ".to_string(),
            },
            ContentBlock::ToolUse {
                block: ToolUseBlock {
                    id: "t1".to_string(),
                    name: "tool".to_string(),
                    input: json!({}),
                },
            },
            ContentBlock::Text {
                text: "world".to_string(),
            },
        ];
        assert_eq!(extract_text_from_content(&content), "Hello world");
    }

    #[test]
    fn test_tool_output_to_string() {
        assert_eq!(
            tool_output_to_string(&ToolOutput::Text("hi".into())),
            "hi"
        );
        assert_eq!(
            tool_output_to_string(&ToolOutput::Structured(json!({"k": "v"}))),
            r#"{"k":"v"}"#
        );
        assert_eq!(
            tool_output_to_string(&ToolOutput::Error("oops".into())),
            "oops"
        );
    }

    #[test]
    fn test_build_tool_definitions() {
        let tool: Arc<dyn Tool> = Arc::new(EchoTool);
        let agent = Agent::builder("test").tool(tool).build();
        let defs = build_tool_definitions(&agent);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
        assert_eq!(defs[0].description, "Echoes input text");
    }


    // --- Budget enforcement tests ---

    /// A mock model that returns a fixed usage per call, useful for budget tests.
    struct HighCostModel {
        input_tokens: u64,
        output_tokens: u64,
    }

    #[async_trait]
    impl Model for HighCostModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            let input_tokens = self.input_tokens;
            let output_tokens = self.output_tokens;
            let chunks = vec![
                Ok(StreamChunk::TextDelta {
                    text: "response".to_string(),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens,
                        output_tokens,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "high-cost-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        // $10/M input, $30/M output — expensive model for easy budget trigger
        fn input_cost_per_million(&self) -> f64 { 10.0 }
        fn output_cost_per_million(&self) -> f64 { 30.0 }
    }

    /// A mock model that always calls a tool (looping), used to test budget enforcement
    /// across multiple turns.
    struct LoopingToolModel {
        /// Number of turns before emitting final output
        turns_before_done: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl Model for LoopingToolModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            let remaining = self.turns_before_done.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);

            if remaining <= 1 {
                // Final turn: emit text
                let chunks = vec![
                    Ok(StreamChunk::TextDelta {
                        text: "done".to_string(),
                    }),
                    Ok(StreamChunk::MessageStop {
                        stop_reason: StopReason::EndTurn,
                        usage: Usage {
                            input_tokens: 1_000_000,
                            output_tokens: 1_000_000,
                            cache_read_tokens: None,
                        },
                    }),
                ];
                return Ok(Box::pin(futures::stream::iter(chunks)));
            }

            // Intermediate turn: call a tool with high usage
            let chunks = vec![
                Ok(StreamChunk::ToolUseStart {
                    id: format!("tool_{}", remaining),
                    name: "echo".to_string(),
                }),
                Ok(StreamChunk::ToolUseEnd {
                    id: format!("tool_{}", remaining),
                    input: json!({"text": "hi"}),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 1_000_000,
                        output_tokens: 1_000_000,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "looping-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        // $10/M input, $30/M output
        fn input_cost_per_million(&self) -> f64 { 10.0 }
        fn output_cost_per_million(&self) -> f64 { 30.0 }
    }

    #[tokio::test]
    async fn test_budget_enforcement_triggers_when_exceeded() {
        // Model produces 1M input + 1M output tokens per call
        // At $10/M input + $30/M output = $40 per turn
        // Budget of $0.01 should be exceeded on first turn
        let model: Arc<dyn Model> = Arc::new(HighCostModel {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        });
        let provider = make_provider(model);
        let agent = Agent::builder("budget-agent")
            .instructions(Instructions::Static("Be helpful.".into()))
            .build();
        let config = RunConfig::builder(provider, "mock")
            .budget_usd(0.01) // $0.01 budget
            .build();
        let input = Input::Fresh {
            prompt: "Hi".to_string(),
        };

        let result = run(&agent, input, &config).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RunError::Aborted(reason) => assert_eq!(reason, "budget_exceeded"),
            other => panic!("Expected Aborted(budget_exceeded), got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_no_budget_enforcement_when_none() {
        // Same high-cost model but no budget configured — should succeed
        let model: Arc<dyn Model> = Arc::new(HighCostModel {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        });
        let provider = make_provider(model);
        let agent = Agent::builder("no-budget-agent")
            .instructions(Instructions::Static("Be helpful.".into()))
            .build();
        // No budget_usd set
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "Hi".to_string(),
        };

        let result = run(&agent, input, &config).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.output, "response");
        // Cost should be accumulated but no abort
        assert!(result.cost_usd > 0.0);
    }

    #[tokio::test]
    async fn test_budget_not_exceeded_stays_within() {
        // Low-cost response that stays within budget
        let model: Arc<dyn Model> = Arc::new(HighCostModel {
            input_tokens: 10,
            output_tokens: 5,
        });
        let provider = make_provider(model);
        let agent = Agent::builder("low-cost-agent")
            .instructions(Instructions::Static("Be helpful.".into()))
            .build();
        let config = RunConfig::builder(provider, "mock")
            .budget_usd(1.0) // generous $1 budget
            .build();
        let input = Input::Fresh {
            prompt: "Hi".to_string(),
        };

        let result = run(&agent, input, &config).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.output, "response");
        // Cost: 10 * 10/1M + 5 * 30/1M = 0.0001 + 0.00015 = 0.00025
        assert!(result.cost_usd < 1.0);
    }

    #[tokio::test]
    async fn test_cost_accumulates_over_multiple_turns() {
        // Use a looping model that calls tools for multiple turns
        let model: Arc<dyn Model> = Arc::new(LoopingToolModel {
            turns_before_done: std::sync::atomic::AtomicU32::new(3),
        });
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(EchoTool);
        let agent = Agent::builder("multi-turn-agent")
            .instructions(Instructions::Static("Use tools.".into()))
            .tool(tool)
            .build();
        // Budget of $50 — enough for a couple turns at $40/turn but not all 3
        let config = RunConfig::builder(provider, "mock")
            .budget_usd(50.0)
            .max_turns(10)
            .build();
        let input = Input::Fresh {
            prompt: "Do work".to_string(),
        };

        let result = run(&agent, input, &config).await;
        // Should hit budget_exceeded after second turn
        // Turn 1: $40 (within $50 budget)
        // Turn 2: $40 more → total $80 (exceeds $50 budget)
        assert!(result.is_err());
        match result.unwrap_err() {
            RunError::Aborted(reason) => assert_eq!(reason, "budget_exceeded"),
            other => panic!("Expected Aborted(budget_exceeded), got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_budget_enforcement_in_stream() {
        // Test that streaming mode also enforces budget
        let model: Arc<dyn Model> = Arc::new(HighCostModel {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        });
        let provider = make_provider(model);
        let agent = Agent::builder("stream-budget-agent")
            .instructions(Instructions::Static("Be helpful.".into()))
            .build();
        let config = RunConfig::builder(provider, "mock")
            .budget_usd(0.01)
            .build();
        let input = Input::Fresh {
            prompt: "Hi".to_string(),
        };

        let stream = run_stream(&agent, input, &config);
        let events: Vec<RunEvent> = stream.collect().await;

        // Should have an Aborted event with budget_exceeded
        let has_budget_abort = events.iter().any(|e| matches!(
            e,
            RunEvent::Aborted { reason } if reason == "budget_exceeded"
        ));
        assert!(has_budget_abort, "Expected Aborted event with budget_exceeded, got: {:?}", events);
    }

    #[test]
    fn test_accumulate_usage_with_zero_usage() {
        // Ensure that zero/default usage doesn't cause issues
        let mut state = RunState::new("run-1".to_string(), None, None);
        let usage = Usage::default(); // all zeros
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "x".to_string(),
        });
        accumulate_usage(&mut state, &usage, model.as_ref());
        assert_eq!(state.total_usage.input_tokens, 0);
        assert_eq!(state.total_usage.output_tokens, 0);
        assert_eq!(state.total_cost_usd, 0.0);
    }

    #[test]
    fn test_accumulate_usage_sums_correctly() {
        let mut state = RunState::new("run-1".to_string(), None, None);
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "x".to_string(),
        });

        // Turn 1
        let usage1 = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: Some(20),
        };
        accumulate_usage(&mut state, &usage1, model.as_ref());
        assert_eq!(state.total_usage.input_tokens, 100);
        assert_eq!(state.total_usage.output_tokens, 50);
        assert_eq!(state.total_usage.cache_read_tokens, Some(20));

        // Turn 2
        let usage2 = Usage {
            input_tokens: 200,
            output_tokens: 100,
            cache_read_tokens: Some(30),
        };
        accumulate_usage(&mut state, &usage2, model.as_ref());
        assert_eq!(state.total_usage.input_tokens, 300);
        assert_eq!(state.total_usage.output_tokens, 150);
        assert_eq!(state.total_usage.cache_read_tokens, Some(50));

        // Cost check: MockModel has input_cost_per_million=3.0, output_cost_per_million=15.0
        // Turn 1: 100*3/1M + 50*15/1M = 0.0003 + 0.00075 = 0.00105
        // Turn 2: 200*3/1M + 100*15/1M = 0.0006 + 0.0015 = 0.0021
        // Total: 0.00315
        let expected_cost = (300.0 * 3.0 + 150.0 * 15.0) / 1_000_000.0;
        assert!((state.total_cost_usd - expected_cost).abs() < 1e-10);
    }

    // --- Guardrail invocation tests ---

    /// A mock input guardrail that always fails.
    struct FailingInputGuardrail {
        guardrail_name: String,
        fail_reason: String,
    }

    #[async_trait]
    impl crate::guardrail::InputGuardrail for FailingInputGuardrail {
        fn name(&self) -> &str {
            &self.guardrail_name
        }
        async fn check(&self, _input: &[Message]) -> crate::guardrail::GuardrailResult {
            crate::guardrail::GuardrailResult::fail(&self.fail_reason)
        }
    }

    /// A mock input guardrail that always passes.
    struct PassingInputGuardrail {
        guardrail_name: String,
    }

    #[async_trait]
    impl crate::guardrail::InputGuardrail for PassingInputGuardrail {
        fn name(&self) -> &str {
            &self.guardrail_name
        }
        async fn check(&self, _input: &[Message]) -> crate::guardrail::GuardrailResult {
            crate::guardrail::GuardrailResult::pass()
        }
    }

    /// A mock output guardrail that always fails.
    struct FailingOutputGuardrail {
        guardrail_name: String,
        fail_reason: String,
    }

    #[async_trait]
    impl crate::guardrail::OutputGuardrail for FailingOutputGuardrail {
        fn name(&self) -> &str {
            &self.guardrail_name
        }
        async fn check(
            &self,
            _output: &str,
            _structured: Option<&serde_json::Value>,
        ) -> crate::guardrail::GuardrailResult {
            crate::guardrail::GuardrailResult::fail(&self.fail_reason)
        }
    }

    /// A mock output guardrail that always passes.
    struct PassingOutputGuardrail {
        guardrail_name: String,
    }

    #[async_trait]
    impl crate::guardrail::OutputGuardrail for PassingOutputGuardrail {
        fn name(&self) -> &str {
            &self.guardrail_name
        }
        async fn check(
            &self,
            _output: &str,
            _structured: Option<&serde_json::Value>,
        ) -> crate::guardrail::GuardrailResult {
            crate::guardrail::GuardrailResult::pass()
        }
    }

    /// A model that responds with tool use first, then text on second call.
    /// Used to verify input guardrails are only checked on first turn.
    struct MultiTurnModel;

    #[async_trait]
    impl Model for MultiTurnModel {
        async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
            // If there are tool results, respond with final text
            let has_tool_result = request.messages.iter().any(|m| {
                matches!(m, Message::ToolResult { .. })
            });

            if has_tool_result {
                let chunks = vec![
                    Ok(StreamChunk::TextDelta {
                        text: "Final answer.".to_string(),
                    }),
                    Ok(StreamChunk::MessageStop {
                        stop_reason: StopReason::EndTurn,
                        usage: Usage {
                            input_tokens: 20,
                            output_tokens: 10,
                            cache_read_tokens: None,
                        },
                    }),
                ];
                return Ok(Box::pin(futures::stream::iter(chunks)));
            }

            // First call: invoke a tool
            let chunks = vec![
                Ok(StreamChunk::ToolUseStart {
                    id: "tool_001".to_string(),
                    name: "echo".to_string(),
                }),
                Ok(StreamChunk::ToolUseInputDelta {
                    id: "tool_001".to_string(),
                    delta: r#"{"text":"hi"}"#.to_string(),
                }),
                Ok(StreamChunk::ToolUseEnd {
                    id: "tool_001".to_string(),
                    input: json!({"text": "hi"}),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 15,
                        output_tokens: 8,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "multi-turn-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    #[tokio::test]
    async fn test_input_guardrail_trips_on_first_turn() {
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Hello!".to_string(),
        });
        let provider = make_provider(model);
        let guardrail: Arc<dyn crate::guardrail::InputGuardrail> =
            Arc::new(FailingInputGuardrail {
                guardrail_name: "content_check".to_string(),
                fail_reason: "banned content detected".to_string(),
            });

        let agent = Agent::builder("guarded-agent")
            .input_guardrail(guardrail)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "bad input".to_string(),
        };

        let result = run(&agent, input, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, RunError::Guardrail(_)));
        let msg = format!("{}", err);
        assert!(msg.contains("content_check"));
        assert!(msg.contains("banned content detected"));
    }

    #[tokio::test]
    async fn test_input_guardrail_passes_allows_run() {
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "All good!".to_string(),
        });
        let provider = make_provider(model);
        let guardrail: Arc<dyn crate::guardrail::InputGuardrail> =
            Arc::new(PassingInputGuardrail {
                guardrail_name: "safety_check".to_string(),
            });

        let agent = Agent::builder("safe-agent")
            .input_guardrail(guardrail)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "good input".to_string(),
        };

        let result = run(&agent, input, &config).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().output, "All good!");
    }

    #[tokio::test]
    async fn test_output_guardrail_trips_on_final_output() {
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Harmful output.".to_string(),
        });
        let provider = make_provider(model);
        let guardrail: Arc<dyn crate::guardrail::OutputGuardrail> =
            Arc::new(FailingOutputGuardrail {
                guardrail_name: "output_filter".to_string(),
                fail_reason: "output contains harmful content".to_string(),
            });

        let agent = Agent::builder("output-guarded-agent")
            .output_guardrail(guardrail)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "Hello".to_string(),
        };

        let result = run(&agent, input, &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, RunError::Guardrail(_)));
        let msg = format!("{}", err);
        assert!(msg.contains("output_filter"));
        assert!(msg.contains("output contains harmful content"));
    }

    #[tokio::test]
    async fn test_output_guardrail_passes_allows_delivery() {
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Safe output.".to_string(),
        });
        let provider = make_provider(model);
        let guardrail: Arc<dyn crate::guardrail::OutputGuardrail> =
            Arc::new(PassingOutputGuardrail {
                guardrail_name: "output_check".to_string(),
            });

        let agent = Agent::builder("pass-output-agent")
            .output_guardrail(guardrail)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "Hello".to_string(),
        };

        let result = run(&agent, input, &config).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().output, "Safe output.");
    }

    #[tokio::test]
    async fn test_input_guardrails_short_circuit_at_first_failure() {
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Hello!".to_string(),
        });
        let provider = make_provider(model);

        // First guardrail passes, second fails, third should never be checked
        let g1: Arc<dyn crate::guardrail::InputGuardrail> =
            Arc::new(PassingInputGuardrail {
                guardrail_name: "first".to_string(),
            });
        let g2: Arc<dyn crate::guardrail::InputGuardrail> =
            Arc::new(FailingInputGuardrail {
                guardrail_name: "second".to_string(),
                fail_reason: "blocked".to_string(),
            });
        let g3: Arc<dyn crate::guardrail::InputGuardrail> =
            Arc::new(PassingInputGuardrail {
                guardrail_name: "third".to_string(),
            });

        let agent = Agent::builder("multi-guard-agent")
            .input_guardrail(g1)
            .input_guardrail(g2)
            .input_guardrail(g3)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "test".to_string(),
        };

        let result = run(&agent, input, &config).await;
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        // Should be the second guardrail that tripped
        assert!(msg.contains("second"));
        assert!(msg.contains("blocked"));
    }

    #[tokio::test]
    async fn test_input_guardrails_not_invoked_on_subsequent_turns() {
        // Use a multi-turn model that uses tools on first turn, then responds
        let model: Arc<dyn Model> = Arc::new(MultiTurnModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(EchoTool);

        // This input guardrail always passes — it should only be called on turn 0
        let guardrail: Arc<dyn crate::guardrail::InputGuardrail> =
            Arc::new(PassingInputGuardrail {
                guardrail_name: "first_turn_only".to_string(),
            });

        let agent = Agent::builder("multi-turn-agent")
            .tool(tool)
            .input_guardrail(guardrail)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "Do something".to_string(),
        };

        // Should complete successfully (input guardrails pass on first turn,
        // and are not re-invoked on subsequent turns)
        let result = run(&agent, input, &config).await;
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.output, "Final answer.");
        assert_eq!(res.turns, 2); // Turn 1: tool call, Turn 2: final answer
    }

    #[tokio::test]
    async fn test_guardrail_tripped_in_stream() {
        // Test that streaming mode emits GuardrailTripped event
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Hello!".to_string(),
        });
        let provider = make_provider(model);
        let guardrail: Arc<dyn crate::guardrail::InputGuardrail> =
            Arc::new(FailingInputGuardrail {
                guardrail_name: "stream_guard".to_string(),
                fail_reason: "input blocked".to_string(),
            });

        let agent = Agent::builder("stream-guarded-agent")
            .input_guardrail(guardrail)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "bad input".to_string(),
        };

        let stream = run_stream(&agent, input, &config);
        let events: Vec<RunEvent> = stream.collect().await;

        // Should have a GuardrailTripped event
        let has_guardrail_tripped = events.iter().any(|e| matches!(
            e,
            RunEvent::GuardrailTripped { name, reason }
            if name == "stream_guard" && reason == "input blocked"
        ));
        assert!(
            has_guardrail_tripped,
            "Expected GuardrailTripped event, got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_output_guardrail_tripped_in_stream() {
        // Test that streaming mode emits GuardrailTripped for output guardrails
        let model: Arc<dyn Model> = Arc::new(MockModel {
            response_text: "Bad output.".to_string(),
        });
        let provider = make_provider(model);
        let guardrail: Arc<dyn crate::guardrail::OutputGuardrail> =
            Arc::new(FailingOutputGuardrail {
                guardrail_name: "output_stream_guard".to_string(),
                fail_reason: "output blocked".to_string(),
            });

        let agent = Agent::builder("output-stream-guarded-agent")
            .output_guardrail(guardrail)
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: "Hello".to_string(),
        };

        let stream = run_stream(&agent, input, &config);
        let events: Vec<RunEvent> = stream.collect().await;

        // Should have a GuardrailTripped event for the output guardrail
        let has_guardrail_tripped = events.iter().any(|e| matches!(
            e,
            RunEvent::GuardrailTripped { name, reason }
            if name == "output_stream_guard" && reason == "output blocked"
        ));
        assert!(
            has_guardrail_tripped,
            "Expected GuardrailTripped event for output guardrail, got: {:?}",
            events
        );
    }

    // --- Tests for enhanced resolve_next_step ---

    #[test]
    fn test_resolve_next_step_content_filter_aborts() {
        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let state = RunState::new("r".into(), None, Some(25));
        let content = vec![ContentBlock::Text {
            text: "some text".to_string(),
        }];

        let result = resolve_next_step(
            &StopReason::ContentFilter,
            &content,
            &[],
            &state,
            25,
            &agent,
            &config,
            0,
        );

        assert_eq!(
            result,
            NextStep::Aborted {
                reason: "content_filter".to_string()
            }
        );
    }

    #[test]
    fn test_resolve_next_step_max_turns_check() {
        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let mut state = RunState::new("r".into(), None, Some(5));
        state.current_turn = 4; // current_turn + 1 = 5 >= max_turns=5

        let tool_uses = vec![ToolUseBlock {
            id: "t1".to_string(),
            name: "echo".to_string(),
            input: json!({}),
        }];

        let result = resolve_next_step(
            &StopReason::ToolUse,
            &[],
            &tool_uses,
            &state,
            5,
            &agent,
            &config,
            0,
        );

        assert_eq!(result, NextStep::MaxTurns { count: 5 });
    }

    #[test]
    fn test_resolve_next_step_tool_use_continue() {
        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        // Bypass mode: all tools are allowed
        let config = RunConfig::builder(provider, "mock").build();
        let state = RunState::new("r".into(), None, Some(25));

        let tool_uses = vec![ToolUseBlock {
            id: "t1".to_string(),
            name: "echo".to_string(),
            input: json!({"text": "hi"}),
        }];

        let result = resolve_next_step(
            &StopReason::ToolUse,
            &[],
            &tool_uses,
            &state,
            25,
            &agent,
            &config,
            0,
        );

        assert_eq!(
            result,
            NextStep::Continue
        );
    }

    #[test]
    fn test_resolve_next_step_permission_interruption() {
        use crate::permission::{PermissionEngine, PermissionMode};
        use crate::tool::ApprovalRequirement;

        // Tool that always requires approval
        struct ApprovalTool;
        #[async_trait]
        impl Tool for ApprovalTool {
            fn name(&self) -> &str { "dangerous_tool" }
            fn description(&self) -> &str { "needs approval" }
            fn parameters_schema(&self) -> serde_json::Value { json!({"type": "object"}) }
            fn concurrency(&self, _input: &serde_json::Value) -> Concurrency { Concurrency::Safe }
            async fn execute(
                &self,
                _input: serde_json::Value,
                _ctx: &ToolContext,
            ) -> Result<ToolOutput, crate::error::ToolError> {
                Ok(ToolOutput::Text("done".to_string()))
            }
            fn approval_requirement(&self) -> ApprovalRequirement {
                ApprovalRequirement::Always
            }
        }

        let tool: Arc<dyn Tool> = Arc::new(ApprovalTool);
        let agent = Agent::builder("test").tool(tool).build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        // Normal mode: approval checks are active
        let permissions = PermissionEngine::new(PermissionMode::Normal);
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .build();
        let state = RunState::new("r".into(), None, Some(25));

        let tool_uses = vec![ToolUseBlock {
            id: "t1".to_string(),
            name: "dangerous_tool".to_string(),
            input: json!({"action": "delete"}),
        }];

        let result = resolve_next_step(
            &StopReason::ToolUse,
            &[],
            &tool_uses,
            &state,
            25,
            &agent,
            &config,
            0,
        );

        match result {
            NextStep::Interruption { pending } => {
                assert_eq!(pending.len(), 1);
                assert_eq!(pending[0].tool_name, "dangerous_tool");
                assert_eq!(pending[0].tool_input, json!({"action": "delete"}));
            }
            other => panic!("Expected Interruption, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_next_step_permission_deny_aborts() {
        use crate::permission::{PermissionEngine, PermissionMode};

        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        // Normal mode with a static deny for the tool
        let permissions = PermissionEngine::new(PermissionMode::Normal)
            .with_static_deny(vec!["blocked_tool".to_string()]);
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .build();
        let state = RunState::new("r".into(), None, Some(25));

        let tool_uses = vec![ToolUseBlock {
            id: "t1".to_string(),
            name: "blocked_tool".to_string(),
            input: json!({}),
        }];

        let result = resolve_next_step(
            &StopReason::ToolUse,
            &[],
            &tool_uses,
            &state,
            25,
            &agent,
            &config,
            0,
        );

        match result {
            NextStep::Aborted { reason } => {
                assert!(reason.contains("permission_denied"));
                assert!(reason.contains("blocked_tool"));
            }
            other => panic!("Expected Aborted, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_next_step_max_tokens_recovery_continue_message() {
        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let state = RunState::new("r".into(), None, Some(25));

        let result = resolve_next_step(
            &StopReason::MaxTokens,
            &[ContentBlock::Text {
                text: "partial...".to_string(),
            }],
            &[],
            &state,
            25,
            &agent,
            &config,
            0, // first attempt
        );

        assert_eq!(
            result,
            NextStep::Recovery {
                strategy: RecoveryStrategy::ContinueMessage { attempt: 1 }
            }
        );
    }

    #[test]
    fn test_resolve_next_step_max_tokens_recovery_escalate() {
        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let state = RunState::new("r".into(), None, Some(25));

        // After 2 attempts, should escalate to EscalateOutputTokens
        let result = resolve_next_step(
            &StopReason::MaxTokens,
            &[ContentBlock::Text {
                text: "partial...".to_string(),
            }],
            &[],
            &state,
            25,
            &agent,
            &config,
            2, // attempt >= 2 → escalate
        );

        assert_eq!(
            result,
            NextStep::Recovery {
                strategy: RecoveryStrategy::EscalateOutputTokens { max: 8192 }
            }
        );
    }

    #[test]
    fn test_resolve_next_step_end_turn_final_output() {
        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let state = RunState::new("r".into(), None, Some(25));
        let content = vec![ContentBlock::Text {
            text: "Final answer".to_string(),
        }];

        let result = resolve_next_step(
            &StopReason::EndTurn,
            &content,
            &[],
            &state,
            25,
            &agent,
            &config,
            0,
        );

        assert_eq!(
            result,
            NextStep::FinalOutput {
                text: "Final answer".to_string(),
                structured: None
            }
        );
    }

    #[test]
    fn test_resolve_next_step_structured_output() {
        let agent = Agent::builder("test")
            .output_schema(json!({"type": "object", "properties": {"answer": {"type": "string"}}}))
            .build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let state = RunState::new("r".into(), None, Some(25));
        let content = vec![ContentBlock::Text {
            text: r#"{"answer": "42"}"#.to_string(),
        }];

        let result = resolve_next_step(
            &StopReason::EndTurn,
            &content,
            &[],
            &state,
            25,
            &agent,
            &config,
            0,
        );

        match result {
            NextStep::FinalOutput { text, structured } => {
                assert_eq!(text, r#"{"answer": "42"}"#);
                assert_eq!(structured, Some(json!({"answer": "42"})));
            }
            other => panic!("Expected FinalOutput, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_next_step_structured_output_invalid_json() {
        // When output_schema is set but text isn't valid JSON, structured should be None
        let agent = Agent::builder("test")
            .output_schema(json!({"type": "object"}))
            .build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let state = RunState::new("r".into(), None, Some(25));
        let content = vec![ContentBlock::Text {
            text: "This is not JSON".to_string(),
        }];

        let result = resolve_next_step(
            &StopReason::EndTurn,
            &content,
            &[],
            &state,
            25,
            &agent,
            &config,
            0,
        );

        match result {
            NextStep::FinalOutput { text, structured } => {
                assert_eq!(text, "This is not JSON");
                assert_eq!(structured, None);
            }
            other => panic!("Expected FinalOutput, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_next_step_stop_sequence_final_output() {
        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let state = RunState::new("r".into(), None, Some(25));
        let content = vec![ContentBlock::Text {
            text: "Stopped at sequence".to_string(),
        }];

        let result = resolve_next_step(
            &StopReason::StopSequence,
            &content,
            &[],
            &state,
            25,
            &agent,
            &config,
            0,
        );

        assert_eq!(
            result,
            NextStep::FinalOutput {
                text: "Stopped at sequence".to_string(),
                structured: None
            }
        );
    }

    #[test]
    fn test_resolve_next_step_content_filter_takes_priority_over_max_turns() {
        // ContentFilter should abort even if we're at max turns
        let agent = Agent::builder("test").build();
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider {
            model: Arc::new(MockModel {
                response_text: "".to_string(),
            }),
        });
        let config = RunConfig::builder(provider, "mock").build();
        let mut state = RunState::new("r".into(), None, Some(1));
        state.current_turn = 0; // at max turns boundary

        let result = resolve_next_step(
            &StopReason::ContentFilter,
            &[],
            &[],
            &state,
            1,
            &agent,
            &config,
            0,
        );

        // ContentFilter check comes before MaxTurns
        assert_eq!(
            result,
            NextStep::Aborted {
                reason: "content_filter".to_string()
            }
        );
    }

    // --- Recovery system integration tests ---

    /// A model that fails with PromptTooLong on first call, then succeeds.
    struct PromptTooLongThenSuccessModel {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl Model for PromptTooLongThenSuccessModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            let count = self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                return Err(ModelError::PromptTooLong { tokens: 200000 });
            }
            // Second call succeeds
            let chunks = vec![
                Ok(StreamChunk::TextDelta { text: "recovered!".to_string() }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage { input_tokens: 10, output_tokens: 5, cache_read_tokens: None },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "prompt-too-long-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    #[tokio::test]
    async fn test_recovery_prompt_too_long_compacts_and_retries() {
        let model: Arc<dyn Model> = Arc::new(PromptTooLongThenSuccessModel {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let provider = make_provider(model);
        let agent = Agent::builder("recovery-agent").build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh { prompt: "test".to_string() };

        let result = run(&agent, input, &config).await.unwrap();
        assert_eq!(result.output, "recovered!");
    }

    /// A model that always fails with PromptTooLong, triggering GiveUp after 3 attempts.
    struct AlwaysPromptTooLongModel;

    #[async_trait]
    impl Model for AlwaysPromptTooLongModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            Err(ModelError::PromptTooLong { tokens: 200000 })
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "always-too-long" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    #[tokio::test]
    async fn test_recovery_exhausted_gives_up_after_max_attempts() {
        let model: Arc<dyn Model> = Arc::new(AlwaysPromptTooLongModel);
        let provider = make_provider(model);
        let agent = Agent::builder("exhaust-agent").build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh { prompt: "test".to_string() };

        let result = run(&agent, input, &config).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            RunError::RecoveryExhausted(attempts) => {
                // Should have attempted 4 times (3 real + 1 that triggered GiveUp)
                assert!(attempts >= 3);
            }
            other => panic!("Expected RecoveryExhausted, got {:?}", other),
        }
    }

    /// A model that returns MaxOutputTokens on first call, then succeeds
    /// when given a continuation prompt.
    struct MaxOutputThenSuccessModel {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl Model for MaxOutputThenSuccessModel {
        async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
            let count = self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                // First call: stream some text then error mid-stream with MaxOutputTokens
                // Actually, MaxOutputTokens comes as a stop_reason=MaxTokens in the stream.
                // So let's return a stream that ends with MaxTokens stop reason.
                let chunks = vec![
                    Ok(StreamChunk::TextDelta { text: "partial output".to_string() }),
                    Ok(StreamChunk::MessageStop {
                        stop_reason: StopReason::MaxTokens,
                        usage: Usage { input_tokens: 10, output_tokens: 4096, cache_read_tokens: None },
                    }),
                ];
                return Ok(Box::pin(futures::stream::iter(chunks)));
            }
            // Subsequent calls: check if there's a continuation prompt
            let has_continuation = request.messages.iter().any(|m| {
                match m {
                    Message::User { content } => content.iter().any(|c| match c {
                        ContentBlock::Text { text } => text.contains("continue"),
                        _ => false,
                    }),
                    _ => false,
                }
            });
            if has_continuation {
                let chunks = vec![
                    Ok(StreamChunk::TextDelta { text: " completed".to_string() }),
                    Ok(StreamChunk::MessageStop {
                        stop_reason: StopReason::EndTurn,
                        usage: Usage { input_tokens: 20, output_tokens: 10, cache_read_tokens: None },
                    }),
                ];
                return Ok(Box::pin(futures::stream::iter(chunks)));
            }
            // Shouldn't reach here
            let chunks = vec![
                Ok(StreamChunk::TextDelta { text: "fallback".to_string() }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage { input_tokens: 5, output_tokens: 5, cache_read_tokens: None },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "max-output-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    #[tokio::test]
    async fn test_recovery_max_tokens_continues_with_prompt() {
        let model: Arc<dyn Model> = Arc::new(MaxOutputThenSuccessModel {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let provider = make_provider(model);
        let agent = Agent::builder("max-tokens-agent").build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh { prompt: "write something long".to_string() };

        let result = run(&agent, input, &config).await.unwrap();
        // The recovery should have appended a continuation prompt and got the completion
        assert!(result.output.contains("completed"));
    }

    /// A model that returns a connection error (unrecoverable) immediately.
    struct ConnectionErrorModel;

    #[async_trait]
    impl Model for ConnectionErrorModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            Err(ModelError::Connection("connection refused".to_string()))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "connection-error-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    #[tokio::test]
    async fn test_recovery_connection_error_gives_up_immediately() {
        let model: Arc<dyn Model> = Arc::new(ConnectionErrorModel);
        let provider = make_provider(model);
        let agent = Agent::builder("conn-error-agent").build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh { prompt: "test".to_string() };

        let result = run(&agent, input, &config).await;
        assert!(result.is_err());
        // Connection errors have no recovery strategy → GiveUp → RecoveryExhausted
        match result.unwrap_err() {
            RunError::RecoveryExhausted(_) => { /* expected */ }
            other => panic!("Expected RecoveryExhausted, got {:?}", other),
        }
    }

    /// A model that emits a stream interrupted error mid-stream.
    struct StreamInterruptedThenSuccessModel {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl Model for StreamInterruptedThenSuccessModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            let count = self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                // First call: stream starts then errors
                let chunks: Vec<Result<StreamChunk, ModelError>> = vec![
                    Ok(StreamChunk::TextDelta { text: "partial".to_string() }),
                    Err(ModelError::StreamInterrupted("connection reset".to_string())),
                ];
                return Ok(Box::pin(futures::stream::iter(chunks)));
            }
            // Recovery call succeeds
            let chunks = vec![
                Ok(StreamChunk::TextDelta { text: "full response".to_string() }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage { input_tokens: 10, output_tokens: 5, cache_read_tokens: None },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "stream-interrupted-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    #[tokio::test]
    async fn test_recovery_stream_interrupted_retries_with_continuation() {
        let model: Arc<dyn Model> = Arc::new(StreamInterruptedThenSuccessModel {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let provider = make_provider(model);
        let agent = Agent::builder("stream-interrupted-agent").build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh { prompt: "test".to_string() };

        let result = run(&agent, input, &config).await.unwrap();
        assert_eq!(result.output, "full response");
    }

    // --- Property 22: Usage and cost accumulation ---

    use proptest::prelude::*;

    /// A mock model with configurable pricing for property tests.
    struct PricingModel {
        input_cost: f64,
        output_cost: f64,
    }

    #[async_trait]
    impl Model for PricingModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            unimplemented!("PricingModel is only used for accumulate_usage tests")
        }
        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }
        fn name(&self) -> &str { "pricing-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { self.input_cost }
        fn output_cost_per_million(&self) -> f64 { self.output_cost }
    }

    /// Strategy for generating a Usage value with reasonable token counts.
    fn arb_usage() -> impl Strategy<Value = Usage> {
        (0u64..1_000_000, 0u64..1_000_000, proptest::option::of(0u64..500_000))
            .prop_map(|(input_tokens, output_tokens, cache_read_tokens)| Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
            })
    }

    /// Strategy for generating a pricing rate (cost per million tokens in USD).
    fn arb_pricing_rate() -> impl Strategy<Value = f64> {
        // Realistic range: $0.01 to $100 per million tokens
        (1u32..10000).prop_map(|x| x as f64 / 100.0)
    }

    proptest! {
        /// **Validates: Requirements 26.2, 26.3**
        ///
        /// Property 22: Usage and cost accumulation.
        /// Generate sequences of Usage values with pricing rates, assert total_usage
        /// equals component-wise sum and total_cost_usd equals expected formula.
        #[test]
        fn prop_usage_and_cost_accumulation(
            usages in proptest::collection::vec(arb_usage(), 1..=10),
            input_rate in arb_pricing_rate(),
            output_rate in arb_pricing_rate(),
        ) {
            let model = PricingModel {
                input_cost: input_rate,
                output_cost: output_rate,
            };
            let mut state = RunState::new("prop-run".to_string(), None, None);

            // Accumulate all usages
            for usage in &usages {
                accumulate_usage(&mut state, usage, &model);
            }

            // Assert: total input_tokens == sum of all individual input_tokens
            let expected_input: u64 = usages.iter().map(|u| u.input_tokens).sum();
            prop_assert_eq!(state.total_usage.input_tokens, expected_input);

            // Assert: total output_tokens == sum of all individual output_tokens
            let expected_output: u64 = usages.iter().map(|u| u.output_tokens).sum();
            prop_assert_eq!(state.total_usage.output_tokens, expected_output);

            // Assert: cache_read_tokens accumulates correctly when present
            let expected_cache: Option<u64> = {
                let sum: u64 = usages.iter().filter_map(|u| u.cache_read_tokens).sum();
                if usages.iter().any(|u| u.cache_read_tokens.is_some()) {
                    Some(sum)
                } else {
                    None
                }
            };
            prop_assert_eq!(state.total_usage.cache_read_tokens, expected_cache);

            // Assert: total_cost_usd == sum of per-turn costs using the formula
            let expected_cost: f64 = usages.iter().map(|u| {
                (u.input_tokens as f64) * input_rate / 1_000_000.0
                    + (u.output_tokens as f64) * output_rate / 1_000_000.0
            }).sum();
            let cost_diff = (state.total_cost_usd - expected_cost).abs();
            prop_assert!(
                cost_diff < 1e-10,
                "Cost mismatch: got {}, expected {}, diff {}",
                state.total_cost_usd, expected_cost, cost_diff
            );
        }

        // --- Property 23: Budget enforcement ---
        // **Validates: Requirements 26.4**
        //
        // For any configured budget value, when total_cost_usd exceeds the budget
        // after a turn completes, the RunLoop shall resolve Aborted with reason
        // "budget_exceeded" before starting the next turn. When cost stays within
        // budget, the run completes normally.

        #[test]
        fn prop_budget_enforcement(
            budget in 0.001f64..10.0f64,
            input_tokens in 100u64..1_000_000u64,
            output_tokens in 100u64..1_000_000u64,
            input_rate in arb_pricing_rate(),
            output_rate in arb_pricing_rate(),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // Calculate the cost of a single turn
                let turn_cost = (input_tokens as f64) * input_rate / 1_000_000.0
                    + (output_tokens as f64) * output_rate / 1_000_000.0;

                let model: Arc<dyn Model> = Arc::new(BudgetPropModel {
                    input_tokens_per_turn: input_tokens,
                    output_tokens_per_turn: output_tokens,
                    input_cost_per_m: input_rate,
                    output_cost_per_m: output_rate,
                });
                let provider = make_provider(model);
                let agent = Agent::builder("prop-budget-agent")
                    .instructions(Instructions::Static("test".into()))
                    .build();
                let config = RunConfig::builder(provider, "mock")
                    .budget_usd(budget)
                    .build();
                let input = Input::Fresh {
                    prompt: "test".to_string(),
                };

                let result = run(&agent, input, &config).await;

                if turn_cost > budget {
                    // Cost exceeds budget on first turn → should abort
                    match result {
                        Err(RunError::Aborted(ref reason)) => {
                            prop_assert_eq!(reason, "budget_exceeded");
                        }
                        ref other => {
                            prop_assert!(false,
                                "Expected Aborted(budget_exceeded) when turn_cost ({}) > budget ({}), got: {:?}",
                                turn_cost, budget, other);
                        }
                    }
                } else {
                    // Cost within budget → should complete normally
                    match result {
                        Ok(ref run_result) => {
                            prop_assert_eq!(&run_result.output, "done");
                            // Verify cost was tracked correctly
                            let expected_cost = turn_cost;
                            prop_assert!((run_result.cost_usd - expected_cost).abs() < 1e-10,
                                "Cost mismatch: got {}, expected {}", run_result.cost_usd, expected_cost);
                        }
                        Err(ref e) => {
                            prop_assert!(false,
                                "Expected Ok when turn_cost ({}) <= budget ({}), got error: {:?}",
                                turn_cost, budget, e);
                        }
                    }
                }
                Ok(())
            })?;
        }
    }

    /// A mock model with configurable cost rates and fixed token usage per turn.
    /// Used exclusively by the budget enforcement property test (Property 23).
    struct BudgetPropModel {
        input_tokens_per_turn: u64,
        output_tokens_per_turn: u64,
        input_cost_per_m: f64,
        output_cost_per_m: f64,
    }

    #[async_trait]
    impl Model for BudgetPropModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            let input_tokens = self.input_tokens_per_turn;
            let output_tokens = self.output_tokens_per_turn;
            let chunks = vec![
                Ok(StreamChunk::TextDelta {
                    text: "done".to_string(),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens,
                        output_tokens,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "budget-prop-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { self.input_cost_per_m }
        fn output_cost_per_million(&self) -> f64 { self.output_cost_per_m }
    }

    // ===================================================================
    // Property 14: Approval Delegation to Parent Handler (Task 8.4)
    // **Validates: Requirements 7.2, 7.3, 10.1**
    //
    // Tests that the run loop's Interruption handling correctly delegates
    // to the ApprovalHandler when present, processing Allow/Deny/AlwaysAllow
    // responses, and preserves legacy Interruption behavior when no handler.
    // ===================================================================

    /// A model that invokes a tool requiring approval, then responds on second call.
    struct ApprovalToolModel;

    #[async_trait]
    impl Model for ApprovalToolModel {
        async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
            // If there are tool results in messages, respond with final text
            let has_tool_result = request.messages.iter().any(|m| {
                matches!(m, Message::ToolResult { .. })
            });

            if has_tool_result {
                let chunks = vec![
                    Ok(StreamChunk::TextDelta {
                        text: "Approval flow complete.".to_string(),
                    }),
                    Ok(StreamChunk::MessageStop {
                        stop_reason: StopReason::EndTurn,
                        usage: Usage {
                            input_tokens: 20,
                            output_tokens: 10,
                            cache_read_tokens: None,
                        },
                    }),
                ];
                return Ok(Box::pin(futures::stream::iter(chunks)));
            }

            // First call: invoke a tool that requires approval
            let chunks = vec![
                Ok(StreamChunk::ToolUseStart {
                    id: "tool_approval_001".to_string(),
                    name: "dangerous_tool".to_string(),
                }),
                Ok(StreamChunk::ToolUseInputDelta {
                    id: "tool_approval_001".to_string(),
                    delta: r#"{"action":"delete"}"#.to_string(),
                }),
                Ok(StreamChunk::ToolUseEnd {
                    id: "tool_approval_001".to_string(),
                    input: json!({"action": "delete"}),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        input_tokens: 15,
                        output_tokens: 8,
                        cache_read_tokens: None,
                    },
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            unimplemented!()
        }

        fn name(&self) -> &str { "approval-tool-model" }
        fn provider(&self) -> &str { "mock" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 3.0 }
        fn output_cost_per_million(&self) -> f64 { 15.0 }
    }

    /// A tool that always requires approval.
    struct DangerousTool;

    #[async_trait]
    impl Tool for DangerousTool {
        fn name(&self) -> &str { "dangerous_tool" }
        fn description(&self) -> &str { "A tool that requires approval" }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object", "properties": {"action": {"type": "string"}}})
        }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }
        fn approval_requirement(&self) -> crate::tool::ApprovalRequirement {
            crate::tool::ApprovalRequirement::Always
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, crate::error::ToolError> {
            Ok(ToolOutput::Text("dangerous action executed".to_string()))
        }
    }

    /// An ApprovalHandler that always returns Allow.
    struct AllowAllHandler;

    #[async_trait]
    impl crate::config::ApprovalHandler for AllowAllHandler {
        async fn request_approval(
            &self,
            context: &ApprovalContext,
        ) -> Vec<ApprovalResponse> {
            context.pending.iter().map(|_| ApprovalResponse::Allow).collect()
        }
    }

    /// An ApprovalHandler that always returns Deny.
    struct DenyAllHandler;

    #[async_trait]
    impl crate::config::ApprovalHandler for DenyAllHandler {
        async fn request_approval(
            &self,
            context: &ApprovalContext,
        ) -> Vec<ApprovalResponse> {
            context.pending.iter().map(|_| ApprovalResponse::Deny).collect()
        }
    }

    /// An ApprovalHandler that returns AlwaysAllow with a specific pattern.
    struct AlwaysAllowHandler {
        pattern: String,
    }

    #[async_trait]
    impl crate::config::ApprovalHandler for AlwaysAllowHandler {
        async fn request_approval(
            &self,
            context: &ApprovalContext,
        ) -> Vec<ApprovalResponse> {
            context
                .pending
                .iter()
                .map(|_| ApprovalResponse::AlwaysAllow {
                    pattern: self.pattern.clone(),
                })
                .collect()
        }
    }

    /// An ApprovalHandler that captures the ApprovalContext for verification.
    struct CapturingApprovalHandler {
        captured: Arc<std::sync::Mutex<Vec<ApprovalContext>>>,
    }

    #[async_trait]
    impl crate::config::ApprovalHandler for CapturingApprovalHandler {
        async fn request_approval(
            &self,
            context: &ApprovalContext,
        ) -> Vec<ApprovalResponse> {
            self.captured.lock().unwrap().push(context.clone());
            context.pending.iter().map(|_| ApprovalResponse::Allow).collect()
        }
    }

    #[tokio::test]
    async fn test_approval_handler_allow_continues_run() {
        // When handler returns Allow, the tool result is kept and the loop continues.
        let model: Arc<dyn Model> = Arc::new(ApprovalToolModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(DangerousTool);
        let handler: Arc<dyn crate::config::ApprovalHandler> = Arc::new(AllowAllHandler);

        let agent = Agent::builder("approval-agent")
            .tool(tool)
            .build();
        let permissions = crate::permission::PermissionEngine::new(
            crate::permission::PermissionMode::Normal,
        );
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .approval_handler(handler)
            .build();
        let input = Input::Fresh {
            prompt: "Do something dangerous".to_string(),
        };

        let result = run(&agent, input, &config).await.unwrap();
        // The model sees the tool result and responds with "Approval flow complete."
        assert_eq!(result.output, "Approval flow complete.");
        assert_eq!(result.turns, 2);
    }

    #[tokio::test]
    async fn test_approval_handler_deny_injects_denial_result() {
        // When handler returns Deny, a denial message is injected and the loop continues.
        let model: Arc<dyn Model> = Arc::new(ApprovalToolModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(DangerousTool);
        let handler: Arc<dyn crate::config::ApprovalHandler> = Arc::new(DenyAllHandler);

        let agent = Agent::builder("deny-agent")
            .tool(tool)
            .build();
        let permissions = crate::permission::PermissionEngine::new(
            crate::permission::PermissionMode::Normal,
        );
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .approval_handler(handler)
            .build();
        let input = Input::Fresh {
            prompt: "Do something dangerous".to_string(),
        };

        let result = run(&agent, input, &config).await.unwrap();
        // The denial result is injected as a ToolResult; the model sees it and responds.
        assert_eq!(result.output, "Approval flow complete.");
        assert_eq!(result.turns, 2);

        // Verify that the denial message was injected into the state
        let has_denial = result.state.messages.iter().any(|m| match m {
            Message::ToolResult { content, is_error, .. } => {
                *is_error && content.contains("Permission denied")
                    && content.contains("dangerous_tool")
            }
            _ => false,
        });
        assert!(has_denial, "Expected a denial ToolResult in messages");
    }

    #[tokio::test]
    async fn test_approval_handler_always_allow_grants_session_permission() {
        // When handler returns AlwaysAllow, the session grant is stored.
        let model: Arc<dyn Model> = Arc::new(ApprovalToolModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(DangerousTool);
        let handler: Arc<dyn crate::config::ApprovalHandler> =
            Arc::new(AlwaysAllowHandler {
                pattern: "dangerous_tool".to_string(),
            });

        let agent = Agent::builder("always-allow-agent")
            .tool(tool)
            .build();
        let permissions = crate::permission::PermissionEngine::new(
            crate::permission::PermissionMode::Normal,
        );
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .approval_handler(handler)
            .build();
        let input = Input::Fresh {
            prompt: "Do something dangerous".to_string(),
        };

        let result = run(&agent, input, &config).await.unwrap();
        assert_eq!(result.output, "Approval flow complete.");
        assert_eq!(result.turns, 2);
    }

    #[tokio::test]
    async fn test_no_approval_handler_returns_interruption() {
        // When no handler is set, the run returns with pending_approvals (legacy behavior).
        let model: Arc<dyn Model> = Arc::new(ApprovalToolModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(DangerousTool);

        let agent = Agent::builder("no-handler-agent")
            .tool(tool)
            .build();
        let permissions = crate::permission::PermissionEngine::new(
            crate::permission::PermissionMode::Normal,
        );
        // No approval_handler set → None
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .build();
        let input = Input::Fresh {
            prompt: "Do something dangerous".to_string(),
        };

        let result = run(&agent, input, &config).await.unwrap();
        // The run should return early with pending_approvals populated
        assert!(!result.state.pending_approvals.is_empty());
        assert_eq!(result.state.pending_approvals.len(), 1);
        assert_eq!(result.state.pending_approvals[0].tool_name, "dangerous_tool");
        // The run returns on the first turn without completing
        assert_eq!(result.turns, 0);
    }

    #[tokio::test]
    async fn test_approval_handler_receives_agent_name() {
        // Verify that the ApprovalContext includes the agent_name from RunConfig.
        let model: Arc<dyn Model> = Arc::new(ApprovalToolModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(DangerousTool);
        let captured = Arc::new(std::sync::Mutex::new(Vec::<ApprovalContext>::new()));
        let handler: Arc<dyn crate::config::ApprovalHandler> =
            Arc::new(CapturingApprovalHandler {
                captured: Arc::clone(&captured),
            });

        let agent = Agent::builder("context-agent")
            .tool(tool)
            .build();
        let permissions = crate::permission::PermissionEngine::new(
            crate::permission::PermissionMode::Normal,
        );
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .approval_handler(handler)
            .agent_name("research-sub-agent")
            .build();
        let input = Input::Fresh {
            prompt: "Do something dangerous".to_string(),
        };

        let _result = run(&agent, input, &config).await.unwrap();

        let contexts = captured.lock().unwrap();
        assert_eq!(contexts.len(), 1);
        assert_eq!(
            contexts[0].agent_name,
            Some("research-sub-agent".to_string())
        );
        assert_eq!(contexts[0].pending.len(), 1);
        assert_eq!(contexts[0].pending[0].tool_name, "dangerous_tool");
    }

    // ===================================================================
    // Non-interactive DenyAllApprovalHandler integration tests (Task 13.2)
    // Validates: Requirements 10.1, 10.2, 10.3
    // ===================================================================

    #[tokio::test]
    async fn test_deny_all_approval_handler_denies_tool_in_non_interactive_mode() {
        // Integration test: use the real DenyAllApprovalHandler (the one from config.rs)
        // to verify that in non-interactive mode a tool requiring approval is denied,
        // the denial is injected as a ToolResult, and the run loop completes without hanging.
        use crate::config::DenyAllApprovalHandler;

        let model: Arc<dyn Model> = Arc::new(ApprovalToolModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(DangerousTool);
        let handler: Arc<dyn crate::config::ApprovalHandler> = Arc::new(DenyAllApprovalHandler);

        let agent = Agent::builder("non-interactive-agent")
            .tool(tool)
            .build();
        let permissions = crate::permission::PermissionEngine::new(
            crate::permission::PermissionMode::Normal,
        );
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .approval_handler(handler)
            .build();
        let input = Input::Fresh {
            prompt: "Do something dangerous".to_string(),
        };

        // The run should complete without hanging (DenyAllApprovalHandler responds immediately)
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run(&agent, input, &config),
        )
        .await
        .expect("Run should complete without hanging in non-interactive mode");

        let result = result.expect("Run should succeed");

        // The model should have received the denial and responded
        assert_eq!(result.output, "Approval flow complete.");
        assert_eq!(result.turns, 2);

        // Verify that a denial ToolResult was injected into the conversation
        let has_denial = result.state.messages.iter().any(|m| match m {
            Message::ToolResult { content, is_error, .. } => {
                *is_error && content.contains("Permission denied")
                    && content.contains("dangerous_tool")
            }
            _ => false,
        });
        assert!(
            has_denial,
            "Expected a 'Permission denied' ToolResult for dangerous_tool in non-interactive mode"
        );
    }

    #[tokio::test]
    async fn test_deny_all_approval_handler_run_loop_continues_after_denial() {
        // Verify the run loop doesn't panic or hang after denial — it continues
        // processing the model's next response.
        use crate::config::DenyAllApprovalHandler;

        let model: Arc<dyn Model> = Arc::new(ApprovalToolModel);
        let provider = make_provider(model);
        let tool: Arc<dyn Tool> = Arc::new(DangerousTool);
        let handler: Arc<dyn crate::config::ApprovalHandler> = Arc::new(DenyAllApprovalHandler);

        let agent = Agent::builder("continuation-agent")
            .tool(tool)
            .build();
        let permissions = crate::permission::PermissionEngine::new(
            crate::permission::PermissionMode::Normal,
        );
        let config = RunConfig::builder(provider, "mock")
            .permissions(permissions)
            .approval_handler(handler)
            .build();
        let input = Input::Fresh {
            prompt: "Try dangerous action".to_string(),
        };

        let result = run(&agent, input, &config).await.unwrap();

        // The run completed (2 turns: first invokes tool → denied, second is final response)
        assert_eq!(result.turns, 2);
        // Output is present (not empty), confirming the model responded after denial
        assert!(!result.output.is_empty());
    }
}

