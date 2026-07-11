pub mod app;
pub mod approval;
pub mod commands;
pub mod event_loop;
pub mod input;
pub mod notifications;
pub mod render;

use std::io::{self, stdout};
use std::sync::Arc;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use agent_core::{
    run_stream, Agent, ApprovalResponse, ContentBlock, Input, Instructions, Message, ModelProvider,
    PermissionEngine, PermissionMode, RunConfig, SessionStore, TaskStore, Tool,
};

use self::app::{AppMode, AppState, OutputSpan, SpanStyle, UpdateResult};
use self::approval::InteractiveApprovalHandler;
use self::event_loop::{spawn_stream_forwarder, spawn_terminal_poller, StreamForwarder};

/// Run the interactive TUI REPL.
///
/// Sets up the ratatui terminal, initializes session state, and drives the
/// event loop that multiplexes terminal input with agent RunStream events.
#[allow(clippy::too_many_arguments)]
pub async fn run_tui_repl(
    provider: Arc<dyn ModelProvider>,
    model: &str,
    tools: Vec<Arc<dyn Tool>>,
    instructions: Instructions,
    permission_mode: PermissionMode,
    _task_store: Arc<dyn TaskStore>,
    session_store: Arc<dyn SessionStore>,
    session_id: String,
    initial_history: Vec<Message>,
) -> io::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Install panic hook that cleans up terminal state before delegating
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    // Create approval channels for the InteractiveApprovalHandler
    let (approval_req_tx, mut approval_req_rx) = mpsc::channel::<approval::ApprovalRequest>(1);
    let (approval_resp_tx, approval_resp_rx) = mpsc::channel::<Vec<ApprovalResponse>>(1);
    let approval_handler: Arc<dyn agent_core::ApprovalHandler> = Arc::new(
        InteractiveApprovalHandler::new(approval_req_tx, approval_resp_rx),
    );

    // Initialize application state with permission engine
    let permissions = PermissionEngine::new(permission_mode);
    let mut state = AppState::new(permissions.clone(), Some(_task_store.clone()));

    // Seed conversation history when resuming a stored session
    if !initial_history.is_empty() {
        state.output_buffer.push(OutputSpan {
            text: format!(
                "Resumed session {} ({} messages)\n",
                session_id,
                initial_history.len()
            ),
            style: SpanStyle::System,
        });
        state.history = initial_history;
    }
    // Persist history whenever it grows (user prompts, assistant replies, denials)
    let mut saved_history_len = state.history.len();

    // Keep a reference to task_store for RunConfig building
    let task_store = _task_store;

    // Spawn the terminal poller ONCE — it lives for the entire session.
    // This avoids the race condition where aborting/re-spawning a blocking
    // terminal reader can eat key events during the transition.
    let (event_tx, mut event_rx, _terminal_poller) = spawn_terminal_poller();

    // Track the current stream forwarder (per-run, abortable)
    let mut stream_forwarder: Option<StreamForwarder> = None;

    // Main event loop
    loop {
        // Render current state
        terminal.draw(|f| render::draw(f, &state))?;

        // Wait for next event — multiplex between event sources and approval requests
        let event = tokio::select! {
            ev = event_rx.recv() => {
                match ev {
                    Some(e) => e,
                    None => break, // channel closed (terminal poller exited)
                }
            }
            req = approval_req_rx.recv() => {
                match req {
                    Some(request) => app::AppEvent::ApprovalEvent(request),
                    None => continue, // approval channel closed, ignore
                }
            }
        };

        // Check if event is a Tick before consuming it in update()
        let is_tick = matches!(&event, app::AppEvent::Tick);

        let result = state.update(event);

        // Poll notifications on Tick only when Idle. While a run is in flight
        // the run loop itself drains and acknowledges terminal tasks so the
        // model sees the results — polling here too would steal them.
        if is_tick && matches!(state.mode, AppMode::Idle) {
            notifications::poll_notifications(&mut state).await;
        }

        match result {
            UpdateResult::Continue => {}
            UpdateResult::StartRun(prompt) => {
                // Display user prompt in the output area
                state.output_buffer.push(OutputSpan {
                    text: format!("\n> {}\n\n", prompt),
                    style: SpanStyle::User,
                });

                // Push user message to conversation history
                state.history.push(Message::User {
                    content: vec![ContentBlock::Text { text: prompt }],
                });
                state.mode = AppMode::Running;
                state.activity = app::AgentActivity::Responding;
                state.run_started_at = Some(std::time::Instant::now());
                state.current_turn = 0;
                state.spinner_tick = 0;

                // Build agent
                let mut builder = Agent::builder("arlo").instructions(instructions.clone());
                for tool in &tools {
                    builder = builder.tool(tool.clone());
                }
                let agent = builder.build();

                // Build run config with approval handler and task store
                let config = RunConfig::builder(provider.clone(), model)
                    .permissions(state.permissions.clone())
                    .approval_handler(approval_handler.clone())
                    .task_store(task_store.clone())
                    .build();

                // Create run stream with full conversation history
                let input = Input::Items {
                    messages: state.history.clone(),
                };
                let stream = run_stream(&agent, input, &config);

                // Abort previous stream forwarder (if any) and spawn a new one.
                // The terminal poller stays running — no abort/respawn needed.
                if let Some(fwd) = stream_forwarder.take() {
                    fwd.handle.abort();
                }
                stream_forwarder = Some(spawn_stream_forwarder(stream, event_tx.clone()));
            }
            UpdateResult::ResumeRun(approved) => {
                state.mode = AppMode::Running;

                if !approved {
                    // Append denial as tool results in history
                    for approval in &state.pending_approvals {
                        state.history.push(Message::ToolResult {
                            tool_use_id: approval.request_id.clone(),
                            content: format!(
                                "Permission denied by user for tool '{}'",
                                approval.tool_name
                            ),
                            is_error: true,
                        });
                    }
                }

                state.pending_approvals.clear();

                // Build agent
                let mut builder = Agent::builder("arlo").instructions(instructions.clone());
                for tool in &tools {
                    builder = builder.tool(tool.clone());
                }
                let agent = builder.build();

                // Build run config with approval handler and task store
                let config = RunConfig::builder(provider.clone(), model)
                    .permissions(state.permissions.clone())
                    .approval_handler(approval_handler.clone())
                    .task_store(task_store.clone())
                    .build();

                // Create new run stream to resume
                let input = Input::Items {
                    messages: state.history.clone(),
                };
                let stream = run_stream(&agent, input, &config);

                // Abort previous stream forwarder and spawn new one
                if let Some(fwd) = stream_forwarder.take() {
                    fwd.handle.abort();
                }
                stream_forwarder = Some(spawn_stream_forwarder(stream, event_tx.clone()));
            }
            UpdateResult::ResolveApproval(responses) => {
                // Send responses back to the InteractiveApprovalHandler via channel.
                // The run continues automatically — no need to restart the stream.
                let _ = approval_resp_tx.send(responses).await;
            }
            UpdateResult::AbortRun => {
                // Abort by dropping the stream forwarder
                if let Some(fwd) = stream_forwarder.take() {
                    fwd.handle.abort();
                }
                state.mode = AppMode::Idle;
                state.output_buffer.push(OutputSpan {
                    text: "\nRun cancelled.\n".to_string(),
                    style: SpanStyle::System,
                });
            }
            UpdateResult::Exit => break,
        }

        // Persist session history when it changed this iteration.
        // save() is a full atomic rewrite — cheap at conversation sizes.
        if state.history.len() != saved_history_len {
            if let Err(e) = session_store.save(&session_id, &state.history).await {
                state.output_buffer.push(OutputSpan {
                    text: format!("warning: failed to persist session: {}\n", e),
                    style: SpanStyle::Warning,
                });
            }
            saved_history_len = state.history.len();
        }
    }

    // Teardown terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
