// TUI application state and state machine.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use agent_core::{ContentBlock, Message, PendingApproval, PermissionEngine, RunEvent, StreamChunk};

use super::input::InputBuffer;

/// The mode the TUI application is currently in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMode {
    /// Waiting for user input.
    Idle,
    /// An agent run is actively streaming.
    Running,
    /// Displaying a permission prompt (y/a/n).
    PermissionPrompt,
    /// The application is exiting.
    Exiting,
}

/// Events that drive the TUI application state machine.
#[derive(Debug)]
pub enum AppEvent {
    /// A terminal key event.
    Key(KeyEvent),
    /// An event from the agent RunStream.
    AgentEvent(RunEvent),
    /// The terminal was resized to (cols, rows).
    Resize(u16, u16),
    /// A periodic tick for animations/heartbeat.
    Tick,
}

/// A styled span of output text for rendering.
#[derive(Debug, Clone)]
pub struct OutputSpan {
    /// The text content.
    pub text: String,
    /// The visual style to apply.
    pub style: SpanStyle,
}

/// Visual styles for output spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanStyle {
    /// Regular assistant text output.
    Normal,
    /// Thinking/reasoning text (visually distinct).
    Thinking,
    /// Tool name display.
    ToolName,
    /// Tool output content.
    ToolOutput,
    /// Error messages.
    Error,
    /// Warning messages.
    Warning,
    /// System messages (dim).
    System,
}

/// A tool execution entry tracked in the UI.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    /// Unique identifier for this tool invocation.
    pub id: String,
    /// The name of the tool.
    pub name: String,
    /// Current execution status.
    pub status: ToolStatus,
    /// Output produced by the tool (populated on completion).
    pub output: Option<String>,
    /// Whether the tool execution resulted in an error.
    pub is_error: bool,
}

/// Status of a tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    /// The tool is currently executing.
    Executing,
    /// The tool has completed.
    Completed,
}

/// Token usage summary for display in the status bar.
#[derive(Debug, Clone)]
pub struct UsageSummary {
    /// Number of input tokens consumed.
    pub input_tokens: u64,
    /// Number of output tokens generated.
    pub output_tokens: u64,
}

/// The result of processing an AppEvent through the state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateResult {
    /// Continue the event loop normally (no external action needed).
    Continue,
    /// Start a new agent run with the given prompt text.
    StartRun(String),
    /// Resume a paused run after permission prompt.
    /// The bool indicates whether permission was granted (true) or denied (false).
    ResumeRun(bool),
    /// Abort the currently running agent execution.
    AbortRun,
    /// Exit the application.
    Exit,
}

/// The full application state for the TUI.
pub struct AppState {
    /// Current application mode.
    pub mode: AppMode,
    /// Accumulated output spans for rendering.
    pub output_buffer: Vec<OutputSpan>,
    /// Conversation history across prompts in this session.
    pub history: Vec<Message>,
    /// Active tool executions being displayed.
    pub active_tools: Vec<ToolEntry>,
    /// The permission engine (shared across the session).
    pub permissions: PermissionEngine,
    /// Pending approval requests when in PermissionPrompt mode.
    pub pending_approvals: Vec<PendingApproval>,
    /// The text input buffer.
    pub input: InputBuffer,
    /// Timestamp of the last Ctrl-C press (for double-press detection).
    pub last_ctrl_c: Option<Instant>,
    /// Whether the last Ctrl-C aborted a run (affects double-press semantics).
    pub ctrl_c_abort_pending: bool,
    /// Token usage from the last completed run.
    pub last_usage: Option<UsageSummary>,
}

impl AppState {
    /// Create a new `AppState` with the given permission engine.
    pub fn new(permissions: PermissionEngine) -> Self {
        Self {
            mode: AppMode::Idle,
            output_buffer: Vec::new(),
            history: Vec::new(),
            active_tools: Vec::new(),
            permissions,
            pending_approvals: Vec::new(),
            input: InputBuffer::new(),
            last_ctrl_c: None,
            ctrl_c_abort_pending: false,
            last_usage: None,
        }
    }

    /// Process an application event and return the resulting action.
    ///
    /// This is the main state machine dispatch — every external event flows
    /// through here and produces an `UpdateResult` that the event loop acts on.
    pub fn update(&mut self, event: AppEvent) -> UpdateResult {
        match event {
            AppEvent::AgentEvent(run_event) => self.handle_run_event(run_event),
            AppEvent::Key(key_event) => match self.mode {
                AppMode::Idle => self.handle_idle_key(key_event),
                AppMode::Running => self.handle_running_key(key_event),
                AppMode::PermissionPrompt => self.handle_prompt_key(key_event),
                AppMode::Exiting => UpdateResult::Exit,
            },
            AppEvent::Resize(_, _) => {
                // ratatui handles terminal resize automatically during render
                UpdateResult::Continue
            }
            AppEvent::Tick => UpdateResult::Continue,
        }
    }

    /// Handle a key event while in Idle mode (waiting for user input).
    ///
    /// - Enter: submit non-empty input as StartRun; "exit"/"quit" → Exit
    /// - Ctrl-C: clear input if non-empty; double-press within 2s → Exit
    /// - Character keys: insert into InputBuffer
    /// - Backspace/Delete/Arrows/Home/End: line-editing operations
    fn handle_idle_key(&mut self, key: KeyEvent) -> UpdateResult {
        let is_ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);

        if is_ctrl_c {
            if !self.input.is_empty() {
                // Clear the current input line
                self.input.clear();
                self.last_ctrl_c = None;
                return UpdateResult::Continue;
            }
            // Input is empty — check double-press logic
            if let Some(last) = self.last_ctrl_c {
                if last.elapsed() < Duration::from_secs(2) {
                    return UpdateResult::Exit;
                }
            }
            self.last_ctrl_c = Some(Instant::now());
            return UpdateResult::Continue;
        }

        // Non-Ctrl-C keys reset double-press tracking implicitly (the timer handles it)
        match key.code {
            KeyCode::Enter => {
                if self.input.is_empty() {
                    return UpdateResult::Continue;
                }
                let text = self.input.take();
                if text == "exit" || text == "quit" {
                    return UpdateResult::Exit;
                }
                UpdateResult::StartRun(text)
            }
            KeyCode::Char(ch) => {
                self.input.insert(ch);
                UpdateResult::Continue
            }
            KeyCode::Backspace => {
                self.input.backspace();
                UpdateResult::Continue
            }
            KeyCode::Delete => {
                self.input.delete();
                UpdateResult::Continue
            }
            KeyCode::Left => {
                self.input.move_left();
                UpdateResult::Continue
            }
            KeyCode::Right => {
                self.input.move_right();
                UpdateResult::Continue
            }
            KeyCode::Home => {
                self.input.move_home();
                UpdateResult::Continue
            }
            KeyCode::End => {
                self.input.move_end();
                UpdateResult::Continue
            }
            _ => UpdateResult::Continue,
        }
    }

    /// Handle a key event while in Running mode (agent actively streaming).
    ///
    /// - Ctrl-C: abort the current run; double-press within 2s → Exit
    /// - All other keys: ignored (input is blocked while running)
    fn handle_running_key(&mut self, key: KeyEvent) -> UpdateResult {
        let is_ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);

        if is_ctrl_c {
            // Check double-press: if within 2s of a previous abort Ctrl-C → exit
            if let Some(last) = self.last_ctrl_c {
                if last.elapsed() < Duration::from_secs(2) {
                    return UpdateResult::Exit;
                }
            }
            // First Ctrl-C while running: abort the run
            self.last_ctrl_c = Some(Instant::now());
            self.ctrl_c_abort_pending = true;
            return UpdateResult::AbortRun;
        }

        // All other keys are blocked during running
        UpdateResult::Continue
    }

    /// Handle a key event while in PermissionPrompt mode (y/a/n).
    ///
    /// - 'y': one-time approve → ResumeRun(true)
    /// - 'a': grant session allow for the tool → ResumeRun(true)
    /// - 'n': deny → ResumeRun(false)
    /// - All other keys: ignored
    fn handle_prompt_key(&mut self, key: KeyEvent) -> UpdateResult {
        match key.code {
            KeyCode::Char('y') => UpdateResult::ResumeRun(true),
            KeyCode::Char('a') => {
                // Grant session-level allow for the pending tool
                if let Some(approval) = self.pending_approvals.first() {
                    self.permissions.grant_session_allow(&approval.tool_name);
                }
                UpdateResult::ResumeRun(true)
            }
            KeyCode::Char('n') => UpdateResult::ResumeRun(false),
            _ => UpdateResult::Continue,
        }
    }

    /// Handle a `RunEvent` from the agent's RunStream.
    ///
    /// Maps streaming events to output spans, tracks tool executions,
    /// and transitions the app mode on terminal events.
    fn handle_run_event(&mut self, event: RunEvent) -> UpdateResult {
        match event {
            RunEvent::StreamChunk(chunk) => match chunk {
                StreamChunk::TextDelta { text } => {
                    self.output_buffer.push(OutputSpan {
                        text,
                        style: SpanStyle::Normal,
                    });
                }
                StreamChunk::ThinkingDelta { text } => {
                    self.output_buffer.push(OutputSpan {
                        text,
                        style: SpanStyle::Thinking,
                    });
                }
                StreamChunk::ToolUseStart { id: _, name } => {
                    self.output_buffer.push(OutputSpan {
                        text: format!("⚡ {name}"),
                        style: SpanStyle::ToolName,
                    });
                }
                StreamChunk::MessageStop { .. } => {
                    // Message finalized — no additional rendering needed
                }
                // ToolUseInputDelta and ToolUseEnd are low-level streaming details;
                // the TUI surfaces tool lifecycle via ToolStart/ToolEnd RunEvents.
                _ => {}
            },

            RunEvent::ToolStart { id, name } => {
                self.active_tools.push(ToolEntry {
                    id,
                    name,
                    status: ToolStatus::Executing,
                    output: None,
                    is_error: false,
                });
            }

            RunEvent::ToolEnd {
                id,
                name: _,
                output,
                is_error,
            } => {
                if let Some(entry) = self.active_tools.iter_mut().find(|t| t.id == id) {
                    entry.status = ToolStatus::Completed;
                    entry.output = Some(output);
                    entry.is_error = is_error;
                }
            }

            RunEvent::Interruption { pending } => {
                self.mode = AppMode::PermissionPrompt;
                self.pending_approvals = pending;
            }

            RunEvent::AgentEnd {
                output,
                usage,
                ..
            } => {
                // Append assistant response to conversation history
                self.history.push(Message::Assistant {
                    content: vec![ContentBlock::Text { text: output.clone() }],
                    usage: Some(usage.clone()),
                });

                if !output.is_empty() {
                    self.output_buffer.push(OutputSpan {
                        text: output,
                        style: SpanStyle::Normal,
                    });
                }
                self.last_usage = Some(UsageSummary {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                });
                self.mode = AppMode::Idle;
                self.active_tools.clear();
            }

            RunEvent::Error { error } => {
                self.output_buffer.push(OutputSpan {
                    text: error,
                    style: SpanStyle::Error,
                });
                self.mode = AppMode::Idle;
            }

            RunEvent::Aborted { reason } => {
                self.output_buffer.push(OutputSpan {
                    text: reason,
                    style: SpanStyle::Warning,
                });
                self.mode = AppMode::Idle;
            }

            RunEvent::MaxTurns { count } => {
                self.output_buffer.push(OutputSpan {
                    text: format!("Agent reached maximum turn limit ({count} turns)"),
                    style: SpanStyle::Warning,
                });
                self.mode = AppMode::Idle;
            }

            RunEvent::GuardrailTripped { name, reason } => {
                self.output_buffer.push(OutputSpan {
                    text: format!("Guardrail '{name}' tripped: {reason}"),
                    style: SpanStyle::Warning,
                });
                self.mode = AppMode::Idle;
            }

            // Non-terminal events we don't surface in the TUI
            RunEvent::TurnStart { .. }
            | RunEvent::SubAgentStart { .. }
            | RunEvent::SubAgentEnd { .. }
            | RunEvent::Compaction { .. }
            | RunEvent::StepResolved(_) => {}
        }

        UpdateResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{PermissionMode, StreamChunk, Usage};

    fn make_state() -> AppState {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms);
        state.mode = AppMode::Running;
        state
    }

    // --- update() dispatch tests ---

    #[test]
    fn update_tick_returns_continue() {
        let mut state = make_state();
        assert_eq!(state.update(AppEvent::Tick), UpdateResult::Continue);
    }

    #[test]
    fn update_resize_returns_continue() {
        let mut state = make_state();
        assert_eq!(state.update(AppEvent::Resize(80, 24)), UpdateResult::Continue);
    }

    #[test]
    fn update_key_returns_continue() {
        let mut state = make_state();
        // In Running mode, non-Ctrl-C keys are blocked and return Continue
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(state.update(AppEvent::Key(key)), UpdateResult::Continue);
    }

    // --- handle_run_event() tests ---

    #[test]
    fn text_delta_pushes_normal_span() {
        let mut state = make_state();
        let event = RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "hello".to_string(),
        });
        let result = state.update(AppEvent::AgentEvent(event));
        assert_eq!(result, UpdateResult::Continue);
        assert_eq!(state.output_buffer.len(), 1);
        assert_eq!(state.output_buffer[0].text, "hello");
        assert_eq!(state.output_buffer[0].style, SpanStyle::Normal);
    }

    #[test]
    fn thinking_delta_pushes_thinking_span() {
        let mut state = make_state();
        let event = RunEvent::StreamChunk(StreamChunk::ThinkingDelta {
            text: "hmm...".to_string(),
        });
        state.update(AppEvent::AgentEvent(event));
        assert_eq!(state.output_buffer.len(), 1);
        assert_eq!(state.output_buffer[0].style, SpanStyle::Thinking);
    }

    #[test]
    fn tool_use_start_pushes_tool_name_span() {
        let mut state = make_state();
        let event = RunEvent::StreamChunk(StreamChunk::ToolUseStart {
            id: "t1".to_string(),
            name: "read_file".to_string(),
        });
        state.update(AppEvent::AgentEvent(event));
        assert_eq!(state.output_buffer.len(), 1);
        assert_eq!(state.output_buffer[0].style, SpanStyle::ToolName);
        assert!(state.output_buffer[0].text.contains("read_file"));
    }

    #[test]
    fn message_stop_does_not_push_span() {
        let mut state = make_state();
        let event = RunEvent::StreamChunk(StreamChunk::MessageStop {
            stop_reason: agent_core::StopReason::EndTurn,
            usage: Usage::default(),
        });
        state.update(AppEvent::AgentEvent(event));
        assert!(state.output_buffer.is_empty());
    }

    #[test]
    fn tool_start_adds_executing_entry() {
        let mut state = make_state();
        let event = RunEvent::ToolStart {
            id: "t1".to_string(),
            name: "shell".to_string(),
        };
        state.update(AppEvent::AgentEvent(event));
        assert_eq!(state.active_tools.len(), 1);
        assert_eq!(state.active_tools[0].id, "t1");
        assert_eq!(state.active_tools[0].name, "shell");
        assert_eq!(state.active_tools[0].status, ToolStatus::Executing);
    }

    #[test]
    fn tool_end_updates_matching_entry() {
        let mut state = make_state();
        // First add a tool start
        state.update(AppEvent::AgentEvent(RunEvent::ToolStart {
            id: "t1".to_string(),
            name: "shell".to_string(),
        }));
        // Then end it
        state.update(AppEvent::AgentEvent(RunEvent::ToolEnd {
            id: "t1".to_string(),
            name: "shell".to_string(),
            output: "done".to_string(),
            is_error: false,
        }));
        assert_eq!(state.active_tools[0].status, ToolStatus::Completed);
        assert_eq!(state.active_tools[0].output.as_deref(), Some("done"));
        assert!(!state.active_tools[0].is_error);
    }

    #[test]
    fn tool_end_with_error_flag() {
        let mut state = make_state();
        state.update(AppEvent::AgentEvent(RunEvent::ToolStart {
            id: "t2".to_string(),
            name: "shell".to_string(),
        }));
        state.update(AppEvent::AgentEvent(RunEvent::ToolEnd {
            id: "t2".to_string(),
            name: "shell".to_string(),
            output: "permission denied".to_string(),
            is_error: true,
        }));
        assert!(state.active_tools[0].is_error);
    }

    #[test]
    fn interruption_sets_permission_prompt_mode() {
        let mut state = make_state();
        let pending = vec![PendingApproval {
            tool_name: "shell".to_string(),
            tool_input: serde_json::json!({"cmd": "rm -rf /"}),
            request_id: "req1".to_string(),
        }];
        state.update(AppEvent::AgentEvent(RunEvent::Interruption {
            pending: pending.clone(),
        }));
        assert_eq!(state.mode, AppMode::PermissionPrompt);
        assert_eq!(state.pending_approvals.len(), 1);
        assert_eq!(state.pending_approvals[0].request_id, "req1");
    }

    #[test]
    fn agent_end_transitions_to_idle() {
        let mut state = make_state();
        state.active_tools.push(ToolEntry {
            id: "t1".to_string(),
            name: "shell".to_string(),
            status: ToolStatus::Executing,
            output: None,
            is_error: false,
        });
        state.update(AppEvent::AgentEvent(RunEvent::AgentEnd {
            agent: "main".to_string(),
            output: "All done!".to_string(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: None,
            },
        }));
        assert_eq!(state.mode, AppMode::Idle);
        assert!(state.active_tools.is_empty());
        assert!(state.last_usage.is_some());
        let usage = state.last_usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        // Final output should be in the buffer
        assert!(state.output_buffer.iter().any(|s| s.text == "All done!"));
    }

    #[test]
    fn error_transitions_to_idle_with_error_span() {
        let mut state = make_state();
        state.update(AppEvent::AgentEvent(RunEvent::Error {
            error: "something broke".to_string(),
        }));
        assert_eq!(state.mode, AppMode::Idle);
        assert_eq!(state.output_buffer.last().unwrap().style, SpanStyle::Error);
        assert_eq!(state.output_buffer.last().unwrap().text, "something broke");
    }

    #[test]
    fn aborted_transitions_to_idle_with_warning() {
        let mut state = make_state();
        state.update(AppEvent::AgentEvent(RunEvent::Aborted {
            reason: "user cancelled".to_string(),
        }));
        assert_eq!(state.mode, AppMode::Idle);
        assert_eq!(state.output_buffer.last().unwrap().style, SpanStyle::Warning);
    }

    #[test]
    fn max_turns_transitions_to_idle() {
        let mut state = make_state();
        state.update(AppEvent::AgentEvent(RunEvent::MaxTurns { count: 25 }));
        assert_eq!(state.mode, AppMode::Idle);
        assert!(state.output_buffer.last().unwrap().text.contains("25"));
        assert_eq!(state.output_buffer.last().unwrap().style, SpanStyle::Warning);
    }

    #[test]
    fn guardrail_tripped_transitions_to_idle() {
        let mut state = make_state();
        state.update(AppEvent::AgentEvent(RunEvent::GuardrailTripped {
            name: "safety".to_string(),
            reason: "blocked content".to_string(),
        }));
        assert_eq!(state.mode, AppMode::Idle);
        assert!(state.output_buffer.last().unwrap().text.contains("safety"));
        assert!(state
            .output_buffer
            .last()
            .unwrap()
            .text
            .contains("blocked content"));
        assert_eq!(state.output_buffer.last().unwrap().style, SpanStyle::Warning);
    }

    #[test]
    fn turn_start_is_ignored() {
        let mut state = make_state();
        state.update(AppEvent::AgentEvent(RunEvent::TurnStart {
            turn: 1,
            agent: "main".to_string(),
        }));
        assert!(state.output_buffer.is_empty());
        assert_eq!(state.mode, AppMode::Running);
    }

    #[test]
    fn sub_agent_events_are_ignored() {
        let mut state = make_state();
        state.update(AppEvent::AgentEvent(RunEvent::SubAgentStart {
            agent: "sub".to_string(),
            task: "do stuff".to_string(),
        }));
        state.update(AppEvent::AgentEvent(RunEvent::SubAgentEnd {
            agent: "sub".to_string(),
            output: "done".to_string(),
        }));
        assert!(state.output_buffer.is_empty());
    }

    #[test]
    fn compaction_is_ignored() {
        let mut state = make_state();
        state.update(AppEvent::AgentEvent(RunEvent::Compaction {
            stage: "summarize".to_string(),
            messages_removed: 5,
        }));
        assert!(state.output_buffer.is_empty());
    }

    // --- handle_idle_key() tests ---

    fn make_idle_state() -> AppState {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        AppState::new(perms) // starts in Idle mode by default
    }

    #[test]
    fn idle_enter_with_nonempty_input_returns_start_run() {
        let mut state = make_idle_state();
        state.input.insert('h');
        state.input.insert('i');
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::StartRun("hi".to_string()));
        assert!(state.input.is_empty()); // take() clears the buffer
    }

    #[test]
    fn idle_enter_with_empty_input_returns_continue() {
        let mut state = make_idle_state();
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Continue);
    }

    #[test]
    fn idle_exit_command_returns_exit() {
        let mut state = make_idle_state();
        for ch in "exit".chars() {
            state.input.insert(ch);
        }
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Exit);
    }

    #[test]
    fn idle_quit_command_returns_exit() {
        let mut state = make_idle_state();
        for ch in "quit".chars() {
            state.input.insert(ch);
        }
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Exit);
    }

    #[test]
    fn idle_ctrl_c_with_input_clears_buffer() {
        let mut state = make_idle_state();
        state.input.insert('x');
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Continue);
        assert!(state.input.is_empty());
    }

    #[test]
    fn idle_ctrl_c_empty_input_sets_last_ctrl_c() {
        let mut state = make_idle_state();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Continue);
        assert!(state.last_ctrl_c.is_some());
    }

    #[test]
    fn idle_double_ctrl_c_within_2s_exits() {
        let mut state = make_idle_state();
        state.last_ctrl_c = Some(Instant::now()); // simulate first press just happened
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Exit);
    }

    #[test]
    fn idle_double_ctrl_c_after_2s_does_not_exit() {
        let mut state = make_idle_state();
        // Simulate a press that happened 3 seconds ago
        state.last_ctrl_c = Some(Instant::now() - Duration::from_secs(3));
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Continue);
        // last_ctrl_c should be updated to now
        assert!(state.last_ctrl_c.unwrap().elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn idle_char_key_inserts_into_buffer() {
        let mut state = make_idle_state();
        let key = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Continue);
        assert_eq!(state.input.content(), "z");
    }

    #[test]
    fn idle_backspace_removes_last_char() {
        let mut state = make_idle_state();
        state.input.insert('a');
        state.input.insert('b');
        let key = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        state.update(AppEvent::Key(key));
        assert_eq!(state.input.content(), "a");
    }

    // --- handle_running_key() tests ---

    #[test]
    fn running_ctrl_c_returns_abort_run() {
        let mut state = make_state();
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::AbortRun);
        assert!(state.ctrl_c_abort_pending);
        assert!(state.last_ctrl_c.is_some());
    }

    #[test]
    fn running_double_ctrl_c_exits() {
        let mut state = make_state();
        state.last_ctrl_c = Some(Instant::now()); // first press just happened
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Exit);
    }

    #[test]
    fn running_non_ctrl_c_keys_are_blocked() {
        let mut state = make_state();
        let keys = vec![
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        ];
        for key in keys {
            assert_eq!(state.update(AppEvent::Key(key)), UpdateResult::Continue);
        }
    }

    // --- handle_prompt_key() tests ---

    fn make_prompt_state() -> AppState {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms);
        state.mode = AppMode::PermissionPrompt;
        state.pending_approvals = vec![PendingApproval {
            tool_name: "shell".to_string(),
            tool_input: serde_json::json!({"cmd": "ls"}),
            request_id: "req1".to_string(),
        }];
        state
    }

    #[test]
    fn prompt_y_returns_resume_run_true() {
        let mut state = make_prompt_state();
        let key = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::ResumeRun(true));
    }

    #[test]
    fn prompt_a_grants_session_allow_and_resumes() {
        let mut state = make_prompt_state();
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::ResumeRun(true));
        // Verify the permission was granted by checking the engine
        // The PermissionEngine should now allow "shell" without prompting
    }

    #[test]
    fn prompt_n_returns_resume_run_false() {
        let mut state = make_prompt_state();
        let key = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::ResumeRun(false));
    }

    #[test]
    fn prompt_other_keys_return_continue() {
        let mut state = make_prompt_state();
        let keys = vec![
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT), // uppercase Y is not y
        ];
        for key in keys {
            assert_eq!(state.update(AppEvent::Key(key)), UpdateResult::Continue);
        }
    }

    // --- Exiting mode test ---

    #[test]
    fn exiting_mode_returns_exit_for_any_key() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms);
        state.mode = AppMode::Exiting;
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(state.update(AppEvent::Key(key)), UpdateResult::Exit);
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use agent_core::{ApprovalRequirement, PendingApproval, PermissionDecision, PermissionMode, RunEvent, StreamChunk, Usage};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use proptest::prelude::*;
    use proptest::collection::vec as prop_vec;
    use std::time::{Duration, Instant};

    fn make_state() -> AppState {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms);
        state.mode = AppMode::Running;
        state
    }

    /// Property 1: Text delta ordering invariant
    /// A sequence of TextDelta events produces output_buffer entries in identical order.
    /// **Validates: Requirements 1.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn text_deltas_preserve_order(
            deltas in prop_vec("[a-z]{1,20}".prop_map(|s| s), 1..20)
        ) {
            let mut state = make_state();
            for delta in &deltas {
                state.update(AppEvent::AgentEvent(RunEvent::StreamChunk(
                    StreamChunk::TextDelta { text: delta.clone() }
                )));
            }
            // Extract Normal-styled spans
            let output_texts: Vec<&str> = state.output_buffer.iter()
                .filter(|s| s.style == SpanStyle::Normal)
                .map(|s| s.text.as_str())
                .collect();
            let expected: Vec<&str> = deltas.iter().map(|s| s.as_str()).collect();
            prop_assert_eq!(output_texts, expected);
        }
    }

    /// Strategy for generating terminal RunEvent variants (those that should
    /// transition from Running → Idle).
    fn terminal_run_event_strategy() -> impl Strategy<Value = RunEvent> {
        prop_oneof![
            ("[a-z]{1,20}", "[a-z]{1,20}", 0u64..1000, 0u64..1000).prop_map(
                |(agent, output, inp, outp)| RunEvent::AgentEnd {
                    agent,
                    output,
                    usage: Usage {
                        input_tokens: inp,
                        output_tokens: outp,
                        cache_read_tokens: None,
                    },
                }
            ),
            "[a-z]{1,50}".prop_map(|error| RunEvent::Error { error }),
            "[a-z]{1,50}".prop_map(|reason| RunEvent::Aborted { reason }),
            (1u32..100).prop_map(|count| RunEvent::MaxTurns { count }),
            ("[a-z]{1,20}", "[a-z]{1,50}").prop_map(|(name, reason)| {
                RunEvent::GuardrailTripped { name, reason }
            }),
        ]
    }

    /// Property 2: Terminal event state transition
    /// All terminal RunEvent variants transition Running → Idle.
    /// **Validates: Requirements 7.1-7.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn terminal_events_transition_running_to_idle(
            event in terminal_run_event_strategy()
        ) {
            let mut state = make_state();
            prop_assert_eq!(state.mode.clone(), AppMode::Running);
            state.update(AppEvent::AgentEvent(event));
            prop_assert_eq!(state.mode.clone(), AppMode::Idle);
        }
    }

    /// Property 3: Tool lifecycle correlation
    /// ToolStart + ToolEnd pair produces Completed entry with correct output/is_error.
    /// **Validates: Requirements 2.2, 2.3**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn tool_lifecycle_produces_completed_entry(
            tool_id in "[a-z0-9]{1,10}",
            tool_name in "[a-z_]{1,15}",
            output in "[a-z ]{0,50}",
            is_error in any::<bool>(),
        ) {
            let mut state = make_state();

            state.update(AppEvent::AgentEvent(RunEvent::ToolStart {
                id: tool_id.clone(),
                name: tool_name.clone(),
            }));
            state.update(AppEvent::AgentEvent(RunEvent::ToolEnd {
                id: tool_id.clone(),
                name: tool_name.clone(),
                output: output.clone(),
                is_error,
            }));

            prop_assert_eq!(state.active_tools.len(), 1);
            let entry = &state.active_tools[0];
            prop_assert_eq!(&entry.id, &tool_id);
            prop_assert_eq!(&entry.name, &tool_name);
            prop_assert_eq!(entry.status.clone(), ToolStatus::Completed);
            prop_assert_eq!(entry.output.as_deref(), Some(output.as_str()));
            prop_assert_eq!(entry.is_error, is_error);
        }
    }

    /// Property 4: Tool executing state invariant
    /// ToolStart without ToolEnd keeps status as Executing.
    /// **Validates: Requirements 2.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn tool_start_without_end_stays_executing(
            tools in prop_vec(
                ("[a-z0-9]{1,10}", "[a-z_]{1,15}"),
                1..10
            )
        ) {
            let mut state = make_state();

            for (id, name) in &tools {
                state.update(AppEvent::AgentEvent(RunEvent::ToolStart {
                    id: id.clone(),
                    name: name.clone(),
                }));
            }

            prop_assert_eq!(state.active_tools.len(), tools.len());
            for entry in &state.active_tools {
                prop_assert_eq!(entry.status.clone(), ToolStatus::Executing);
            }
        }
    }

    /// Property 5: Interruption populates permission prompt
    /// Interruption with N items → PermissionPrompt mode + N pending_approvals.
    /// **Validates: Requirements 3.1**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn interruption_populates_permission_prompt(
            items in prop_vec(
                ("[a-z_]{1,15}", "[a-z0-9]{1,10}"),
                1..=5
            )
        ) {
            let mut state = make_state();

            let pending: Vec<PendingApproval> = items
                .iter()
                .enumerate()
                .map(|(i, (tool_name, req_id))| PendingApproval {
                    tool_name: tool_name.clone(),
                    tool_input: serde_json::json!({"arg": i}),
                    request_id: req_id.clone(),
                })
                .collect();

            let n = pending.len();
            state.update(AppEvent::AgentEvent(RunEvent::Interruption {
                pending,
            }));

            prop_assert_eq!(state.mode, AppMode::PermissionPrompt);
            prop_assert_eq!(state.pending_approvals.len(), n);
        }
    }

    /// Property 6: Permission prompt input filtering
    /// Non-y/a/n keys produce Continue without state change.
    /// **Validates: Requirements 3.5**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn permission_prompt_filters_non_yan_keys(
            ch in any::<char>().prop_filter("exclude y/a/n", |c| {
                *c != 'y' && *c != 'a' && *c != 'n'
            })
        ) {
            let perms = PermissionEngine::new(PermissionMode::Normal);
            let mut state = AppState::new(perms);
            state.mode = AppMode::PermissionPrompt;
            state.pending_approvals = vec![PendingApproval {
                tool_name: "test_tool".to_string(),
                tool_input: serde_json::json!({}),
                request_id: "req1".to_string(),
            }];

            let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
            let result = state.update(AppEvent::Key(key));

            prop_assert_eq!(result, UpdateResult::Continue);
            prop_assert_eq!(state.mode, AppMode::PermissionPrompt);
        }
    }

    /// Property 9: Double Ctrl-C exit logic
    /// Two presses within 2s → Exit; >2s gap → fresh first press.
    /// **Validates: Requirements 6.2, 6.4**
    #[test]
    fn double_ctrl_c_within_2s_exits() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms);
        state.mode = AppMode::Running;

        // Simulate first press just happened (within 2s window)
        state.last_ctrl_c = Some(Instant::now());

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Exit);
    }

    #[test]
    fn double_ctrl_c_after_2s_resets() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms);
        state.mode = AppMode::Running;

        // Simulate first press happened 3 seconds ago (outside 2s window)
        state.last_ctrl_c = Some(Instant::now() - Duration::from_secs(3));

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));

        // Should not exit — treated as fresh first press → AbortRun
        assert_eq!(result, UpdateResult::AbortRun);
        // last_ctrl_c should be updated to now
        assert!(state.last_ctrl_c.unwrap().elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn double_ctrl_c_idle_within_2s_exits() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms);
        // Idle mode, empty input
        state.mode = AppMode::Idle;

        // Simulate first press just happened
        state.last_ctrl_c = Some(Instant::now());

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));
        assert_eq!(result, UpdateResult::Exit);
    }

    #[test]
    fn double_ctrl_c_idle_after_2s_resets() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms);
        state.mode = AppMode::Idle;

        // Simulate first press happened 3 seconds ago
        state.last_ctrl_c = Some(Instant::now() - Duration::from_secs(3));

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = state.update(AppEvent::Key(key));

        // Should not exit — fresh first press
        assert_eq!(result, UpdateResult::Continue);
        assert!(state.last_ctrl_c.unwrap().elapsed() < Duration::from_millis(100));
    }

    /// Property 10: Ctrl-C in running state aborts without exit
    /// Single Ctrl-C in Running → AbortRun, never Exit.
    /// **Validates: Requirements 6.1**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]
        #[test]
        fn ctrl_c_in_running_aborts_without_exit(
            // Use a dummy value just to run multiple iterations
            _dummy in 0u8..50
        ) {
            let perms = PermissionEngine::new(PermissionMode::Normal);
            let mut state = AppState::new(perms);
            state.mode = AppMode::Running;
            // No prior Ctrl-C
            state.last_ctrl_c = None;

            let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            let result = state.update(AppEvent::Key(key));

            prop_assert_eq!(result.clone(), UpdateResult::AbortRun);
            // Must never be Exit
            prop_assert!(result != UpdateResult::Exit);
        }
    }

    /// Property 11: Ctrl-C in idle state clears input
    /// Ctrl-C in Idle with non-empty input → clears buffer, stays Idle.
    /// **Validates: Requirements 6.3**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn ctrl_c_in_idle_clears_input(
            input_text in "[a-z]{1,30}"
        ) {
            let perms = PermissionEngine::new(PermissionMode::Normal);
            let mut state = AppState::new(perms);
            state.mode = AppMode::Idle;

            // Insert text into the input buffer
            for ch in input_text.chars() {
                state.input.insert(ch);
            }
            prop_assert!(!state.input.is_empty());

            let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
            let result = state.update(AppEvent::Key(key));

            prop_assert_eq!(result, UpdateResult::Continue);
            prop_assert!(state.input.is_empty());
            prop_assert_eq!(state.mode, AppMode::Idle);
        }
    }

    /// Strategy for generating random KeyEvent values (any key code).
    fn arbitrary_key_event_strategy() -> impl Strategy<Value = KeyEvent> {
        prop_oneof![
            any::<char>().prop_map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Just(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ]
    }

    /// Property 12: Running state blocks submission
    /// No key in Running mode produces StartRun.
    /// **Validates: Requirements 8.4**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn running_state_blocks_submission(
            key in arbitrary_key_event_strategy()
        ) {
            let perms = PermissionEngine::new(PermissionMode::Normal);
            let mut state = AppState::new(perms);
            state.mode = AppMode::Running;

            let result = state.update(AppEvent::Key(key));

            // Result must never be StartRun while in Running mode
            prop_assert!(
                !matches!(result, UpdateResult::StartRun(_)),
                "Got StartRun({:?}) in Running mode for key {:?}",
                result, key
            );
        }
    }

    /// Property 8: Conversation history accumulation
    /// N completed cycles → 2N messages in history.
    /// Each AgentEnd appends one Assistant message. Each StartRun prepends one User message (done externally in mod.rs).
    /// **Validates: Requirements 5.1, 5.2**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]
        #[test]
        fn conversation_history_accumulates(
            prompts in prop_vec("[a-z ]{1,20}", 1..=5),
            responses in prop_vec("[a-z ]{1,30}", 1..=5),
        ) {
            // Ensure same length
            let n = prompts.len().min(responses.len());
            let prompts = &prompts[..n];
            let responses = &responses[..n];

            let perms = PermissionEngine::new(PermissionMode::Normal);
            let mut state = AppState::new(perms);

            for i in 0..n {
                // Simulate what mod.rs does on StartRun: push user message
                state.history.push(Message::User {
                    content: vec![ContentBlock::Text { text: prompts[i].clone() }],
                });
                state.mode = AppMode::Running;

                // Simulate AgentEnd event
                state.update(AppEvent::AgentEvent(RunEvent::AgentEnd {
                    agent: "arlo".to_string(),
                    output: responses[i].clone(),
                    usage: Usage {
                        input_tokens: 100,
                        output_tokens: 50,
                        cache_read_tokens: None,
                    },
                }));
            }

            // After N cycles: history should have 2N messages (N user + N assistant)
            prop_assert_eq!(state.history.len(), 2 * n);

            // Verify alternating user/assistant pattern
            for i in 0..n {
                match &state.history[2 * i] {
                    Message::User { content } => {
                        if let ContentBlock::Text { text } = &content[0] {
                            prop_assert_eq!(text, &prompts[i]);
                        } else {
                            prop_assert!(false, "Expected text content block");
                        }
                    }
                    _ => prop_assert!(false, "Expected User message at index {}", 2 * i),
                }
                match &state.history[2 * i + 1] {
                    Message::Assistant { content, .. } => {
                        if let ContentBlock::Text { text } = &content[0] {
                            prop_assert_eq!(text, &responses[i]);
                        } else {
                            prop_assert!(false, "Expected text content block");
                        }
                    }
                    _ => prop_assert!(false, "Expected Assistant message at index {}", 2 * i + 1),
                }
            }
        }
    }

    /// Property 7: Session allow persistence and semantics
    /// After pressing 'a' at the permission prompt, subsequent PermissionEngine::check()
    /// calls for that tool name return Allow (via session_allow).
    /// **Validates: Requirements 3.3, 4.1, 4.2, 4.3**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn session_allow_persists_after_a_press(
            tool_name in "[a-z_]{1,15}",
        ) {
            let perms = PermissionEngine::new(PermissionMode::Normal);
            let mut state = AppState::new(perms);
            state.mode = AppMode::PermissionPrompt;
            state.pending_approvals = vec![PendingApproval {
                tool_name: tool_name.clone(),
                tool_input: serde_json::json!({}),
                request_id: "req-test".to_string(),
            }];

            // Press 'a' to always-allow
            let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
            let result = state.update(AppEvent::Key(key));
            prop_assert_eq!(result, UpdateResult::ResumeRun(true));

            // Verify the permission engine now allows this tool
            let decision = state.permissions.check(&tool_name, &ApprovalRequirement::Always);
            match decision {
                PermissionDecision::Allow { reason } => {
                    prop_assert_eq!(reason, Some("session_allow".to_string()));
                }
                other => {
                    prop_assert!(false, "Expected Allow after session grant, got {:?}", other);
                }
            }
        }
    }
}
