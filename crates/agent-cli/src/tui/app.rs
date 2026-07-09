// TUI application state and state machine.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use agent_core::{ApprovalResponse, ContentBlock, Message, PendingApproval, PermissionEngine, RunEvent, StreamChunk, TaskStore};
use agent_core::pattern::extract_primary_arg;

use super::approval::ApprovalRequest;
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
    #[allow(dead_code)]
    Exiting,
}

/// The number of selectable options in the permission prompt.
pub const PERMISSION_OPTION_COUNT: usize = 4;

/// Sub-state for the permission prompt overlay.
///
/// Tracks whether the user is at the initial decision point (y/a/p/n)
/// or editing a pattern string after pressing 'p'.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionPromptState {
    /// Waiting for user to select an option via arrows + Enter or direct key press.
    AwaitingKey {
        /// Currently highlighted option index (0=allow, 1=always, 2=pattern, 3=deny).
        selected: usize,
    },
    /// User pressed 'p', showing pattern suggestion for editing.
    EditingPattern {
        /// Current edit buffer (user can modify the suggestion).
        edit_buffer: String,
        /// Cursor position within edit_buffer.
        cursor: usize,
    },
}

/// Generate a suggested `ToolPattern` string from a tool call context.
///
/// Heuristics:
/// - For Bash: if the command starts with a known prefix (npm, git, cargo, make,
///   docker, pip, yarn, pnpm), suggest `Bash({prefix}*)`. Otherwise, use the first
///   word of the command.
/// - For file tools (tool names starting with `fs_`, `read_`, or `write_`): if the
///   path has a directory prefix, suggest `{tool_name}({dir_prefix}*)`.
/// - Fallback: suggest the exact tool name (equivalent to the 'a' key).
///
/// The returned string is always parseable by `ToolPattern::parse` and always matches
/// the original tool call.
pub fn suggest_pattern(tool_name: &str, tool_input: &serde_json::Value) -> String {
    const KNOWN_BASH_PREFIXES: &[&str] = &[
        "npm", "git", "cargo", "make", "docker", "pip", "yarn", "pnpm",
    ];

    if let Some(primary_arg) = extract_primary_arg(tool_input) {
        // Bash tool: suggest based on command prefix
        if tool_name == "Bash" || tool_name == "bash" {
            for prefix in KNOWN_BASH_PREFIXES {
                if primary_arg.starts_with(prefix) {
                    return format!("{}({}*)", tool_name, prefix);
                }
            }
            // No known prefix — use everything up to and including the first
            // whitespace-delimited word (preserving any leading whitespace so the
            // pattern still matches the original arg via glob).
            let first_word_end = primary_arg
                .char_indices()
                .skip_while(|(_, c)| c.is_whitespace()) // skip leading whitespace
                .find(|(_, c)| c.is_whitespace()) // find end of first word
                .map(|(i, _)| i)
                .unwrap_or(primary_arg.len());
            let prefix = &primary_arg[..first_word_end];
            if !prefix.is_empty() {
                return format!("{}({}*)", tool_name, prefix);
            }
        }

        // File tools: if path has a directory prefix, suggest tool_name(dir/*)
        if tool_name.starts_with("fs_")
            || tool_name.starts_with("read_")
            || tool_name.starts_with("write_")
        {
            if let Some(last_slash) = primary_arg.rfind('/') {
                let dir_prefix = &primary_arg[..=last_slash];
                return format!("{}({}*)", tool_name, dir_prefix);
            }
        }

        // Generic: tool_name(truncated_arg*) — truncate long arguments
        let truncated = if primary_arg.len() > 20 {
            &primary_arg[..20]
        } else {
            primary_arg
        };
        return format!("{}({}*)", tool_name, truncated);
    }

    // No primary arg found: suggest exact tool name
    tool_name.to_string()
}

/// Events that drive the TUI application state machine.
#[derive(Debug)]
pub enum AppEvent {
    /// A terminal key event.
    Key(KeyEvent),
    /// An event from the agent RunStream.
    AgentEvent(RunEvent),
    /// The terminal was resized to (cols, rows).
    Resize(#[allow(dead_code)] u16, #[allow(dead_code)] u16),
    /// A periodic tick for animations/heartbeat.
    Tick,
    /// An approval request from the InteractiveApprovalHandler channel.
    ApprovalEvent(ApprovalRequest),
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
    #[allow(dead_code)]
    ToolOutput,
    /// Error messages.
    Error,
    /// Warning messages.
    Warning,
    /// System messages (dim).
    System,
    /// User input text (displayed distinctly).
    User,
}

/// A tool execution entry tracked in the UI.
#[derive(Debug, Clone)]
pub struct ToolEntry {
    /// Unique identifier for this tool invocation.
    pub id: String,
    /// The name of the tool.
    #[allow(dead_code)]
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

/// What the agent is currently doing — drives the status bar activity indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentActivity {
    /// No activity (idle or between events).
    Idle,
    /// Model is generating a response (streaming text).
    Responding,
    /// Model is in extended thinking mode.
    Thinking,
    /// A tool is executing.
    ToolExecuting {
        /// The name of the tool currently running.
        tool_name: String,
    },
}

/// Spinner frames for the activity indicator (Braille pattern animation).
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
    /// Resolve an approval request from the InteractiveApprovalHandler channel.
    /// Contains the responses to send back via the response channel.
    ResolveApproval(Vec<ApprovalResponse>),
    /// Abort the currently running agent execution.
    AbortRun,
    /// Exit the application.
    Exit,
}

/// The full application state for the TUI.
pub struct AppState {
    /// Current application mode.
    pub mode: AppMode,
    /// Sub-state for the permission prompt (active when `mode == PermissionPrompt`).
    pub prompt_state: PermissionPromptState,
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
    /// The agent name associated with the current approval request (for sub-agent context).
    /// `Some(name)` when a sub-agent triggered the prompt; `None` for top-level agent.
    pub approval_agent_name: Option<String>,
    /// Whether the current permission prompt originated from the handler channel.
    /// When true, the response goes back via ResolveApproval; when false, via ResumeRun.
    pub approval_via_handler: bool,
    /// The text input buffer.
    pub input: InputBuffer,
    /// Timestamp of the last Ctrl-C press (for double-press detection).
    pub last_ctrl_c: Option<Instant>,
    /// Whether the last Ctrl-C aborted a run (affects double-press semantics).
    pub ctrl_c_abort_pending: bool,
    /// Token usage from the last completed run.
    pub last_usage: Option<UsageSummary>,
    /// Current agent activity for the status bar indicator.
    pub activity: AgentActivity,
    /// Spinner animation frame counter (incremented on each Tick while running).
    pub spinner_tick: usize,
    /// Current turn number in the agent loop.
    pub current_turn: u32,
    /// When the current run started (for elapsed time display).
    pub run_started_at: Option<Instant>,
    /// Shared task store for polling and slash commands.
    pub task_store: Option<Arc<dyn TaskStore>>,
}

impl AppState {
    /// Create a new `AppState` with the given permission engine.
    pub fn new(permissions: PermissionEngine, task_store: Option<Arc<dyn TaskStore>>) -> Self {
        Self {
            mode: AppMode::Idle,
            prompt_state: PermissionPromptState::AwaitingKey { selected: 0 },
            output_buffer: Vec::new(),
            history: Vec::new(),
            active_tools: Vec::new(),
            permissions,
            pending_approvals: Vec::new(),
            approval_agent_name: None,
            approval_via_handler: false,
            input: InputBuffer::new(),
            last_ctrl_c: None,
            ctrl_c_abort_pending: false,
            last_usage: None,
            activity: AgentActivity::Idle,
            spinner_tick: 0,
            current_turn: 0,
            run_started_at: None,
            task_store,
        }
    }

    /// Get the current spinner frame character for the activity indicator.
    pub fn spinner_frame(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner_tick % SPINNER_FRAMES.len()]
    }

    /// Get the elapsed time since the current run started, formatted as a string.
    pub fn elapsed_display(&self) -> String {
        match self.run_started_at {
            Some(started) => {
                let elapsed = started.elapsed();
                let secs = elapsed.as_secs();
                if secs < 60 {
                    format!("{}s", secs)
                } else {
                    format!("{}m{}s", secs / 60, secs % 60)
                }
            }
            None => String::new(),
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
            AppEvent::ApprovalEvent(request) => {
                // An approval request arrived from the InteractiveApprovalHandler channel.
                // Store the pending approvals and agent name, then show the permission prompt.
                self.pending_approvals = request.pending;
                self.approval_agent_name = request.agent_name;
                self.approval_via_handler = true;
                self.prompt_state = PermissionPromptState::AwaitingKey { selected: 0 };
                self.mode = AppMode::PermissionPrompt;
                UpdateResult::Continue
            }
            AppEvent::Resize(_, _) => {
                // ratatui handles terminal resize automatically during render
                UpdateResult::Continue
            }
            AppEvent::Tick => {
                if self.mode == AppMode::Running {
                    self.spinner_tick = self.spinner_tick.wrapping_add(1);
                }
                UpdateResult::Continue
            }
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
                // Intercept slash commands — never send /‑prefixed input to the LLM.
                if text.starts_with('/') {
                    // Display the user's command in the output buffer
                    self.output_buffer.push(OutputSpan {
                        text: format!("\n> {}\n", text),
                        style: SpanStyle::User,
                    });
                    // Try to dispatch as a known slash command
                    if let Some(result) = super::commands::try_handle_slash_command(&text, self) {
                        return result;
                    }
                    // If try_handle_slash_command returns None, it means the input
                    // started with '/' but wasn't recognized — this shouldn't happen
                    // since the command handler itself handles unknown commands, but
                    // guard against it by just continuing.
                    return UpdateResult::Continue;
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

    /// Handle a key event while in PermissionPrompt mode (y/a/p/n).
    ///
    /// Dispatches based on the current `prompt_state`:
    /// - `AwaitingKey`: handle y/a/p/n decisions
    /// - `EditingPattern`: handle text editing, Enter to confirm, Esc to cancel
    ///
    /// When the prompt was triggered by the InteractiveApprovalHandler channel
    /// (`approval_via_handler == true`), responses are sent back via `ResolveApproval`.
    /// Otherwise, the legacy `ResumeRun` path is used.
    fn handle_prompt_key(&mut self, key: KeyEvent) -> UpdateResult {
        match &self.prompt_state {
            PermissionPromptState::AwaitingKey { .. } => self.handle_awaiting_key(key),
            PermissionPromptState::EditingPattern { .. } => self.handle_editing_pattern(key),
        }
    }

    /// Handle key events in the `AwaitingKey` sub-state of the permission prompt.
    ///
    /// - 'y': one-time approve
    /// - 'a': always allow exact tool name
    /// - 'p': transition to pattern editing mode with a suggested pattern
    /// - 'n': deny
    fn handle_awaiting_key(&mut self, key: KeyEvent) -> UpdateResult {
        match key.code {
            // Arrow keys navigate the selected option
            KeyCode::Left => {
                if let PermissionPromptState::AwaitingKey { ref mut selected } = self.prompt_state {
                    if *selected > 0 {
                        *selected -= 1;
                    } else {
                        *selected = PERMISSION_OPTION_COUNT - 1;
                    }
                }
                UpdateResult::Continue
            }
            KeyCode::Right => {
                if let PermissionPromptState::AwaitingKey { ref mut selected } = self.prompt_state {
                    *selected = (*selected + 1) % PERMISSION_OPTION_COUNT;
                }
                UpdateResult::Continue
            }
            // Enter confirms the currently selected option
            KeyCode::Enter => {
                let selected = if let PermissionPromptState::AwaitingKey { selected } = self.prompt_state {
                    selected
                } else {
                    0
                };
                match selected {
                    0 => self.execute_allow(),
                    1 => self.execute_always(),
                    2 => self.execute_pattern(),
                    3 => self.execute_deny(),
                    _ => UpdateResult::Continue,
                }
            }
            // Direct key shortcuts still work
            KeyCode::Char('y') => self.execute_allow(),
            KeyCode::Char('a') => self.execute_always(),
            KeyCode::Char('p') => self.execute_pattern(),
            KeyCode::Char('n') => self.execute_deny(),
            _ => UpdateResult::Continue,
        }
    }

    /// Execute "allow once" action from the permission prompt.
    fn execute_allow(&mut self) -> UpdateResult {
        if self.approval_via_handler {
            let responses: Vec<ApprovalResponse> = self
                .pending_approvals
                .iter()
                .map(|_| ApprovalResponse::Allow)
                .collect();
            self.clear_approval_state();
            UpdateResult::ResolveApproval(responses)
        } else {
            UpdateResult::ResumeRun(true)
        }
    }

    /// Execute "always allow" action from the permission prompt.
    fn execute_always(&mut self) -> UpdateResult {
        if self.approval_via_handler {
            let responses: Vec<ApprovalResponse> = self
                .pending_approvals
                .iter()
                .map(|pa| ApprovalResponse::AlwaysAllow {
                    pattern: pa.tool_name.clone(),
                })
                .collect();
            if let Some(approval) = self.pending_approvals.first() {
                self.permissions.grant_session_allow(&approval.tool_name);
            }
            self.clear_approval_state();
            UpdateResult::ResolveApproval(responses)
        } else {
            if let Some(approval) = self.pending_approvals.first() {
                self.permissions.grant_session_allow(&approval.tool_name);
            }
            UpdateResult::ResumeRun(true)
        }
    }

    /// Execute "pattern" action — transition to pattern editing mode.
    fn execute_pattern(&mut self) -> UpdateResult {
        if let Some(approval) = self.pending_approvals.first() {
            let suggested = suggest_pattern(&approval.tool_name, &approval.tool_input);
            let cursor = suggested.len();
            self.prompt_state = PermissionPromptState::EditingPattern {
                edit_buffer: suggested,
                cursor,
            };
        }
        UpdateResult::Continue
    }

    /// Execute "deny" action from the permission prompt.
    fn execute_deny(&mut self) -> UpdateResult {
        if self.approval_via_handler {
            let responses: Vec<ApprovalResponse> = self
                .pending_approvals
                .iter()
                .map(|_| ApprovalResponse::Deny)
                .collect();
            self.clear_approval_state();
            UpdateResult::ResolveApproval(responses)
        } else {
            UpdateResult::ResumeRun(false)
        }
    }

    /// Handle key events in the `EditingPattern` sub-state of the permission prompt.
    ///
    /// - Printable chars: insert at cursor position
    /// - Backspace: delete char before cursor
    /// - Delete: delete char at cursor
    /// - Left/Right arrows: move cursor
    /// - Enter: confirm the pattern, grant session allow and resolve
    /// - Esc: cancel, return to AwaitingKey state
    fn handle_editing_pattern(&mut self, key: KeyEvent) -> UpdateResult {
        // We need to extract mutable references to the edit_buffer and cursor.
        // Use a temporary to avoid borrow conflicts with self.
        let (edit_buffer, cursor) = match &mut self.prompt_state {
            PermissionPromptState::EditingPattern { edit_buffer, cursor } => {
                (edit_buffer, cursor)
            }
            _ => return UpdateResult::Continue,
        };

        match key.code {
            KeyCode::Enter => {
                let pattern = edit_buffer.clone();
                if self.approval_via_handler {
                    let responses: Vec<ApprovalResponse> = self
                        .pending_approvals
                        .iter()
                        .map(|_| ApprovalResponse::AlwaysAllow {
                            pattern: pattern.clone(),
                        })
                        .collect();
                    self.permissions.grant_session_allow(&pattern);
                    self.clear_approval_state();
                    UpdateResult::ResolveApproval(responses)
                } else {
                    self.permissions.grant_session_allow(&pattern);
                    self.prompt_state = PermissionPromptState::AwaitingKey { selected: 0 };
                    self.pending_approvals.clear();
                    UpdateResult::ResumeRun(true)
                }
            }
            KeyCode::Esc => {
                self.prompt_state = PermissionPromptState::AwaitingKey { selected: 0 };
                UpdateResult::Continue
            }
            KeyCode::Char(ch) => {
                edit_buffer.insert(*cursor, ch);
                *cursor += ch.len_utf8();
                UpdateResult::Continue
            }
            KeyCode::Backspace => {
                if *cursor > 0 {
                    let prev_char_len = edit_buffer[..*cursor]
                        .chars()
                        .last()
                        .map_or(0, |c| c.len_utf8());
                    *cursor -= prev_char_len;
                    edit_buffer.remove(*cursor);
                }
                UpdateResult::Continue
            }
            KeyCode::Delete => {
                if *cursor < edit_buffer.len() {
                    edit_buffer.remove(*cursor);
                }
                UpdateResult::Continue
            }
            KeyCode::Left => {
                if *cursor > 0 {
                    let prev_len = edit_buffer[..*cursor]
                        .chars()
                        .last()
                        .map_or(0, |c| c.len_utf8());
                    *cursor -= prev_len;
                }
                UpdateResult::Continue
            }
            KeyCode::Right => {
                if *cursor < edit_buffer.len() {
                    let next_len = edit_buffer[*cursor..]
                        .chars()
                        .next()
                        .map_or(0, |c| c.len_utf8());
                    *cursor += next_len;
                }
                UpdateResult::Continue
            }
            _ => UpdateResult::Continue,
        }
    }

    /// Clear approval-related state after responding to a handler-based prompt.
    fn clear_approval_state(&mut self) {
        self.pending_approvals.clear();
        self.approval_agent_name = None;
        self.approval_via_handler = false;
        self.prompt_state = PermissionPromptState::AwaitingKey { selected: 0 };
        self.mode = AppMode::Running;
    }

    /// Handle a `RunEvent` from the agent's RunStream.
    ///
    /// Maps streaming events to output spans, tracks tool executions,
    /// and transitions the app mode on terminal events.
    fn handle_run_event(&mut self, event: RunEvent) -> UpdateResult {
        match event {
            RunEvent::StreamChunk(chunk) => match chunk {
                StreamChunk::TextDelta { text } => {
                    self.activity = AgentActivity::Responding;
                    self.output_buffer.push(OutputSpan {
                        text,
                        style: SpanStyle::Normal,
                    });
                }
                StreamChunk::ThinkingDelta { text } => {
                    self.activity = AgentActivity::Thinking;
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

            RunEvent::TurnStart { turn, .. } => {
                self.current_turn = turn;
                self.activity = AgentActivity::Responding;
            }

            RunEvent::ToolStart { id, name } => {
                self.activity = AgentActivity::ToolExecuting {
                    tool_name: name.clone(),
                };
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
                // Revert to Responding after tool completes (model will continue)
                self.activity = AgentActivity::Responding;
            }

            RunEvent::Interruption { pending } => {
                self.activity = AgentActivity::Idle;
                self.mode = AppMode::PermissionPrompt;
                self.prompt_state = PermissionPromptState::AwaitingKey { selected: 0 };
                self.pending_approvals = pending;
                self.approval_via_handler = false;
                self.approval_agent_name = None;
            }

            RunEvent::AgentEnd {
                output,
                usage,
                ..
            } => {
                // Append assistant response to conversation history
                self.history.push(Message::Assistant {
                    content: vec![ContentBlock::Text { text: output }],
                    usage: Some(usage.clone()),
                });

                // The response text already streamed in via StreamChunk::TextDelta
                // events, so it is not re-appended here.
                // Add separator after response
                self.output_buffer.push(OutputSpan {
                    text: "\n".to_string(),
                    style: SpanStyle::System,
                });
                self.last_usage = Some(UsageSummary {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                });
                self.mode = AppMode::Idle;
                self.activity = AgentActivity::Idle;
                self.current_turn = 0;
                self.run_started_at = None;
                self.active_tools.clear();
            }

            RunEvent::Error { error } => {
                self.output_buffer.push(OutputSpan {
                    text: error,
                    style: SpanStyle::Error,
                });
                self.mode = AppMode::Idle;
                self.activity = AgentActivity::Idle;
                self.run_started_at = None;
            }

            RunEvent::Aborted { reason } => {
                self.output_buffer.push(OutputSpan {
                    text: reason,
                    style: SpanStyle::Warning,
                });
                self.mode = AppMode::Idle;
                self.activity = AgentActivity::Idle;
                self.run_started_at = None;
            }

            RunEvent::MaxTurns { count } => {
                self.output_buffer.push(OutputSpan {
                    text: format!("Agent reached maximum turn limit ({count} turns)"),
                    style: SpanStyle::Warning,
                });
                self.mode = AppMode::Idle;
                self.activity = AgentActivity::Idle;
                self.run_started_at = None;
            }

            RunEvent::GuardrailTripped { name, reason } => {
                self.output_buffer.push(OutputSpan {
                    text: format!("Guardrail '{name}' tripped: {reason}"),
                    style: SpanStyle::Warning,
                });
                self.mode = AppMode::Idle;
                self.activity = AgentActivity::Idle;
                self.run_started_at = None;
            }

            // Non-terminal events we don't surface in the TUI
            RunEvent::Compaction { .. } | RunEvent::StepResolved(_) => {}
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
        let mut state = AppState::new(perms, None);
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
        // Final output is not re-appended — it already streamed via TextDelta.
        assert!(!state.output_buffer.iter().any(|s| s.text == "All done!"));
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
        AppState::new(perms, None) // starts in Idle mode by default
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
        let mut state = AppState::new(perms, None);
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
            KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT), // uppercase Y is not y
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        ];
        for key in keys {
            assert_eq!(state.update(AppEvent::Key(key)), UpdateResult::Continue);
        }
    }

    // --- Exiting mode test ---

    #[test]
    fn exiting_mode_returns_exit_for_any_key() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
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
        let mut state = AppState::new(perms, None);
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
            ch in any::<char>().prop_filter("exclude y/a/p/n", |c| {
                *c != 'y' && *c != 'a' && *c != 'n' && *c != 'p'
            })
        ) {
            let perms = PermissionEngine::new(PermissionMode::Normal);
            let mut state = AppState::new(perms, None);
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
        let mut state = AppState::new(perms, None);
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
        let mut state = AppState::new(perms, None);
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
        let mut state = AppState::new(perms, None);
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
        let mut state = AppState::new(perms, None);
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
            let mut state = AppState::new(perms, None);
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
            let mut state = AppState::new(perms, None);
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
            let mut state = AppState::new(perms, None);
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
            let mut state = AppState::new(perms, None);

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
            let mut state = AppState::new(perms, None);
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
            let decision = state.permissions.check(&tool_name, &ApprovalRequirement::Always, None);
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

#[cfg(test)]
mod suggest_pattern_tests {
    use super::*;
    use serde_json::json;

    // --- suggest_pattern tests ---

    #[test]
    fn suggest_pattern_bash_npm_prefix() {
        let input = json!({"command": "npm install lodash"});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(npm*)");
    }

    #[test]
    fn suggest_pattern_bash_git_prefix() {
        let input = json!({"command": "git push origin main"});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(git*)");
    }

    #[test]
    fn suggest_pattern_bash_cargo_prefix() {
        let input = json!({"command": "cargo build --release"});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(cargo*)");
    }

    #[test]
    fn suggest_pattern_bash_make_prefix() {
        let input = json!({"command": "make test"});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(make*)");
    }

    #[test]
    fn suggest_pattern_bash_docker_prefix() {
        let input = json!({"command": "docker build ."});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(docker*)");
    }

    #[test]
    fn suggest_pattern_bash_pip_prefix() {
        let input = json!({"command": "pip install requests"});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(pip*)");
    }

    #[test]
    fn suggest_pattern_bash_yarn_prefix() {
        let input = json!({"command": "yarn add react"});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(yarn*)");
    }

    #[test]
    fn suggest_pattern_bash_pnpm_prefix() {
        let input = json!({"command": "pnpm install"});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(pnpm*)");
    }

    #[test]
    fn suggest_pattern_bash_unknown_command_uses_first_word() {
        let input = json!({"command": "ls -la /tmp"});
        let result = suggest_pattern("Bash", &input);
        assert_eq!(result, "Bash(ls*)");
    }

    #[test]
    fn suggest_pattern_bash_lowercase_tool_name() {
        let input = json!({"command": "npm run build"});
        let result = suggest_pattern("bash", &input);
        assert_eq!(result, "bash(npm*)");
    }

    #[test]
    fn suggest_pattern_file_tool_with_directory() {
        let input = json!({"path": "/tmp/foo/bar.txt"});
        let result = suggest_pattern("fs_write", &input);
        assert_eq!(result, "fs_write(/tmp/foo/*)");
    }

    #[test]
    fn suggest_pattern_file_tool_with_root_dir() {
        let input = json!({"path": "/etc/config.json"});
        let result = suggest_pattern("fs_read", &input);
        assert_eq!(result, "fs_read(/etc/*)");
    }

    #[test]
    fn suggest_pattern_read_tool_with_path() {
        let input = json!({"path": "/home/user/project/src/main.rs"});
        let result = suggest_pattern("read_file", &input);
        assert_eq!(result, "read_file(/home/user/project/src/*)");
    }

    #[test]
    fn suggest_pattern_write_tool_with_path() {
        let input = json!({"path": "/var/log/app.log"});
        let result = suggest_pattern("write_file", &input);
        assert_eq!(result, "write_file(/var/log/*)");
    }

    #[test]
    fn suggest_pattern_file_tool_without_slash() {
        // No directory separator — falls through to generic truncation
        let input = json!({"path": "local_file.txt"});
        let result = suggest_pattern("fs_write", &input);
        assert_eq!(result, "fs_write(local_file.txt*)");
    }

    #[test]
    fn suggest_pattern_no_primary_arg() {
        let input = json!({"other_key": "value"});
        let result = suggest_pattern("custom_tool", &input);
        assert_eq!(result, "custom_tool");
    }

    #[test]
    fn suggest_pattern_empty_object() {
        let input = json!({});
        let result = suggest_pattern("some_tool", &input);
        assert_eq!(result, "some_tool");
    }

    #[test]
    fn suggest_pattern_generic_tool_with_command() {
        // Non-bash tool with a command argument — uses generic truncation
        let input = json!({"command": "some-very-long-command-argument-here"});
        let result = suggest_pattern("shell_exec", &input);
        // Should truncate to 20 chars: "some-very-long-comma"
        assert_eq!(result, "shell_exec(some-very-long-comma*)");
    }

    #[test]
    fn suggest_pattern_short_generic_arg() {
        let input = json!({"command": "echo hi"});
        let result = suggest_pattern("run", &input);
        assert_eq!(result, "run(echo hi*)");
    }

    #[test]
    fn suggest_pattern_result_matches_original_call() {
        // Verify that the suggested pattern always matches the original tool call
        let cases = vec![
            ("Bash", json!({"command": "npm install lodash"})),
            ("Bash", json!({"command": "git status"})),
            ("Bash", json!({"command": "ls -la"})),
            ("fs_write", json!({"path": "/tmp/foo.txt"})),
            ("fs_read", json!({"path": "/home/user/file.rs"})),
            ("read_file", json!({"path": "/etc/config"})),
        ];

        for (tool_name, tool_input) in cases {
            let suggestion = suggest_pattern(tool_name, &tool_input);
            let pattern = agent_core::pattern::ToolPattern::parse(&suggestion)
                .expect(&format!("Failed to parse suggestion: {}", suggestion));
            assert!(
                pattern.matches(tool_name, Some(&tool_input)),
                "Suggested pattern '{}' does not match original call ({}, {:?})",
                suggestion,
                tool_name,
                tool_input
            );
        }
    }

    #[test]
    fn suggest_pattern_fallback_matches_tool_name() {
        // When no primary arg, the suggestion is the exact tool name
        let input = json!({"data": [1, 2, 3]});
        let suggestion = suggest_pattern("my_tool", &input);
        let pattern = agent_core::pattern::ToolPattern::parse(&suggestion).unwrap();
        assert!(pattern.matches("my_tool", Some(&input)));
    }

    // --- PermissionPromptState tests ---

    #[test]
    fn prompt_state_defaults_to_awaiting_key() {
        let perms = PermissionEngine::new(agent_core::PermissionMode::Normal);
        let state = AppState::new(perms, None);
        assert_eq!(state.prompt_state, PermissionPromptState::AwaitingKey { selected: 0 });
    }

    #[test]
    fn prompt_state_reset_on_interruption() {
        let perms = PermissionEngine::new(agent_core::PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::Running;
        // Simulate being in EditingPattern state from a previous prompt
        state.prompt_state = PermissionPromptState::EditingPattern {
            edit_buffer: "Bash(npm*)".to_string(),
            cursor: 5,
        };

        let pending = vec![PendingApproval {
            tool_name: "shell".to_string(),
            tool_input: json!({"command": "ls"}),
            request_id: "req1".to_string(),
        }];
        state.update(AppEvent::AgentEvent(RunEvent::Interruption { pending }));

        assert_eq!(state.mode, AppMode::PermissionPrompt);
        assert_eq!(state.prompt_state, PermissionPromptState::AwaitingKey { selected: 0 });
    }

    #[test]
    fn prompt_state_reset_on_approval_event() {
        let perms = PermissionEngine::new(agent_core::PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::Idle;
        // Simulate leftover editing state
        state.prompt_state = PermissionPromptState::EditingPattern {
            edit_buffer: "old_pattern".to_string(),
            cursor: 3,
        };

        let request = ApprovalRequest {
            agent_name: Some("sub-agent".to_string()),
            pending: vec![PendingApproval {
                tool_name: "Bash".to_string(),
                tool_input: json!({"command": "npm install"}),
                request_id: "req2".to_string(),
            }],
        };
        state.update(AppEvent::ApprovalEvent(request));

        assert_eq!(state.mode, AppMode::PermissionPrompt);
        assert_eq!(state.prompt_state, PermissionPromptState::AwaitingKey { selected: 0 });
    }

    #[test]
    fn prompt_state_reset_on_clear_approval() {
        let perms = PermissionEngine::new(agent_core::PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.approval_via_handler = true;
        state.prompt_state = PermissionPromptState::AwaitingKey { selected: 0 };
        state.pending_approvals = vec![PendingApproval {
            tool_name: "Bash".to_string(),
            tool_input: json!({"command": "npm install"}),
            request_id: "req3".to_string(),
        }];

        // Press 'y' to approve via handler path (while in AwaitingKey state)
        let key = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        state.update(AppEvent::Key(key));

        assert_eq!(state.prompt_state, PermissionPromptState::AwaitingKey { selected: 0 });
        assert_eq!(state.mode, AppMode::Running);
    }
}

#[cfg(test)]
mod suggest_pattern_property_tests {
    use super::*;
    use proptest::prelude::*;

    // ===================================================================
    // Property 12: Pattern Suggestion Validity
    //
    // For any tool call (tool_name, tool_input), suggest_pattern always produces
    // a string that, when parsed as a ToolPattern, matches the original tool call.
    //
    // **Validates: Requirements 8.3**
    // ===================================================================

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(200))]

        #[test]
        fn suggest_pattern_always_matches_original_call(
            tool_name in prop_oneof![
                Just("Bash".to_string()),
                Just("fs_read".to_string()),
                Just("fs_write".to_string()),
                Just("read_file".to_string()),
                Just("web_fetch".to_string()),
                "[a-z][a-z_]{1,10}".prop_map(|s| s),
            ],
            arg_value in "[a-zA-Z0-9/_. -]{1,30}",
            key in prop_oneof![
                Just("command".to_string()),
                Just("path".to_string()),
                Just("url".to_string()),
            ],
        ) {
            let tool_input = serde_json::json!({ key: arg_value });
            let suggested = suggest_pattern(&tool_name, &tool_input);
            let pattern = agent_core::pattern::ToolPattern::parse(&suggested)
                .expect("suggest_pattern should always produce a parseable pattern");
            prop_assert!(
                pattern.matches(&tool_name, Some(&tool_input)),
                "Pattern '{}' should match tool_name='{}' with input={}",
                suggested, tool_name, tool_input
            );
        }
    }

    /// Test the no-primary-arg case separately: when tool_input has no recognized
    /// primary argument key, suggest_pattern returns the tool name as-is,
    /// which is a bare pattern that matches the tool name.
    #[test]
    fn suggest_pattern_no_primary_arg_matches() {
        let cases = vec![
            ("my_tool", serde_json::json!({"data": "value"})),
            ("custom_op", serde_json::json!({"count": 42})),
            ("analyzer", serde_json::json!({})),
            ("Bash", serde_json::json!({"cwd": "/tmp"})), // no "command" key
        ];

        for (tool_name, tool_input) in &cases {
            let suggested = suggest_pattern(tool_name, tool_input);
            let pattern = agent_core::pattern::ToolPattern::parse(&suggested)
                .expect(&format!(
                    "suggest_pattern should produce a parseable pattern, got '{}'",
                    suggested
                ));
            // For bare patterns (no primary arg), the pattern matches on tool name alone
            assert!(
                pattern.matches(tool_name, Some(tool_input)),
                "Pattern '{}' should match tool_name='{}' with input={:?}",
                suggested,
                tool_name,
                tool_input
            );
        }
    }
}

#[cfg(test)]
mod p_key_handler_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use agent_core::{ApprovalResponse, PendingApproval, PermissionEngine, PermissionMode};
    use serde_json::json;

    fn make_prompt_state_with_tool(tool_name: &str, tool_input: serde_json::Value) -> AppState {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.prompt_state = PermissionPromptState::AwaitingKey { selected: 0 };
        state.pending_approvals = vec![PendingApproval {
            tool_name: tool_name.to_string(),
            tool_input,
            request_id: "req1".to_string(),
        }];
        state
    }

    // --- AwaitingKey → EditingPattern transition on 'p' key ---

    #[test]
    fn p_key_transitions_to_editing_pattern() {
        let mut state = make_prompt_state_with_tool("Bash", json!({"command": "npm install lodash"}));
        let key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));

        assert_eq!(result, UpdateResult::Continue);
        match &state.prompt_state {
            PermissionPromptState::EditingPattern { edit_buffer, cursor } => {
                assert_eq!(edit_buffer, "Bash(npm*)");
                assert_eq!(*cursor, "Bash(npm*)".len());
            }
            _ => panic!("Expected EditingPattern state after pressing 'p'"),
        }
    }

    #[test]
    fn p_key_with_no_approvals_stays_awaiting() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.prompt_state = PermissionPromptState::AwaitingKey { selected: 0 };
        state.pending_approvals = vec![]; // no pending approvals

        let key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key));

        assert_eq!(result, UpdateResult::Continue);
        assert_eq!(state.prompt_state, PermissionPromptState::AwaitingKey { selected: 0 });
    }

    // --- EditingPattern: Enter confirms and resolves ---

    #[test]
    fn editing_pattern_enter_resolves_legacy_path() {
        let mut state = make_prompt_state_with_tool("Bash", json!({"command": "cargo build"}));
        state.approval_via_handler = false;

        // Press 'p' to enter editing mode
        let key_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        state.update(AppEvent::Key(key_p));

        // Press Enter to confirm
        let key_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key_enter));

        assert_eq!(result, UpdateResult::ResumeRun(true));
        assert_eq!(state.prompt_state, PermissionPromptState::AwaitingKey { selected: 0 });
        assert!(state.pending_approvals.is_empty());
    }

    #[test]
    fn editing_pattern_enter_resolves_handler_path() {
        let mut state = make_prompt_state_with_tool("Bash", json!({"command": "npm test"}));
        state.approval_via_handler = true;

        // Press 'p' to enter editing mode
        let key_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        state.update(AppEvent::Key(key_p));

        // Press Enter to confirm
        let key_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key_enter));

        match result {
            UpdateResult::ResolveApproval(responses) => {
                assert_eq!(responses.len(), 1);
                assert_eq!(
                    responses[0],
                    ApprovalResponse::AlwaysAllow {
                        pattern: "Bash(npm*)".to_string(),
                    }
                );
            }
            other => panic!("Expected ResolveApproval, got {:?}", other),
        }
    }

    // --- EditingPattern: Esc cancels and returns to AwaitingKey ---

    #[test]
    fn editing_pattern_esc_returns_to_awaiting_key() {
        let mut state = make_prompt_state_with_tool("Bash", json!({"command": "git push"}));

        // Press 'p' to enter editing mode
        let key_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        state.update(AppEvent::Key(key_p));
        assert!(matches!(state.prompt_state, PermissionPromptState::EditingPattern { .. }));

        // Press Esc to cancel
        let key_esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let result = state.update(AppEvent::Key(key_esc));

        assert_eq!(result, UpdateResult::Continue);
        assert_eq!(state.prompt_state, PermissionPromptState::AwaitingKey { selected: 0 });
        assert_eq!(state.mode, AppMode::PermissionPrompt); // still in prompt mode
    }

    // --- EditingPattern: character insertion ---

    #[test]
    fn editing_pattern_char_inserts_at_cursor() {
        let mut state = make_prompt_state_with_tool("Bash", json!({"command": "npm run build"}));

        // Press 'p' to enter editing mode (cursor at end of "Bash(npm*)")
        let key_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        state.update(AppEvent::Key(key_p));

        // Type a character
        let key_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        state.update(AppEvent::Key(key_x));

        match &state.prompt_state {
            PermissionPromptState::EditingPattern { edit_buffer, cursor } => {
                assert_eq!(edit_buffer, "Bash(npm*)x");
                assert_eq!(*cursor, "Bash(npm*)x".len());
            }
            _ => panic!("Expected EditingPattern state"),
        }
    }

    // --- EditingPattern: backspace ---

    #[test]
    fn editing_pattern_backspace_removes_char() {
        let mut state = make_prompt_state_with_tool("Bash", json!({"command": "npm install"}));

        // Press 'p' → editing mode with "Bash(npm*)" cursor at end
        let key_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        state.update(AppEvent::Key(key_p));

        // Press backspace to remove the last char ')'
        let key_bs = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        state.update(AppEvent::Key(key_bs));

        match &state.prompt_state {
            PermissionPromptState::EditingPattern { edit_buffer, cursor } => {
                assert_eq!(edit_buffer, "Bash(npm*");
                assert_eq!(*cursor, "Bash(npm*".len());
            }
            _ => panic!("Expected EditingPattern state"),
        }
    }

    #[test]
    fn editing_pattern_backspace_at_start_does_nothing() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.prompt_state = PermissionPromptState::EditingPattern {
            edit_buffer: "hello".to_string(),
            cursor: 0, // cursor at start
        };
        state.pending_approvals = vec![PendingApproval {
            tool_name: "test".to_string(),
            tool_input: json!({}),
            request_id: "req1".to_string(),
        }];

        let key_bs = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        state.update(AppEvent::Key(key_bs));

        match &state.prompt_state {
            PermissionPromptState::EditingPattern { edit_buffer, cursor } => {
                assert_eq!(edit_buffer, "hello");
                assert_eq!(*cursor, 0);
            }
            _ => panic!("Expected EditingPattern state"),
        }
    }

    // --- EditingPattern: delete ---

    #[test]
    fn editing_pattern_delete_removes_char_at_cursor() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.prompt_state = PermissionPromptState::EditingPattern {
            edit_buffer: "hello".to_string(),
            cursor: 0,
        };
        state.pending_approvals = vec![PendingApproval {
            tool_name: "test".to_string(),
            tool_input: json!({}),
            request_id: "req1".to_string(),
        }];

        let key_del = KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE);
        state.update(AppEvent::Key(key_del));

        match &state.prompt_state {
            PermissionPromptState::EditingPattern { edit_buffer, cursor } => {
                assert_eq!(edit_buffer, "ello");
                assert_eq!(*cursor, 0);
            }
            _ => panic!("Expected EditingPattern state"),
        }
    }

    // --- EditingPattern: arrow keys ---

    #[test]
    fn editing_pattern_left_arrow_moves_cursor() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.prompt_state = PermissionPromptState::EditingPattern {
            edit_buffer: "abc".to_string(),
            cursor: 3,
        };
        state.pending_approvals = vec![PendingApproval {
            tool_name: "test".to_string(),
            tool_input: json!({}),
            request_id: "req1".to_string(),
        }];

        let key_left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        state.update(AppEvent::Key(key_left));

        match &state.prompt_state {
            PermissionPromptState::EditingPattern { cursor, .. } => {
                assert_eq!(*cursor, 2);
            }
            _ => panic!("Expected EditingPattern state"),
        }
    }

    #[test]
    fn editing_pattern_right_arrow_moves_cursor() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.prompt_state = PermissionPromptState::EditingPattern {
            edit_buffer: "abc".to_string(),
            cursor: 0,
        };
        state.pending_approvals = vec![PendingApproval {
            tool_name: "test".to_string(),
            tool_input: json!({}),
            request_id: "req1".to_string(),
        }];

        let key_right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        state.update(AppEvent::Key(key_right));

        match &state.prompt_state {
            PermissionPromptState::EditingPattern { cursor, .. } => {
                assert_eq!(*cursor, 1);
            }
            _ => panic!("Expected EditingPattern state"),
        }
    }

    #[test]
    fn editing_pattern_left_at_start_does_nothing() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.prompt_state = PermissionPromptState::EditingPattern {
            edit_buffer: "abc".to_string(),
            cursor: 0,
        };
        state.pending_approvals = vec![PendingApproval {
            tool_name: "test".to_string(),
            tool_input: json!({}),
            request_id: "req1".to_string(),
        }];

        let key_left = KeyEvent::new(KeyCode::Left, KeyModifiers::NONE);
        state.update(AppEvent::Key(key_left));

        match &state.prompt_state {
            PermissionPromptState::EditingPattern { cursor, .. } => {
                assert_eq!(*cursor, 0);
            }
            _ => panic!("Expected EditingPattern state"),
        }
    }

    #[test]
    fn editing_pattern_right_at_end_does_nothing() {
        let perms = PermissionEngine::new(PermissionMode::Normal);
        let mut state = AppState::new(perms, None);
        state.mode = AppMode::PermissionPrompt;
        state.prompt_state = PermissionPromptState::EditingPattern {
            edit_buffer: "abc".to_string(),
            cursor: 3,
        };
        state.pending_approvals = vec![PendingApproval {
            tool_name: "test".to_string(),
            tool_input: json!({}),
            request_id: "req1".to_string(),
        }];

        let key_right = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
        state.update(AppEvent::Key(key_right));

        match &state.prompt_state {
            PermissionPromptState::EditingPattern { cursor, .. } => {
                assert_eq!(*cursor, 3);
            }
            _ => panic!("Expected EditingPattern state"),
        }
    }

    // --- Existing y/a/n behavior is preserved in AwaitingKey ---

    #[test]
    fn y_a_n_still_work_in_awaiting_key() {
        // 'y' still allows once
        let mut state = make_prompt_state_with_tool("shell", json!({"command": "ls"}));
        let key = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(state.update(AppEvent::Key(key)), UpdateResult::ResumeRun(true));

        // 'a' still grants session allow
        let mut state = make_prompt_state_with_tool("shell", json!({"command": "ls"}));
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(state.update(AppEvent::Key(key)), UpdateResult::ResumeRun(true));

        // 'n' still denies
        let mut state = make_prompt_state_with_tool("shell", json!({"command": "ls"}));
        let key = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(state.update(AppEvent::Key(key)), UpdateResult::ResumeRun(false));
    }

    // --- Editing and confirming actually grants session allow ---

    #[test]
    fn editing_pattern_confirm_grants_session_allow() {
        let mut state = make_prompt_state_with_tool("Bash", json!({"command": "cargo test"}));
        state.approval_via_handler = false;

        // Press 'p' to enter editing mode
        let key_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        state.update(AppEvent::Key(key_p));

        // The suggested pattern should be "Bash(cargo*)"
        match &state.prompt_state {
            PermissionPromptState::EditingPattern { edit_buffer, .. } => {
                assert_eq!(edit_buffer, "Bash(cargo*)");
            }
            _ => panic!("Expected EditingPattern state"),
        }

        // Confirm with Enter
        let key_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        state.update(AppEvent::Key(key_enter));

        // Verify the session allow was granted: a subsequent "cargo build" call should be allowed
        let check_input = json!({"command": "cargo build --release"});
        assert!(state.permissions.has_session_allow("Bash", Some(&check_input)));
    }
}
