//! AG-UI ↔ Arlo bridge: translates between the AG-UI `Agent` trait and Arlo's run loop.

use std::sync::Arc;

use ag_ui_protocol::{
    BaseEvent, Event, Interrupt, RunAgentInput, RunErrorEvent, RunFinishedEvent,
    RunFinishedOutcome, StepFinishedEvent, StepStartedEvent, TextMessageContentEvent,
    TextMessageEndEvent, TextMessageRole, TextMessageStartEvent, ToolCallArgsEvent,
    ToolCallEndEvent, ToolCallResultEvent, ToolCallStartEvent, ToolResultRole,
};
use ag_ui_server::{AgentError, EventEmitter, RunOutcome};
use agent_core::agent::Agent;
use agent_core::config::{Input, RunConfig};
use agent_core::event::RunEvent;
use agent_core::message::{ContentBlock, Message as ArloMessage};
use agent_core::next_step::PendingApproval;
use agent_core::run_loop::run_stream;
use agent_core::stream::StreamChunk;
use futures::StreamExt;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::approval::{map_resume_to_responses, AgUiApprovalHandler};
use super::session::{ParkedRun, RunSession, RunStream, SessionState, SessionStore};

// ---------------------------------------------------------------------------
// DrainOutcome
// ---------------------------------------------------------------------------

/// Outcome of draining a run stream.
pub enum DrainOutcome {
    /// The stream ran to completion (terminal AG-UI event emitted).
    Finished,
    /// The run was interrupted — the pending approvals are returned.
    Interrupted(Vec<PendingApproval>),
    /// The run failed or the client disconnected.
    Failed(String),
}

// ---------------------------------------------------------------------------
// drain — shared drain loop used by both the new-run and resume paths
// ---------------------------------------------------------------------------

/// Drain a run stream, mapping events through `mapper` and emitting them
/// through `emitter`, until a terminal event or an interrupt occurs.
///
/// Returns:
/// - `DrainOutcome::Finished` when a terminal event is emitted
/// - `DrainOutcome::Interrupted(pending)` when the approval handler signals an interrupt
/// - `DrainOutcome::Failed(msg)` on run error or client disconnect
async fn drain(
    stream: &mut RunStream,
    mapper: &mut EventMapper,
    emitter: &EventEmitter,
    interrupt_rx: &mut mpsc::Receiver<Vec<PendingApproval>>,
) -> DrainOutcome {
    loop {
        tokio::select! {
            event = stream.next() => {
                match event {
                    Some(run_event) => {
                        let ag_events = mapper.map_event(run_event);
                        for ev in ag_events {
                            match &ev {
                                Event::RunFinished(_) => {
                                    // Do NOT emit the terminal event through the emitter.
                                    // run_agent (the outer pipeline) owns the terminal
                                    // RUN_FINISHED/RUN_ERROR lifecycle events; the bridge
                                    // signals completion by returning RunOutcome/AgentError.
                                    // Emitting here would produce a protocol-violating
                                    // duplicate terminal event.
                                    return DrainOutcome::Finished;
                                }
                                Event::RunError(e) => {
                                    let msg = e.message.clone();
                                    // Same reasoning: let run_agent emit RUN_ERROR from
                                    // the AgentError we return, rather than double-emitting.
                                    return DrainOutcome::Failed(msg);
                                }
                                _ => {
                                    if emitter.emit(ev).await.is_err() {
                                        return DrainOutcome::Failed(
                                            "client disconnected".to_string(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        // Stream ended without a terminal event — flush and finish.
                        let flush = mapper.finish();
                        for ev in flush {
                            let _ = emitter.emit(ev).await;
                        }
                        return DrainOutcome::Finished;
                    }
                }
            }
            pending = interrupt_rx.recv() => {
                if let Some(pending_approvals) = pending {
                    // Flush any open text message before parking.
                    let flush = mapper.finish();
                    for ev in flush {
                        let _ = emitter.emit(ev).await;
                    }
                    return DrainOutcome::Interrupted(pending_approvals);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ArloBridge — implements ag_ui_server::Agent
// ---------------------------------------------------------------------------

/// Convert a list of `PendingApproval`s into AG-UI `Interrupt`s.
///
/// `request_id` is `"approval-{tool_call_id}"` (set in `run_loop.rs`).
/// `id` keeps the full `request_id` (resume correlation key).
/// `tool_call_id` strips the prefix so the client can match the `TOOL_CALL_START`.
fn pending_to_interrupts(pending: Vec<PendingApproval>) -> Vec<Interrupt> {
    pending
        .into_iter()
        .map(|p| {
            let tool_call_id = p.request_id.strip_prefix("approval-").map(str::to_string);
            Interrupt {
                id: p.request_id.clone(),
                reason: "tool_call".to_string(),
                message: Some(format!(
                    "Run {}: {}",
                    p.tool_name,
                    serde_json::to_string(&p.tool_input).unwrap_or_default()
                )),
                tool_call_id,
                response_schema: None,
                expires_at: Some((chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339()),
                metadata: Some(
                    serde_json::json!({ "toolName": p.tool_name, "toolInput": p.tool_input })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            }
        })
        .collect()
}

/// Bridges AG-UI protocol requests to Arlo's run loop.
pub struct ArloBridge {
    pub agent: Agent,
    pub base_config: RunConfig,
    pub sessions: Arc<SessionStore>,
}

impl ArloBridge {
    pub fn new(agent: Agent, base_config: RunConfig, sessions: Arc<SessionStore>) -> Self {
        Self {
            agent,
            base_config,
            sessions,
        }
    }

    /// Convert AG-UI messages to Arlo `Input::Items`.
    /// We only pick user/assistant/system text — ignore tool messages and
    /// multimodal content beyond plain text for now.
    fn convert_messages(messages: &[ag_ui_protocol::Message]) -> Input {
        let items: Vec<ArloMessage> = messages
            .iter()
            .filter_map(|m| match m {
                ag_ui_protocol::Message::User(u) => {
                    let text = match &u.content {
                        ag_ui_protocol::message::UserContent::Text(t) => t.clone(),
                        ag_ui_protocol::message::UserContent::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| match p {
                                ag_ui_protocol::message::InputContent::Text { text } => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    Some(ArloMessage::User {
                        content: vec![ContentBlock::Text { text }],
                    })
                }
                ag_ui_protocol::Message::Assistant(a) => {
                    let text = a.content.clone().unwrap_or_default();
                    if text.is_empty() {
                        return None;
                    }
                    Some(ArloMessage::Assistant {
                        content: vec![ContentBlock::Text { text }],
                        usage: None,
                    })
                }
                ag_ui_protocol::Message::System(s) => Some(ArloMessage::System {
                    content: s.content.clone(),
                }),
                ag_ui_protocol::Message::Developer(d) => {
                    // Treat developer messages as system messages
                    Some(ArloMessage::System {
                        content: d.content.clone(),
                    })
                }
                // Tool, Activity, Reasoning — skip
                _ => None,
            })
            .collect();

        if items.is_empty() {
            Input::Fresh {
                prompt: String::new(),
            }
        } else {
            Input::Items { messages: items }
        }
    }
}

impl ag_ui_server::Agent for ArloBridge {
    fn run(
        &self,
        input: RunAgentInput,
        emitter: EventEmitter,
    ) -> impl std::future::Future<Output = Result<RunOutcome, AgentError>> + Send {
        let agent = self.agent.clone();
        let base_config = self.base_config.clone();
        let sessions = Arc::clone(&self.sessions);
        let thread_id = input.thread_id.clone();

        async move {
            // --- Resume path ---
            if let Some(ref resume_entries) = input.resume {
                if !resume_entries.is_empty() {
                    let mut session = match sessions.remove(&thread_id) {
                        Some(s) => s,
                        None => {
                            return Err(AgentError::msg(format!(
                                "No suspended run for thread_id '{thread_id}'"
                            )));
                        }
                    };

                    let SessionState::Interrupted { .. } = session.state else {
                        return Err(AgentError::msg(format!(
                            "No suspended run for thread_id '{thread_id}'"
                        )));
                    };

                    // Unblock drive() — sends the approval responses through the oneshot.
                    let responses = map_resume_to_responses(resume_entries);
                    if let Some(tx) = session.resume_tx.take() {
                        let _ = tx.send(responses);
                    }

                    // Take the live stream that was parked during the interrupt.
                    let mut parked = match session.parked.take() {
                        Some(p) => p,
                        None => {
                            return Err(AgentError::msg("run stream is not available for resume"));
                        }
                    };

                    // Re-insert a running session so the approval handler can find it if
                    // the resumed run hits another approval request.
                    sessions.insert(
                        thread_id.clone(),
                        RunSession {
                            run_id: input.run_id.clone(),
                            state: SessionState::Running,
                            created_at: session.created_at,
                            last_active: std::time::Instant::now(),
                            resume_tx: None,
                            parked: None,
                        },
                    );

                    // Drain the retained stream into this request's emitter.
                    return match drain(
                        &mut parked.stream,
                        &mut parked.mapper,
                        &emitter,
                        &mut parked.interrupt_rx,
                    )
                    .await
                    {
                        DrainOutcome::Finished => {
                            sessions.remove(&thread_id);
                            Ok(RunOutcome::success())
                        }
                        DrainOutcome::Interrupted(pending_approvals) => {
                            // Re-park for a consecutive approval (Req 5.4).
                            let real_parked = ParkedRun {
                                stream: parked.stream,
                                mapper: parked.mapper,
                                interrupt_rx: parked.interrupt_rx,
                            };
                            sessions.update_parked(&thread_id, real_parked);
                            let interrupts = pending_to_interrupts(pending_approvals);
                            Ok(RunOutcome::Interrupt(interrupts))
                        }
                        DrainOutcome::Failed(msg) => {
                            sessions.remove(&thread_id);
                            Err(AgentError::msg(msg))
                        }
                    };
                }
            }

            // --- New run path ---
            let arlo_input = Self::convert_messages(&input.messages);

            // Channel for the approval handler to signal interrupts
            let (interrupt_tx, mut interrupt_rx) = mpsc::channel::<Vec<PendingApproval>>(1);

            // Build a config with our custom approval handler by cloning base_config.
            // This preserves the PermissionEngine (and task store) the CLI assembled,
            // rather than constructing a fresh config that would drop those fields.
            let approval_handler: Arc<dyn agent_core::config::ApprovalHandler> =
                Arc::new(AgUiApprovalHandler {
                    sessions: Arc::clone(&sessions),
                    thread_id: thread_id.clone(),
                    interrupt_tx,
                });
            let mut config = base_config.clone();
            config.approval_handler = Some(approval_handler);

            // Start the run stream
            let mut stream = run_stream(&agent, arlo_input, &config);

            // Register a placeholder session so the approval handler can find it.
            // `parked` is None until/unless the run is interrupted.
            sessions.insert(
                thread_id.clone(),
                RunSession {
                    run_id: input.run_id.clone(),
                    state: SessionState::Running,
                    created_at: std::time::Instant::now(),
                    last_active: std::time::Instant::now(),
                    resume_tx: None,
                    parked: None,
                },
            );

            // Drain the stream into the emitter.
            let mut mapper = EventMapper::new();
            match drain(&mut stream, &mut mapper, &emitter, &mut interrupt_rx).await {
                DrainOutcome::Finished => {
                    sessions.remove(&thread_id);
                    Ok(RunOutcome::success())
                }
                DrainOutcome::Interrupted(pending_approvals) => {
                    // Park the live stream so the resume path can continue it.
                    let real_parked = ParkedRun {
                        stream,
                        mapper,
                        interrupt_rx,
                    };
                    sessions.update_parked(&thread_id, real_parked);
                    let interrupts = pending_to_interrupts(pending_approvals);
                    Ok(RunOutcome::Interrupt(interrupts))
                }
                DrainOutcome::Failed(msg) => {
                    sessions.remove(&thread_id);
                    Err(AgentError::msg(msg))
                }
            }
        }
    }
}

/// Stateful mapper that translates Arlo `RunEvent`s into AG-UI `Event`s.
///
/// Tracks whether a text message is "open" so it can emit the correct
/// START/CONTENT/END lifecycle events around consecutive TextDelta chunks.
#[allow(dead_code)]
pub struct EventMapper {
    /// The message_id of the currently-open text message, if any.
    open_message_id: Option<String>,
    /// The step_name of the currently-open step, if any.
    open_step: Option<String>,
    /// tool_call_ids that got a TOOL_CALL_START but no TOOL_CALL_END yet.
    ///
    /// TOOL_CALL_START comes from the model stream; TOOL_CALL_END only comes
    /// from `ToolEnd` after the tool actually executes. Any run that ends in
    /// between (approval interrupt, error, max turns, a tool call the run loop
    /// never executes) would otherwise emit RUN_FINISHED with the call still
    /// open, which AG-UI clients reject.
    open_tool_calls: Vec<String>,
}

#[allow(dead_code)]
impl EventMapper {
    pub fn new() -> Self {
        Self {
            open_message_id: None,
            open_step: None,
            open_tool_calls: Vec::new(),
        }
    }

    /// Translate a single `RunEvent` into zero or more AG-UI events.
    pub fn map_event(&mut self, event: RunEvent) -> Vec<Event> {
        match event {
            RunEvent::StreamChunk(chunk) => self.map_stream_chunk(chunk),
            RunEvent::TurnStart { turn, .. } => {
                let mut out = self.close_text_message();
                out.extend(self.close_tool_calls());
                out.extend(self.close_step());
                out.push(Event::StepStarted(StepStartedEvent {
                    base: BaseEvent::default(),
                    step_name: format!("turn-{turn}"),
                }));
                self.open_step = Some(format!("turn-{turn}"));
                out
            }
            RunEvent::ToolStart { .. } => {
                // ToolStart is Arlo's execution-started signal; AG-UI TOOL_CALL_START
                // is emitted from the streaming ToolUseStart chunk instead.
                self.close_text_message()
            }
            RunEvent::ToolEnd {
                id,
                output,
                is_error,
                ..
            } => {
                let mut out = self.close_text_message();
                // ponytail: is_error folded into the content string rather than adding a
                // field. ToolCallResultEvent has no error flag, and the UI only needs to
                // show that it failed.
                out.push(Event::ToolCallResult(ToolCallResultEvent {
                    base: BaseEvent::default(),
                    message_id: Uuid::new_v4().to_string(),
                    tool_call_id: id.clone(),
                    content: if is_error {
                        format!("Error: {output}")
                    } else {
                        output
                    },
                    role: Some(ToolResultRole::Tool),
                }));
                // ponytail: no truncation. Arlo already compacts tool results before they
                // reach the model; this is the raw text for the human. Cap it if a real
                // deployment starts pushing large outputs to browsers.
                //
                // Only close a call that is still open: an interrupt already closed it in
                // the previous run, and a second END would be rejected as unmatched.
                if let Some(pos) = self.open_tool_calls.iter().position(|t| *t == id) {
                    self.open_tool_calls.remove(pos);
                    out.push(Event::ToolCallEnd(ToolCallEndEvent {
                        base: BaseEvent::default(),
                        tool_call_id: id,
                    }));
                }
                out
            }
            RunEvent::AgentEnd { .. } => {
                let mut out = self.close_text_message();
                out.extend(self.close_tool_calls());
                out.extend(self.close_step());
                out.push(Event::RunFinished(RunFinishedEvent {
                    base: BaseEvent::default(),
                    thread_id: String::new(),
                    run_id: String::new(),
                    result: None,
                    outcome: Some(RunFinishedOutcome::Success),
                }));
                out
            }
            RunEvent::Interruption { pending } => {
                let mut out = self.close_text_message();
                out.extend(self.close_tool_calls());
                out.extend(self.close_step());
                let interrupts = pending_to_interrupts(pending);
                out.push(Event::RunFinished(RunFinishedEvent {
                    base: BaseEvent::default(),
                    thread_id: String::new(),
                    run_id: String::new(),
                    result: None,
                    outcome: Some(RunFinishedOutcome::Interrupt { interrupts }),
                }));
                out
            }
            RunEvent::MaxTurns { .. } => {
                let mut out = self.close_text_message();
                out.extend(self.close_tool_calls());
                out.extend(self.close_step());
                out.push(Event::RunFinished(RunFinishedEvent {
                    base: BaseEvent::default(),
                    thread_id: String::new(),
                    run_id: String::new(),
                    result: None,
                    outcome: Some(RunFinishedOutcome::Success),
                }));
                out
            }
            RunEvent::Error { error } => {
                let mut out = self.close_text_message();
                out.extend(self.close_tool_calls());
                out.extend(self.close_step());
                out.push(Event::RunError(RunErrorEvent {
                    base: BaseEvent::default(),
                    message: error,
                    code: None,
                }));
                out
            }
            RunEvent::Aborted { reason } => {
                let mut out = self.close_text_message();
                out.extend(self.close_tool_calls());
                out.extend(self.close_step());
                out.push(Event::RunError(RunErrorEvent {
                    base: BaseEvent::default(),
                    message: reason,
                    code: None,
                }));
                out
            }
            RunEvent::GuardrailTripped { name, reason } => {
                let mut out = self.close_text_message();
                out.extend(self.close_tool_calls());
                out.extend(self.close_step());
                out.push(Event::RunError(RunErrorEvent {
                    base: BaseEvent::default(),
                    message: format!("Guardrail '{name}': {reason}"),
                    code: None,
                }));
                out
            }
            // Compaction and StepResolved are internal — no AG-UI equivalent.
            RunEvent::Compaction { .. } | RunEvent::StepResolved(_) => Vec::new(),
        }
    }

    /// Call when the stream ends to flush any open text message, tool call and step.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut out = self.close_text_message();
        out.extend(self.close_tool_calls());
        out.extend(self.close_step());
        out
    }

    fn map_stream_chunk(&mut self, chunk: StreamChunk) -> Vec<Event> {
        match chunk {
            StreamChunk::TextDelta { text } => {
                let mut out = Vec::new();
                let msg_id = match &self.open_message_id {
                    Some(id) => id.clone(),
                    None => {
                        let id = Uuid::new_v4().to_string();
                        out.push(Event::TextMessageStart(TextMessageStartEvent {
                            base: BaseEvent::default(),
                            message_id: id.clone(),
                            role: TextMessageRole::Assistant,
                            name: None,
                        }));
                        self.open_message_id = Some(id.clone());
                        id
                    }
                };
                out.push(Event::TextMessageContent(TextMessageContentEvent {
                    base: BaseEvent::default(),
                    message_id: msg_id,
                    delta: text,
                }));
                out
            }
            StreamChunk::ThinkingDelta { text: _ } => {
                // ThinkingDelta has no AG-UI equivalent in this demo.
                // ponytail: full REASONING_* lifecycle deferred until needed.
                Vec::new()
            }
            StreamChunk::ToolUseStart { id, name } => {
                let mut out = self.close_text_message();
                self.open_tool_calls.push(id.clone());
                out.push(Event::ToolCallStart(ToolCallStartEvent {
                    base: BaseEvent::default(),
                    tool_call_id: id,
                    tool_call_name: name,
                    parent_message_id: None,
                }));
                out
            }
            StreamChunk::ToolUseInputDelta { id, delta } => {
                vec![Event::ToolCallArgs(ToolCallArgsEvent {
                    base: BaseEvent::default(),
                    tool_call_id: id,
                    delta,
                })]
            }
            // ToolUseEnd and MessageStop are internal model-stream signals,
            // not directly mapped to AG-UI events.
            StreamChunk::ToolUseEnd { .. } | StreamChunk::MessageStop { .. } => Vec::new(),
        }
    }

    /// If a text message is open, emit TEXT_MESSAGE_END and clear state.
    fn close_text_message(&mut self) -> Vec<Event> {
        match self.open_message_id.take() {
            Some(id) => vec![Event::TextMessageEnd(TextMessageEndEvent {
                base: BaseEvent::default(),
                message_id: id,
            })],
            None => Vec::new(),
        }
    }

    /// Close every tool call that was started but never ended.
    ///
    /// AG-UI rejects RUN_FINISHED while a tool call is still active, so every
    /// terminal event closes the stragglers first.
    fn close_tool_calls(&mut self) -> Vec<Event> {
        self.open_tool_calls
            .drain(..)
            .map(|id| {
                Event::ToolCallEnd(ToolCallEndEvent {
                    base: BaseEvent::default(),
                    tool_call_id: id,
                })
            })
            .collect()
    }

    /// If a step is open, emit STEP_FINISHED and clear state.
    fn close_step(&mut self) -> Vec<Event> {
        match self.open_step.take() {
            Some(name) => vec![Event::StepFinished(StepFinishedEvent {
                base: BaseEvent::default(),
                step_name: name,
            })],
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::next_step::PendingApproval;
    use proptest::collection::vec as prop_vec;
    use proptest::prelude::*;

    #[test]
    fn text_delta_starts_message_on_first_chunk() {
        let mut m = EventMapper::new();
        let out = m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "hello".into(),
        }));
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Event::TextMessageStart(_)));
        assert!(matches!(out[1], Event::TextMessageContent(_)));
    }

    #[test]
    fn subsequent_text_deltas_emit_content_only() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "a".into(),
        }));
        let out = m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "b".into(),
        }));
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Event::TextMessageContent(_)));
    }

    #[test]
    fn tool_use_start_closes_open_text() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "x".into(),
        }));
        let out = m.map_event(RunEvent::StreamChunk(StreamChunk::ToolUseStart {
            id: "t1".into(),
            name: "shell".into(),
        }));
        // TEXT_MESSAGE_END + TOOL_CALL_START
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Event::TextMessageEnd(_)));
        assert!(matches!(out[1], Event::ToolCallStart(_)));
    }

    #[test]
    fn tool_use_input_delta_maps_to_args() {
        let mut m = EventMapper::new();
        let out = m.map_event(RunEvent::StreamChunk(StreamChunk::ToolUseInputDelta {
            id: "t1".into(),
            delta: r#"{"cmd":"ls"}"#.into(),
        }));
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Event::ToolCallArgs(_)));
    }

    #[test]
    fn tool_end_maps_to_tool_call_end() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::ToolUseStart {
            id: "t1".into(),
            name: "shell".into(),
        }));
        let out = m.map_event(RunEvent::ToolEnd {
            id: "t1".into(),
            name: "shell".into(),
            output: "ok".into(),
            is_error: false,
        });
        // Expect [ToolCallResult, ToolCallEnd]
        assert_eq!(out.len(), 2);
        match &out[0] {
            Event::ToolCallResult(e) => {
                assert_eq!(e.tool_call_id, "t1");
                assert_eq!(e.content, "ok");
            }
            other => panic!("expected ToolCallResult, got {other:?}"),
        }
        match &out[1] {
            Event::ToolCallEnd(e) => assert_eq!(e.tool_call_id, "t1"),
            other => panic!("expected ToolCallEnd, got {other:?}"),
        }
    }

    #[test]
    fn tool_end_is_error_prefixes_content() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::ToolUseStart {
            id: "t1".into(),
            name: "shell".into(),
        }));
        let out = m.map_event(RunEvent::ToolEnd {
            id: "t1".into(),
            name: "shell".into(),
            output: "permission denied".into(),
            is_error: true,
        });
        assert_eq!(out.len(), 2);
        match &out[0] {
            Event::ToolCallResult(e) => {
                assert_eq!(e.tool_call_id, "t1");
                assert!(
                    e.content.starts_with("Error: "),
                    "expected content to start with 'Error: ', got {:?}",
                    e.content
                );
            }
            other => panic!("expected ToolCallResult, got {other:?}"),
        }
        assert!(matches!(out[1], Event::ToolCallEnd(_)));
    }

    #[test]
    fn tool_end_success_no_error_prefix() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::ToolUseStart {
            id: "t1".into(),
            name: "shell".into(),
        }));
        let out = m.map_event(RunEvent::ToolEnd {
            id: "t1".into(),
            name: "shell".into(),
            output: "hello world".into(),
            is_error: false,
        });
        assert_eq!(out.len(), 2);
        match &out[0] {
            Event::ToolCallResult(e) => {
                assert_eq!(e.content, "hello world");
            }
            other => panic!("expected ToolCallResult, got {other:?}"),
        }
    }

    #[test]
    fn agent_end_closes_text_and_finishes() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "done".into(),
        }));
        let out = m.map_event(RunEvent::AgentEnd {
            agent: "main".into(),
            output: "done".into(),
            usage: Default::default(),
        });
        // TEXT_MESSAGE_END + RUN_FINISHED
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Event::TextMessageEnd(_)));
        assert!(matches!(out[1], Event::RunFinished(_)));
    }

    #[test]
    fn interruption_maps_to_interrupt_outcome() {
        let mut m = EventMapper::new();
        let out = m.map_event(RunEvent::Interruption {
            pending: vec![PendingApproval {
                tool_name: "shell".into(),
                tool_input: serde_json::json!({"cmd": "rm -rf /"}),
                request_id: "req-1".into(),
            }],
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            Event::RunFinished(e) => match &e.outcome {
                Some(RunFinishedOutcome::Interrupt { interrupts }) => {
                    assert_eq!(interrupts.len(), 1);
                    assert_eq!(interrupts[0].id, "req-1");
                }
                other => panic!("expected Interrupt outcome, got {other:?}"),
            },
            other => panic!("expected RunFinished, got {other:?}"),
        }
    }

    // A tool call that never executes (interrupt, error, abandoned call) must still
    // be closed before the terminal event — AG-UI clients reject RUN_FINISHED while
    // a tool call is active. On resume the late ToolEnd must not close it twice.
    #[test]
    fn unexecuted_tool_call_is_closed_before_terminal_event() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::ToolUseStart {
            id: "t1".into(),
            name: "shell".into(),
        }));

        let out = m.map_event(RunEvent::Interruption {
            pending: vec![PendingApproval {
                tool_name: "shell".into(),
                tool_input: serde_json::json!({"cmd": "ls"}),
                request_id: "approval-t1".into(),
            }],
        });
        assert!(
            matches!(&out[0], Event::ToolCallEnd(e) if e.tool_call_id == "t1"),
            "expected ToolCallEnd(t1) first, got {:?}",
            out
        );
        assert!(matches!(out[1], Event::RunFinished(_)));

        // Resume: the result arrives, but the call was already closed.
        let out = m.map_event(RunEvent::ToolEnd {
            id: "t1".into(),
            name: "shell".into(),
            output: "ok".into(),
            is_error: false,
        });
        assert_eq!(out.len(), 1, "expected result only, got {out:?}");
        assert!(matches!(out[0], Event::ToolCallResult(_)));
    }

    #[test]
    fn error_maps_to_run_error() {
        let mut m = EventMapper::new();
        let out = m.map_event(RunEvent::Error {
            error: "boom".into(),
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            Event::RunError(e) => assert_eq!(e.message, "boom"),
            other => panic!("expected RunError, got {other:?}"),
        }
    }

    #[test]
    fn aborted_maps_to_run_error() {
        let mut m = EventMapper::new();
        let out = m.map_event(RunEvent::Aborted {
            reason: "cancelled".into(),
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            Event::RunError(e) => assert_eq!(e.message, "cancelled"),
            other => panic!("expected RunError, got {other:?}"),
        }
    }

    #[test]
    fn max_turns_maps_to_run_finished() {
        let mut m = EventMapper::new();
        let out = m.map_event(RunEvent::MaxTurns { count: 5 });
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Event::RunFinished(_)));
    }

    #[test]
    fn turn_start_closes_text_and_emits_step() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "hi".into(),
        }));
        let out = m.map_event(RunEvent::TurnStart {
            turn: 2,
            agent: "main".into(),
        });
        // TEXT_MESSAGE_END + STEP_STARTED
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Event::TextMessageEnd(_)));
        match &out[1] {
            Event::StepStarted(e) => assert_eq!(e.step_name, "turn-2"),
            other => panic!("expected StepStarted, got {other:?}"),
        }
    }

    // Task 6.3 — step lifecycle tests

    #[test]
    fn turn_start_emits_step_finished_before_new_step_started() {
        let mut m = EventMapper::new();

        // First TurnStart opens a step.
        let out1 = m.map_event(RunEvent::TurnStart {
            turn: 1,
            agent: "main".into(),
        });
        // Should emit only STEP_STARTED("turn-1"); no prior step to close.
        assert_eq!(out1.len(), 1);
        match &out1[0] {
            Event::StepStarted(e) => assert_eq!(e.step_name, "turn-1"),
            other => panic!("expected StepStarted, got {other:?}"),
        }

        // Second TurnStart must close the open step before opening the new one.
        let out2 = m.map_event(RunEvent::TurnStart {
            turn: 2,
            agent: "main".into(),
        });
        // STEP_FINISHED("turn-1") + STEP_STARTED("turn-2")
        assert_eq!(
            out2.len(),
            2,
            "expected [STEP_FINISHED, STEP_STARTED], got {out2:?}"
        );
        match &out2[0] {
            Event::StepFinished(e) => assert_eq!(e.step_name, "turn-1"),
            other => panic!("expected StepFinished('turn-1'), got {other:?}"),
        }
        match &out2[1] {
            Event::StepStarted(e) => assert_eq!(e.step_name, "turn-2"),
            other => panic!("expected StepStarted('turn-2'), got {other:?}"),
        }
    }

    #[test]
    fn terminal_event_closes_step() {
        let mut m = EventMapper::new();

        // Open a step via TurnStart.
        m.map_event(RunEvent::TurnStart {
            turn: 1,
            agent: "main".into(),
        });

        // AgentEnd must close the open step before RUN_FINISHED.
        let out = m.map_event(RunEvent::AgentEnd {
            agent: "main".into(),
            output: "done".into(),
            usage: Default::default(),
        });

        // Expected: [STEP_FINISHED("turn-1"), RUN_FINISHED]
        assert_eq!(
            out.len(),
            2,
            "expected [STEP_FINISHED, RUN_FINISHED], got {out:?}"
        );
        match &out[0] {
            Event::StepFinished(e) => assert_eq!(e.step_name, "turn-1"),
            other => panic!("expected StepFinished('turn-1'), got {other:?}"),
        }
        assert!(
            matches!(out[1], Event::RunFinished(_)),
            "expected RunFinished, got {:?}",
            out[1]
        );
    }

    #[test]
    fn finish_closes_open_step() {
        let mut m = EventMapper::new();

        // Open a step via TurnStart.
        m.map_event(RunEvent::TurnStart {
            turn: 1,
            agent: "main".into(),
        });

        // finish() must close the open step.
        let out = m.finish();
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::StepFinished(s) if s.step_name == "turn-1")),
            "expected STEP_FINISHED('turn-1') in finish() output, got {out:?}"
        );
    }

    #[test]
    fn finish_closes_open_text_message() {
        let mut m = EventMapper::new();
        m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "partial".into(),
        }));
        let out = m.finish();
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Event::TextMessageEnd(_)));
    }

    #[test]
    fn finish_on_empty_produces_nothing() {
        let mut m = EventMapper::new();
        let out = m.finish();
        assert!(out.is_empty());
    }

    #[test]
    fn thinking_delta_produces_no_events() {
        let mut m = EventMapper::new();
        let out = m.map_event(RunEvent::StreamChunk(StreamChunk::ThinkingDelta {
            text: "hmm".into(),
        }));
        assert!(out.is_empty());
    }

    #[test]
    fn message_id_consistent_across_deltas() {
        let mut m = EventMapper::new();
        let out1 = m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "a".into(),
        }));
        let out2 = m.map_event(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "b".into(),
        }));
        let id1 = match &out1[1] {
            Event::TextMessageContent(e) => e.message_id.clone(),
            _ => panic!(),
        };
        let id2 = match &out2[0] {
            Event::TextMessageContent(e) => e.message_id.clone(),
            _ => panic!(),
        };
        assert_eq!(id1, id2);
    }

    // -- Property-based tests --------------------------------------------------

    /// Strategy: generate a non-terminal RunEvent (one that doesn't end the stream).
    fn arb_non_terminal_event() -> impl Strategy<Value = RunEvent> {
        prop_oneof![
            ".*".prop_map(|text| RunEvent::StreamChunk(StreamChunk::TextDelta { text })),
            ("[a-z]{1,8}", "[a-z]{1,8}").prop_map(|(id, name)| RunEvent::StreamChunk(
                StreamChunk::ToolUseStart { id, name }
            )),
            ("[a-z]{1,8}", ".*").prop_map(|(id, delta)| RunEvent::StreamChunk(
                StreamChunk::ToolUseInputDelta { id, delta }
            )),
            ("[a-z]{1,8}", "[a-z]{1,8}", ".*", any::<bool>()).prop_map(
                |(id, name, output, is_error)| RunEvent::ToolEnd {
                    id,
                    name,
                    output,
                    is_error,
                }
            ),
            (1..100u32, "[a-z]{1,8}").prop_map(|(turn, agent)| RunEvent::TurnStart { turn, agent }),
        ]
    }

    /// Strategy: generate a terminal RunEvent.
    fn arb_terminal_event() -> impl Strategy<Value = RunEvent> {
        prop_oneof![
            ("[a-z]{1,8}", ".*").prop_map(|(agent, output)| RunEvent::AgentEnd {
                agent,
                output,
                usage: Default::default(),
            }),
            ".*".prop_map(|error| RunEvent::Error { error }),
            ".*".prop_map(|reason| RunEvent::Aborted { reason }),
            (1..100u32).prop_map(|count| RunEvent::MaxTurns { count }),
        ]
    }

    /// Strategy: a valid RunEvent sequence — zero or more non-terminal events
    /// followed by exactly one terminal event.
    fn arb_run_event_sequence() -> impl Strategy<Value = Vec<RunEvent>> {
        (
            prop::collection::vec(arb_non_terminal_event(), 0..20),
            arb_terminal_event(),
        )
            .prop_map(|(mut events, terminal)| {
                events.push(terminal);
                events
            })
    }

    // Feature: ag-ui-server, Property 1: Event mapping preserves ordering
    // **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7**
    //
    // For any valid RunEvent sequence, the AG-UI output events maintain strict
    // temporal ordering — all outputs from input[i] appear before any outputs
    // from input[i+1].
    proptest! {
        #[test]
        fn prop_event_ordering_preserved(events in arb_run_event_sequence()) {
            let mut mapper = EventMapper::new();
            let mut all_outputs: Vec<(usize, Event)> = Vec::new();

            for (idx, event) in events.into_iter().enumerate() {
                let outputs = mapper.map_event(event);
                for out in outputs {
                    all_outputs.push((idx, out));
                }
            }

            // Also flush any remaining state
            let finish_outputs = mapper.finish();
            let last_idx = all_outputs.last().map(|(i, _)| *i).unwrap_or(0);
            for out in finish_outputs {
                all_outputs.push((last_idx, out));
            }

            // Verify: source indices are monotonically non-decreasing.
            // This confirms no output from a later input appeared before an
            // earlier one — i.e., strict temporal ordering preserved.
            for window in all_outputs.windows(2) {
                prop_assert!(
                    window[0].0 <= window[1].0,
                    "Ordering violated: output from input {} appeared after output from input {}",
                    window[1].0,
                    window[0].0,
                );
            }

            // Verify no duplication: total output count equals sum of individual
            // map_event calls (already guaranteed by construction above, but
            // explicitly confirms no events were silently injected).
            // This is implicitly satisfied by the collection approach.
        }
    }

    /// Strategy: events mixing TextDelta with non-text events that close
    /// an open message. Weighted toward TextDelta to exercise the lifecycle.
    fn arb_lifecycle_event() -> impl Strategy<Value = RunEvent> {
        prop_oneof![
            3 => "[a-z]{1,10}".prop_map(|t| RunEvent::StreamChunk(StreamChunk::TextDelta { text: t })),
            1 => ("[a-z]{1,5}", "[a-z]{1,5}").prop_map(|(id, name)| RunEvent::StreamChunk(
                StreamChunk::ToolUseStart { id, name }
            )),
            1 => (1u32..10u32).prop_map(|t| RunEvent::TurnStart { turn: t, agent: "main".into() }),
            1 => Just(RunEvent::AgentEnd { agent: "main".into(), output: "done".into(), usage: Default::default() }),
        ]
    }

    // Feature: ag-ui-server, Property 2: Text message lifecycle correctness
    // **Validates: Requirements 3.1, 3.7**
    //
    // For any stream with interleaved TextDelta and non-text events:
    // - Exactly one TEXT_MESSAGE_START before first content
    // - Exactly one TEXT_MESSAGE_END before non-text or termination
    // - No TEXT_MESSAGE_CONTENT outside a START/END pair
    // - START/END pairs don't nest
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_text_message_lifecycle(events in proptest::collection::vec(arb_lifecycle_event(), 1..30)) {
            let mut mapper = EventMapper::new();
            let mut all_output: Vec<Event> = Vec::new();

            for ev in events {
                all_output.extend(mapper.map_event(ev));
            }
            // Flush any open message at stream end
            all_output.extend(mapper.finish());

            // Scan output verifying lifecycle invariants
            let mut open_msg_id: Option<String> = None;

            for event in &all_output {
                match event {
                    Event::TextMessageStart(e) => {
                        // No nesting: cannot START while another is open
                        prop_assert!(
                            open_msg_id.is_none(),
                            "TEXT_MESSAGE_START while message {:?} already open",
                            open_msg_id
                        );
                        open_msg_id = Some(e.message_id.clone());
                    }
                    Event::TextMessageContent(e) => {
                        // Content must be inside a START/END pair with matching id
                        prop_assert!(
                            open_msg_id.as_ref() == Some(&e.message_id),
                            "TEXT_MESSAGE_CONTENT id={:?} but open={:?}",
                            e.message_id,
                            open_msg_id
                        );
                    }
                    Event::TextMessageEnd(e) => {
                        // END must close the currently-open message
                        prop_assert!(
                            open_msg_id.as_ref() == Some(&e.message_id),
                            "TEXT_MESSAGE_END id={:?} but open={:?}",
                            e.message_id,
                            open_msg_id
                        );
                        open_msg_id = None;
                    }
                    _ => {}
                }
            }

            // After finish(), no message should remain open
            prop_assert!(
                open_msg_id.is_none(),
                "Stream ended with unclosed message {:?}",
                open_msg_id
            );
        }
    }

    /// A tool call script: unique id, tool name, and argument deltas.
    #[derive(Debug, Clone)]
    struct ToolCallScript {
        id: String,
        name: String,
        arg_deltas: Vec<String>,
    }

    /// Strategy: generate 1..=5 tool call scripts with unique ids.
    fn arb_tool_call_scripts() -> impl Strategy<Value = Vec<ToolCallScript>> {
        prop_vec(
            (
                "[a-z]{3,8}",                       // id suffix
                "[a-z_]{2,10}",                     // tool name
                prop_vec("[a-z0-9 ]{1,20}", 0..=3), // arg deltas
            ),
            1..=5,
        )
        .prop_map(|raw| {
            raw.into_iter()
                .enumerate()
                .map(|(i, (suffix, name, arg_deltas))| ToolCallScript {
                    id: format!("tc-{i}-{suffix}"),
                    name,
                    arg_deltas,
                })
                .collect()
        })
    }

    /// Convert tool call scripts into a RunEvent sequence:
    /// ToolUseStart → N × ToolUseInputDelta → ToolEnd for each script.
    fn scripts_to_events(scripts: &[ToolCallScript]) -> Vec<RunEvent> {
        let mut events = Vec::new();
        for s in scripts {
            events.push(RunEvent::StreamChunk(StreamChunk::ToolUseStart {
                id: s.id.clone(),
                name: s.name.clone(),
            }));
            for delta in &s.arg_deltas {
                events.push(RunEvent::StreamChunk(StreamChunk::ToolUseInputDelta {
                    id: s.id.clone(),
                    delta: delta.clone(),
                }));
            }
            events.push(RunEvent::ToolEnd {
                id: s.id.clone(),
                name: s.name.clone(),
                output: "ok".into(),
                is_error: false,
            });
        }
        events
    }

    // Feature: ag-ui-server, Property 4: Exactly one terminal event per stream
    // **Validates: Requirements 2.1, 2.2, 3.6, 3.7**
    //
    // For any valid RunEvent sequence ending with a terminal event, the full
    // AG-UI output (including finish()) contains exactly one terminal AG-UI
    // event (RunFinished or RunError) and it is the last event.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_terminal_event_uniqueness(events in arb_run_event_sequence()) {
            let mut mapper = EventMapper::new();
            let mut all_output: Vec<Event> = Vec::new();

            for ev in events {
                all_output.extend(mapper.map_event(ev));
            }
            all_output.extend(mapper.finish());

            // Count terminal AG-UI events (RunFinished or RunError)
            let terminal_positions: Vec<usize> = all_output
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e, Event::RunFinished(_) | Event::RunError(_)))
                .map(|(i, _)| i)
                .collect();

            prop_assert_eq!(
                terminal_positions.len(),
                1,
                "Expected exactly 1 terminal event, found {} at positions {:?}",
                terminal_positions.len(),
                terminal_positions,
            );

            // The single terminal event must be last
            prop_assert_eq!(
                terminal_positions[0],
                all_output.len() - 1,
                "Terminal event at position {}, but last position is {}",
                terminal_positions[0],
                all_output.len() - 1,
            );
        }
    }

    // Feature: ag-ui-server, Property 3: Tool call lifecycle completeness
    // **Validates: Requirements 3.3, 3.4, 3.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_tool_call_lifecycle_completeness(scripts in arb_tool_call_scripts()) {
            let events = scripts_to_events(&scripts);
            let mut mapper = EventMapper::new();
            let mut all_ag: Vec<Event> = Vec::new();
            for ev in events {
                all_ag.extend(mapper.map_event(ev));
            }
            all_ag.extend(mapper.finish());

            let mut starts: Vec<String> = Vec::new();
            let mut ends: Vec<String> = Vec::new();
            let mut args_ids: Vec<String> = Vec::new();

            for ev in &all_ag {
                match ev {
                    Event::ToolCallStart(e) => starts.push(e.tool_call_id.clone()),
                    Event::ToolCallEnd(e) => ends.push(e.tool_call_id.clone()),
                    Event::ToolCallArgs(e) => args_ids.push(e.tool_call_id.clone()),
                    _ => {}
                }
            }

            // 1. Every TOOL_CALL_START has exactly one TOOL_CALL_END with same id
            for id in &starts {
                let n = ends.iter().filter(|e| *e == id).count();
                prop_assert_eq!(n, 1, "tool {}: expected 1 END, got {}", id, n);
            }

            // 2. No TOOL_CALL_END without a prior TOOL_CALL_START
            for id in &ends {
                prop_assert!(starts.contains(id), "END for {id} without START");
            }

            // 3. All TOOL_CALL_ARGS reference a started tool_call_id
            for id in &args_ids {
                prop_assert!(starts.contains(id), "ARGS for {id} without START");
            }

            // 4. Ordering: START < all ARGS < END for each tool call
            for script in &scripts {
                let id = &script.id;
                let start_pos = all_ag.iter().position(|e| matches!(e, Event::ToolCallStart(s) if s.tool_call_id == *id)).unwrap();
                let end_pos = all_ag.iter().position(|e| matches!(e, Event::ToolCallEnd(s) if s.tool_call_id == *id)).unwrap();
                prop_assert!(start_pos < end_pos, "START must precede END for {id}");

                for (pos, ev) in all_ag.iter().enumerate() {
                    if let Event::ToolCallArgs(a) = ev {
                        if a.tool_call_id == *id {
                            prop_assert!(
                                pos > start_pos && pos < end_pos,
                                "ARGS for {id} at {pos} not between START({start_pos}) and END({end_pos})"
                            );
                        }
                    }
                }
            }
        }
    }

    // Feature: ag-ui-server, Property 5: No open step at terminal event
    // **Validates: Requirements 9.1, 9.2, 9.3, 9.4**
    //
    // For any valid run sequence that contains at least one TurnStart followed by
    // a terminal event, when the terminal event is emitted the step lifecycle is
    // closed: every STEP_STARTED that appears before the terminal event has a
    // matching STEP_FINISHED before it.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_no_open_step_at_terminal(
            turn_count in 1u32..5u32,
            terminal in arb_terminal_event(),
        ) {
            let mut mapper = EventMapper::new();
            let mut all_output: Vec<Event> = Vec::new();

            // Emit a sequence of TurnStart events to open and close steps.
            for t in 1..=turn_count {
                all_output.extend(mapper.map_event(RunEvent::TurnStart {
                    turn: t,
                    agent: "main".into(),
                }));
            }
            // Emit terminal event — must close the last open step.
            all_output.extend(mapper.map_event(terminal));
            // finish() is also called as a belt-and-suspenders flush.
            all_output.extend(mapper.finish());

            // Find the position of the terminal AG-UI event.
            let terminal_pos = all_output
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e, Event::RunFinished(_) | Event::RunError(_)))
                .map(|(i, _)| i)
                .next_back()
                .expect("terminal event must exist");

            // Examine events before the terminal event.
            let events_before_terminal = &all_output[..terminal_pos];
            let step_starts: Vec<_> = events_before_terminal
                .iter()
                .filter_map(|e| {
                    if let Event::StepStarted(s) = e {
                        Some(s.step_name.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let step_ends: Vec<_> = events_before_terminal
                .iter()
                .filter_map(|e| {
                    if let Event::StepFinished(s) = e {
                        Some(s.step_name.clone())
                    } else {
                        None
                    }
                })
                .collect();

            // Every step that was started must have been finished before the terminal.
            prop_assert_eq!(
                step_starts.len(),
                step_ends.len(),
                "Steps opened: {:?}, steps closed: {:?}",
                step_starts,
                step_ends,
            );
        }
    }

    // -- drain / resume integration tests -------------------------------------

    /// Helper: build a minimal ArloBridge whose base_config will never be used
    /// for a real model call (resume path never calls the model).
    fn make_bridge(sessions: Arc<super::super::session::SessionStore>) -> ArloBridge {
        use agent_core::agent::Instructions;
        use agent_core::config::RunConfig;
        use agent_core::error::ModelError;
        use agent_core::model::{Model, ModelProvider};

        struct NullProvider;

        #[async_trait::async_trait]
        impl ModelProvider for NullProvider {
            async fn resolve(&self, _: &str) -> Result<Arc<dyn Model>, ModelError> {
                Err(ModelError::Connection("null provider — tests only".into()))
            }
            fn available_models(&self) -> Vec<String> {
                vec![]
            }
        }

        let agent = agent_core::agent::Agent::builder("test")
            .instructions(Instructions::Static(String::new()))
            .build();

        let base_config = RunConfig::builder(Arc::new(NullProvider), "null-model").build();

        ArloBridge::new(agent, base_config, sessions)
    }

    /// Build a minimal `RunAgentInput` for new-run or resume calls.
    fn make_input(
        thread_id: &str,
        run_id: &str,
        resume: Option<Vec<ag_ui_protocol::ResumeEntry>>,
    ) -> RunAgentInput {
        RunAgentInput {
            thread_id: thread_id.to_string(),
            run_id: run_id.to_string(),
            parent_run_id: None,
            state: serde_json::Value::Null,
            messages: vec![],
            tools: vec![],
            context: vec![],
            forwarded_props: serde_json::Value::Null,
            resume,
        }
    }

    // Task 3.3 — Test 1:
    // Drain over a terminal stream returns Finished; the AG-UI event stream
    // ends with RunFinished. Tested end-to-end via the resume path so the
    // EventEmitter is constructed by the ag_ui_server pipeline.
    #[tokio::test]
    async fn drain_over_terminal_stream_returns_finished() {
        use ag_ui_protocol::{Event, ResumeEntry, ResumeStatus};
        use ag_ui_server::run_agent;
        use agent_core::event::RunEvent;
        use futures::StreamExt;

        let sessions = Arc::new(super::super::session::SessionStore::new());
        let bridge = Arc::new(make_bridge(Arc::clone(&sessions)));

        // Pre-build a parked stream that emits a single terminal RunEvent.
        let parked_stream: super::super::session::RunStream =
            Box::pin(futures::stream::iter(vec![RunEvent::AgentEnd {
                agent: "test".into(),
                output: "done".into(),
                usage: Default::default(),
            }]));

        let (interrupt_tx, interrupt_rx) = tokio::sync::mpsc::channel(1);
        // interrupt_tx is intentionally dropped immediately — no interrupt will fire.
        drop(interrupt_tx);

        let parked = super::super::session::ParkedRun {
            stream: parked_stream,
            mapper: EventMapper::new(),
            interrupt_rx,
        };

        // Set up a fully-interrupted session with the parked run.
        let (resume_tx, _resume_rx) = tokio::sync::oneshot::channel();
        sessions.insert(
            "t-finish".into(),
            super::super::session::RunSession {
                run_id: "r1".into(),
                state: super::super::session::SessionState::Interrupted { pending: vec![] },
                created_at: std::time::Instant::now(),
                last_active: std::time::Instant::now(),
                resume_tx: Some(resume_tx),
                parked: Some(parked),
            },
        );

        // Resume the parked run — drain should drain the stream to Finished.
        let resume_entry = ResumeEntry {
            interrupt_id: "req-1".into(),
            status: ResumeStatus::Resolved,
            payload: None,
        };
        let input = make_input("t-finish", "r2", Some(vec![resume_entry]));
        let events: Vec<Event> = run_agent(bridge, input).collect().await;

        // The stream must end with RunFinished (success), not RunError.
        let last = events.last().expect("at least one event");
        assert!(
            matches!(
                last,
                Event::RunFinished(e)
                if e.outcome == Some(ag_ui_protocol::RunFinishedOutcome::Success)
            ),
            "expected RunFinished(Success), got {last:?}"
        );

        // Session should be cleaned up after a clean finish.
        assert_eq!(
            sessions.len(),
            0,
            "session should be removed after drain finishes"
        );
    }

    // Task 3.3 — Test 1b:
    // Drain over a stream that triggers an interrupt returns Interrupted;
    // the AG-UI outcome is RunFinished(Interrupt) and the session is re-parked.
    #[tokio::test]
    async fn drain_over_interrupt_stream_returns_interrupted() {
        use ag_ui_protocol::{Event, ResumeEntry, ResumeStatus};
        use ag_ui_server::run_agent;
        use agent_core::event::RunEvent;
        use agent_core::next_step::PendingApproval;
        use futures::StreamExt;

        let sessions = Arc::new(super::super::session::SessionStore::new());
        let bridge = Arc::new(make_bridge(Arc::clone(&sessions)));

        // The interrupt channel: send a pending approval to trigger an interrupt.
        let (interrupt_tx, interrupt_rx) = tokio::sync::mpsc::channel::<Vec<PendingApproval>>(1);

        // The parked stream blocks forever — the interrupt arrives on interrupt_rx.
        let parked_stream: super::super::session::RunStream =
            Box::pin(futures::stream::pending::<RunEvent>());

        let parked = super::super::session::ParkedRun {
            stream: parked_stream,
            mapper: EventMapper::new(),
            interrupt_rx,
        };

        let (resume_tx, _resume_rx) = tokio::sync::oneshot::channel();
        sessions.insert(
            "t-interrupt".into(),
            super::super::session::RunSession {
                run_id: "r1".into(),
                state: super::super::session::SessionState::Interrupted { pending: vec![] },
                created_at: std::time::Instant::now(),
                last_active: std::time::Instant::now(),
                resume_tx: Some(resume_tx),
                parked: Some(parked),
            },
        );

        // Spawn a task to send the interrupt signal after a short delay so the
        // drain loop is already running by the time it arrives.
        let approval = PendingApproval {
            tool_name: "shell".into(),
            tool_input: serde_json::json!({"cmd": "ls"}),
            request_id: "approval-tc-1".into(),
        };
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = interrupt_tx.send(vec![approval]).await;
        });

        let resume_entry = ResumeEntry {
            interrupt_id: "req-1".into(),
            status: ResumeStatus::Resolved,
            payload: None,
        };
        let input = make_input("t-interrupt", "r2", Some(vec![resume_entry]));
        let events: Vec<Event> = run_agent(bridge, input).collect().await;

        // The stream should end with RunFinished(Interrupt { .. }).
        let last = events.last().expect("at least one event");
        assert!(
            matches!(
                last,
                Event::RunFinished(e)
                if matches!(&e.outcome, Some(ag_ui_protocol::RunFinishedOutcome::Interrupt { interrupts }) if !interrupts.is_empty())
            ),
            "expected RunFinished(Interrupt), got {last:?}"
        );

        // The session should be re-parked (kept alive for the next resume).
        assert_eq!(sessions.len(), 1, "session should remain for re-park");
    }

    // Task 3.3 — Test 2:
    // Resume with a thread_id not in the store returns an error containing
    // "No suspended run".
    #[tokio::test]
    async fn resume_unknown_thread_returns_error() {
        use ag_ui_protocol::{Event, ResumeEntry, ResumeStatus};
        use ag_ui_server::run_agent;
        use futures::StreamExt;

        let sessions = Arc::new(super::super::session::SessionStore::new());
        let bridge = Arc::new(make_bridge(sessions));

        let resume_entry = ResumeEntry {
            interrupt_id: "req-x".into(),
            status: ResumeStatus::Resolved,
            payload: None,
        };
        let input = make_input("unknown-thread", "r1", Some(vec![resume_entry]));
        let events: Vec<Event> = run_agent(bridge, input).collect().await;

        // The pipeline wraps agent errors as RunError events.
        let last = events.last().expect("at least one event");
        match last {
            Event::RunError(e) => {
                assert!(
                    e.message.contains("No suspended run"),
                    "expected 'No suspended run' in error message, got: {:?}",
                    e.message
                );
            }
            other => panic!("expected RunError, got {other:?}"),
        }
    }

    // Task 3.3 — Test 3:
    // Events produced by the parked stream after resume are delivered to the
    // second emitter (the resume request's event stream), not lost.
    //
    // We verify this by:
    //  1. Manually seeding a session with a parked stream that emits
    //     [TextDelta("after-resume"), AgentEnd]
    //  2. Calling the bridge's resume path via run_agent
    //  3. Asserting the collected event stream contains the text content and
    //     ends with RunFinished (i.e., events reach the second emitter)
    #[tokio::test]
    async fn park_then_resume_delivers_to_second_emitter() {
        use ag_ui_protocol::{Event, ResumeEntry, ResumeStatus};
        use ag_ui_server::run_agent;
        use agent_core::event::RunEvent;
        use agent_core::stream::StreamChunk;
        use futures::StreamExt;

        let sessions = Arc::new(super::super::session::SessionStore::new());
        let bridge = Arc::new(make_bridge(Arc::clone(&sessions)));

        // Build a stream that emits a text delta then terminates.
        let parked_events = vec![
            RunEvent::StreamChunk(StreamChunk::TextDelta {
                text: "after-resume".into(),
            }),
            RunEvent::AgentEnd {
                agent: "test".into(),
                output: "after-resume".into(),
                usage: Default::default(),
            },
        ];
        let parked_stream: super::super::session::RunStream =
            Box::pin(futures::stream::iter(parked_events));

        let (interrupt_tx, interrupt_rx) = tokio::sync::mpsc::channel(1);
        // No interrupt will fire — drop the sender.
        drop(interrupt_tx);

        let parked = super::super::session::ParkedRun {
            stream: parked_stream,
            mapper: EventMapper::new(),
            interrupt_rx,
        };

        // Seed the session store as if the first HTTP request already parked here.
        let (resume_tx, _resume_rx) = tokio::sync::oneshot::channel();
        sessions.insert(
            "t-resume".into(),
            super::super::session::RunSession {
                run_id: "r1".into(),
                state: super::super::session::SessionState::Interrupted { pending: vec![] },
                created_at: std::time::Instant::now(),
                last_active: std::time::Instant::now(),
                resume_tx: Some(resume_tx),
                parked: Some(parked),
            },
        );

        // Second HTTP request: resume. This is the "second emitter".
        let resume_entry = ResumeEntry {
            interrupt_id: "req-1".into(),
            status: ResumeStatus::Resolved,
            payload: None,
        };
        let input = make_input("t-resume", "r2", Some(vec![resume_entry]));
        let events: Vec<Event> = run_agent(bridge, input).collect().await;

        // The text content "after-resume" must appear in the second emitter's stream.
        let has_text_content = events
            .iter()
            .any(|e| matches!(e, Event::TextMessageContent(c) if c.delta == "after-resume"));
        assert!(
            has_text_content,
            "expected 'after-resume' text content in resume stream; events: {events:?}"
        );

        // Stream must end with RunFinished (not RunError or missing terminal).
        let last = events.last().expect("at least one event");
        assert!(
            matches!(last, Event::RunFinished(_)),
            "expected RunFinished, got {last:?}"
        );

        // Session cleaned up after successful drain.
        assert_eq!(
            sessions.len(),
            0,
            "session should be removed after clean drain"
        );
    }

    // -- ArloBridge tests ------------------------------------------------------

    // Task 4.2 — interrupt payload fields
    #[test]
    fn interrupt_fields_from_pending_approval() {
        let pending = vec![PendingApproval {
            tool_name: "shell".into(),
            tool_input: serde_json::json!({"command": "ls -la"}),
            request_id: "approval-tc_7".into(),
        }];
        let interrupts = pending_to_interrupts(pending);
        assert_eq!(interrupts.len(), 1);
        let i = &interrupts[0];

        // id retains the full prefix
        assert_eq!(i.id, "approval-tc_7");

        // reason is "tool_call"
        assert_eq!(i.reason, "tool_call");

        // tool_call_id strips the prefix
        assert_eq!(i.tool_call_id, Some("tc_7".to_string()));

        // message is Some and non-empty
        assert!(i.message.is_some());
        assert!(!i.message.as_ref().unwrap().is_empty());

        // metadata carries toolName and toolInput
        let metadata = i.metadata.as_ref().expect("metadata must be present");
        assert_eq!(
            metadata.get("toolName").and_then(|v| v.as_str()),
            Some("shell")
        );
        assert!(
            metadata.contains_key("toolInput"),
            "metadata must contain toolInput"
        );

        // expires_at parses as RFC 3339
        let expires_at = i.expires_at.as_ref().expect("expires_at must be present");
        assert!(
            chrono::DateTime::parse_from_rfc3339(expires_at).is_ok(),
            "expires_at must be valid RFC 3339: {expires_at}"
        );
    }

    #[test]
    fn convert_messages_user_text() {
        use ag_ui_protocol::message::{UserContent, UserMessage};
        use ag_ui_protocol::Message as AgMsg;

        let msgs = vec![AgMsg::User(UserMessage {
            id: "m1".into(),
            content: UserContent::Text("hello".into()),
            name: None,
            encrypted_value: None,
        })];

        let input = ArloBridge::convert_messages(&msgs);
        match input {
            Input::Items { messages } => {
                assert_eq!(messages.len(), 1);
                assert_eq!(
                    messages[0],
                    ArloMessage::User {
                        content: vec![ContentBlock::Text {
                            text: "hello".into()
                        }]
                    }
                );
            }
            other => panic!("expected Items, got {other:?}"),
        }
    }

    #[test]
    fn convert_messages_system_and_developer() {
        use ag_ui_protocol::message::{DeveloperMessage, SystemMessage};
        use ag_ui_protocol::Message as AgMsg;

        let msgs = vec![
            AgMsg::System(SystemMessage {
                id: "s1".into(),
                content: "sys".into(),
                name: None,
                encrypted_value: None,
            }),
            AgMsg::Developer(DeveloperMessage {
                id: "d1".into(),
                content: "dev".into(),
                name: None,
                encrypted_value: None,
            }),
        ];

        let input = ArloBridge::convert_messages(&msgs);
        match input {
            Input::Items { messages } => {
                assert_eq!(messages.len(), 2);
                assert_eq!(
                    messages[0],
                    ArloMessage::System {
                        content: "sys".into()
                    }
                );
                assert_eq!(
                    messages[1],
                    ArloMessage::System {
                        content: "dev".into()
                    }
                );
            }
            other => panic!("expected Items, got {other:?}"),
        }
    }

    #[test]
    fn convert_messages_empty_returns_fresh() {
        let input = ArloBridge::convert_messages(&[]);
        assert!(matches!(input, Input::Fresh { .. }));
    }

    #[test]
    fn convert_messages_skips_tool_messages() {
        use ag_ui_protocol::message::{ToolMessage, UserContent, UserMessage};
        use ag_ui_protocol::Message as AgMsg;

        let msgs = vec![
            AgMsg::Tool(ToolMessage {
                id: "t1".into(),
                content: "result".into(),
                tool_call_id: "tc1".into(),
                error: None,
                encrypted_value: None,
            }),
            AgMsg::User(UserMessage {
                id: "m1".into(),
                content: UserContent::Text("hi".into()),
                name: None,
                encrypted_value: None,
            }),
        ];

        let input = ArloBridge::convert_messages(&msgs);
        match input {
            Input::Items { messages } => {
                // Only the user message, tool is skipped
                assert_eq!(messages.len(), 1);
            }
            other => panic!("expected Items, got {other:?}"),
        }
    }

    #[test]
    fn convert_messages_assistant_empty_content_skipped() {
        use ag_ui_protocol::message::AssistantMessage;
        use ag_ui_protocol::Message as AgMsg;

        let msgs = vec![AgMsg::Assistant(AssistantMessage {
            id: "a1".into(),
            content: None,
            name: None,
            tool_calls: None,
            encrypted_value: None,
        })];

        let input = ArloBridge::convert_messages(&msgs);
        // Empty assistant content → skipped → falls back to Fresh
        assert!(matches!(input, Input::Fresh { .. }));
    }
}
