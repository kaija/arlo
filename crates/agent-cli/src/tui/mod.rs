pub mod app;
pub mod event_loop;
pub mod input;
pub mod render;

use std::io::{self, stdout};
use std::sync::Arc;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use agent_core::{
    run_stream, Agent, ContentBlock, Input, Message, PermissionEngine, PermissionMode, RunConfig,
    Tool,
};
use agent_llm::UnifiedProvider;

use self::app::{AppMode, AppState, OutputSpan, SpanStyle, UpdateResult};
use self::event_loop::spawn_event_sources;

/// Run the interactive TUI REPL.
///
/// Sets up the ratatui terminal, initializes session state, and drives the
/// event loop that multiplexes terminal input with agent RunStream events.
pub async fn run_tui_repl(
    provider: Arc<UnifiedProvider>,
    model: &str,
    tools: Vec<Arc<dyn Tool>>,
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

    // Initialize application state with permission engine
    let permissions = PermissionEngine::new(PermissionMode::Normal);
    let mut state = AppState::new(permissions.clone());

    // Spawn initial event sources (terminal-only, no RunStream yet)
    let (mut event_rx, event_sources) = spawn_event_sources(None);
    let mut event_sources = Some(event_sources);

    // Main event loop
    loop {
        // Render current state
        terminal.draw(|f| render::draw(f, &state))?;

        // Wait for next event
        match event_rx.recv().await {
            Some(event) => {
                let result = state.update(event);
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

                        // Build agent
                        let mut builder = Agent::builder("arlo");
                        for tool in &tools {
                            builder = builder.tool(tool.clone());
                        }
                        let agent = builder.build();

                        // Build run config
                        let config = RunConfig::builder(provider.clone(), model)
                            .permissions(state.permissions.clone())
                            .build();

                        // Create run stream with full conversation history
                        let input = Input::Items {
                            messages: state.history.clone(),
                        };
                        let stream = run_stream(&agent, input, &config);

                        // Abort previous event sources and spawn new ones with stream
                        if let Some(sources) = event_sources.take() {
                            sources.terminal_handle.abort();
                            if let Some(h) = sources.stream_handle {
                                h.abort();
                            }
                        }
                        let (new_rx, new_sources) = spawn_event_sources(Some(stream));
                        event_rx = new_rx;
                        event_sources = Some(new_sources);
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
                        let mut builder = Agent::builder("arlo");
                        for tool in &tools {
                            builder = builder.tool(tool.clone());
                        }
                        let agent = builder.build();

                        // Build run config
                        let config = RunConfig::builder(provider.clone(), model)
                            .permissions(state.permissions.clone())
                            .build();

                        // Create new run stream to resume
                        let input = Input::Items {
                            messages: state.history.clone(),
                        };
                        let stream = run_stream(&agent, input, &config);

                        // Abort previous event sources and spawn new ones with stream
                        if let Some(sources) = event_sources.take() {
                            sources.terminal_handle.abort();
                            if let Some(h) = sources.stream_handle {
                                h.abort();
                            }
                        }
                        let (new_rx, new_sources) = spawn_event_sources(Some(stream));
                        event_rx = new_rx;
                        event_sources = Some(new_sources);
                    }
                    UpdateResult::AbortRun => {
                        // Abort by dropping the stream handles
                        if let Some(sources) = event_sources.take() {
                            sources.terminal_handle.abort();
                            if let Some(h) = sources.stream_handle {
                                h.abort();
                            }
                        }
                        state.mode = AppMode::Idle;
                        state.output_buffer.push(OutputSpan {
                            text: "\nRun cancelled.\n".to_string(),
                            style: SpanStyle::System,
                        });

                        // Re-spawn terminal-only event sources (no RunStream)
                        let (new_rx, new_sources) = spawn_event_sources(None);
                        event_rx = new_rx;
                        event_sources = Some(new_sources);
                    }
                    UpdateResult::Exit => break,
                }
            }
            None => break, // channel closed
        }
    }

    // Teardown terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
