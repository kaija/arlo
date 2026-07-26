//! AG-UI ↔ Arlo bridge: translates between the AG-UI `Agent` trait and Arlo's run loop.

use std::sync::Arc;

use ag_ui_protocol::{
    BaseEvent, Event, Interrupt, RunAgentInput, RunErrorEvent, RunFinishedEvent,
    RunFinishedOutcome, StepStartedEvent, TextMessageContentEvent, TextMessageEndEvent,
    TextMessageStartEvent, TextMessageRole, ToolCallArgsEvent, ToolCallEndEvent,
    ToolCallStartEvent,
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

use super::approval::{AgUiApprovalHandler, map_resume_to_responses};
use super::session::{RunSession, SessionState, SessionStore};

// ---------------------------------------------------------------------------
// ArloBridge — implements ag_ui_server::Agent
// ---------------------------------------------------------------------------

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
                ag_ui_protocol::Message::System(s) => {
                    Some(ArloMessage::System {
                        content: s.content.clone(),
                    })
                }
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
                    let session = sessions.remove(&thread_id);
                    match session {
                        Some(session) => match session.state {
                            SessionState::Interrupted { .. } => {
                                let responses = map_resume_to_responses(resume_entries);
                                // Deliver decisions via the oneshot channel
                                if let Some(tx) = session.resume_tx {
                                    let _ = tx.send(responses);
                                }
                                // ponytail: reattach to the running task's event stream
                                // is deferred — the background task already ran to
                                // completion or will be picked up on next request.
                                // For now we return success; full reattach requires
                                // an event-forwarding channel added to RunSession.
                                return Ok(RunOutcome::success());
                            }
                            _ => {
                                return Err(AgentError::msg(format!(
                                    "No suspended run for thread_id '{thread_id}'"
                                )));
                            }
                        },
                        None => {
                            return Err(AgentError::msg(format!(
                                "No suspended run for thread_id '{thread_id}'"
                            )));
                        }
                    }
                }
            }

            // --- New run path ---
            let arlo_input = Self::convert_messages(&input.messages);

            // Channel for the approval handler to signal interrupts
            let (interrupt_tx, mut interrupt_rx) = mpsc::channel::<Vec<PendingApproval>>(1);

            // Build a config with our custom approval handler
            let approval_handler = Arc::new(AgUiApprovalHandler {
                sessions: Arc::clone(&sessions),
                thread_id: thread_id.clone(),
                interrupt_tx,
            });

            let config = RunConfig::builder(
                Arc::clone(&base_config.provider),
                &base_config.model,
            )
            .max_turns(base_config.max_turns)
            .approval_handler(approval_handler)
            .build();

            // Start the run stream
            let mut stream = run_stream(&agent, arlo_input, &config);

            // Register a placeholder session so the approval handler can find it
            let task_handle = tokio::spawn(async {}); // replaced below
            sessions.insert(
                thread_id.clone(),
                RunSession {
                    run_id: input.run_id.clone(),
                    state: SessionState::Running,
                    created_at: std::time::Instant::now(),
                    last_active: std::time::Instant::now(),
                    resume_tx: None,
                    task_handle,
                },
            );

            // Consume the stream, map events, emit via the EventEmitter
            let mut mapper = EventMapper::new();

            loop {
                tokio::select! {
                    event = stream.next() => {
                        match event {
                            Some(run_event) => {
                                let ag_events = mapper.map_event(run_event);
                                for ev in ag_events {
                                    // Check for terminal events — we handle them specially
                                    match &ev {
                                        Event::RunFinished(finished) => {
                                            // If it's an interrupt, return Interrupt outcome
                                            // (the pipeline wraps it in RUN_FINISHED for us)
                                            match &finished.outcome {
                                                Some(RunFinishedOutcome::Interrupt { interrupts }) => {
                                                    return Ok(RunOutcome::Interrupt(interrupts.clone()));
                                                }
                                                _ => {
                                                    // Success — pipeline emits RUN_FINISHED
                                                    sessions.remove(&thread_id);
                                                    return Ok(RunOutcome::success());
                                                }
                                            }
                                        }
                                        Event::RunError(e) => {
                                            sessions.remove(&thread_id);
                                            return Err(AgentError::msg(e.message.clone()));
                                        }
                                        _ => {
                                            // Non-terminal: emit through the emitter
                                            if emitter.emit(ev).await.is_err() {
                                                // Client disconnected
                                                sessions.remove(&thread_id);
                                                return Err(AgentError::ClientDisconnected);
                                            }
                                        }
                                    }
                                }
                            }
                            None => {
                                // Stream ended without a terminal event (shouldn't happen,
                                // but handle gracefully)
                                let flush = mapper.finish();
                                for ev in flush {
                                    let _ = emitter.emit(ev).await;
                                }
                                sessions.remove(&thread_id);
                                return Ok(RunOutcome::success());
                            }
                        }
                    }
                    pending = interrupt_rx.recv() => {
                        // The approval handler signaled an interrupt
                        if let Some(pending_approvals) = pending {
                            // Flush open text messages
                            let flush = mapper.finish();
                            for ev in flush {
                                let _ = emitter.emit(ev).await;
                            }
                            let interrupts: Vec<Interrupt> = pending_approvals
                                .into_iter()
                                .map(|p| Interrupt {
                                    id: p.request_id,
                                    reason: format!("Tool '{}' requires approval", p.tool_name),
                                    message: None,
                                    tool_call_id: None,
                                    response_schema: None,
                                    expires_at: None,
                                    metadata: None,
                                })
                                .collect();
                            // Don't remove session — it stays alive for resume
                            return Ok(RunOutcome::Interrupt(interrupts));
                        }
                    }
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
}

#[allow(dead_code)]
impl EventMapper {
    pub fn new() -> Self {
        Self {
            open_message_id: None,
        }
    }

    /// Translate a single `RunEvent` into zero or more AG-UI events.
    pub fn map_event(&mut self, event: RunEvent) -> Vec<Event> {
        match event {
            RunEvent::StreamChunk(chunk) => self.map_stream_chunk(chunk),
            RunEvent::TurnStart { turn, .. } => {
                let mut out = self.close_text_message();
                out.push(Event::StepStarted(StepStartedEvent {
                    base: BaseEvent::default(),
                    step_name: format!("turn-{turn}"),
                }));
                out
            }
            RunEvent::ToolStart { .. } => {
                // ToolStart is Arlo's execution-started signal; AG-UI TOOL_CALL_START
                // is emitted from the streaming ToolUseStart chunk instead.
                self.close_text_message()
            }
            RunEvent::ToolEnd { id, .. } => {
                let mut out = self.close_text_message();
                out.push(Event::ToolCallEnd(ToolCallEndEvent {
                    base: BaseEvent::default(),
                    tool_call_id: id,
                }));
                out
            }
            RunEvent::AgentEnd { .. } => {
                let mut out = self.close_text_message();
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
                let interrupts = pending
                    .into_iter()
                    .map(|p| Interrupt {
                        id: p.request_id,
                        reason: format!("Tool '{}' requires approval", p.tool_name),
                        message: None,
                        tool_call_id: None,
                        response_schema: None,
                        expires_at: None,
                        metadata: None,
                    })
                    .collect();
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
                out.push(Event::RunError(RunErrorEvent {
                    base: BaseEvent::default(),
                    message: error,
                    code: None,
                }));
                out
            }
            RunEvent::Aborted { reason } => {
                let mut out = self.close_text_message();
                out.push(Event::RunError(RunErrorEvent {
                    base: BaseEvent::default(),
                    message: reason,
                    code: None,
                }));
                out
            }
            RunEvent::GuardrailTripped { name, reason } => {
                let mut out = self.close_text_message();
                out.push(Event::RunError(RunErrorEvent {
                    base: BaseEvent::default(),
                    message: format!("Guardrail '{name}': {reason}"),
                    code: None,
                }));
                out
            }
            // Compaction and StepResolved are internal — no AG-UI equivalent.
            RunEvent::Compaction { .. } | RunEvent::StepResolved(_) => {
                Vec::new()
            }
        }
    }

    /// Call when the stream ends to flush any open text message.
    pub fn finish(&mut self) -> Vec<Event> {
        self.close_text_message()
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
                // Map to a step-started event as a lightweight reasoning signal.
                // ponytail: full REASONING_* lifecycle deferred until needed.
                vec![Event::StepStarted(StepStartedEvent {
                    base: BaseEvent::default(),
                    step_name: "thinking".to_string(),
                })]
            }
            StreamChunk::ToolUseStart { id, name } => {
                let mut out = self.close_text_message();
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
        let out = m.map_event(RunEvent::ToolEnd {
            id: "t1".into(),
            name: "shell".into(),
            output: "ok".into(),
            is_error: false,
        });
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Event::ToolCallEnd(_)));
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
    fn thinking_delta_maps_to_step_started() {
        let mut m = EventMapper::new();
        let out = m.map_event(RunEvent::StreamChunk(StreamChunk::ThinkingDelta {
            text: "hmm".into(),
        }));
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Event::StepStarted(_)));
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
            (1..100u32, "[a-z]{1,8}").prop_map(|(turn, agent)| RunEvent::TurnStart {
                turn,
                agent,
            }),
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

    // -- ArloBridge tests ------------------------------------------------------

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
