// Event loop multiplexer for terminal and agent events.

use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use agent_core::RunStream;

use super::app::AppEvent;

/// A persistent terminal event poller that lives for the entire session.
///
/// Unlike stream tasks (which are per-run), the terminal poller is created once
/// and sends key/resize events through a long-lived channel. This avoids the race
/// condition where aborting and re-spawning a blocking poll task can eat key events
/// that arrive during the transition window.
pub struct TerminalPoller {
    /// Handle to the blocking poll task (aborted only on app exit).
    #[allow(dead_code)]
    pub handle: JoinHandle<()>,
}

/// Spawn the terminal polling task. Call once at session start.
///
/// Returns (event_sender, poller) — the sender is shared with stream tasks so
/// all events flow into a single receiver in the main loop.
pub fn spawn_terminal_poller() -> (mpsc::UnboundedSender<AppEvent>, mpsc::UnboundedReceiver<AppEvent>, TerminalPoller) {
    let (tx, rx) = mpsc::unbounded_channel();

    let term_tx = tx.clone();
    let handle = tokio::task::spawn_blocking(move || {
        loop {
            if event::poll(Duration::from_millis(33)).unwrap_or(false) {
                if let Ok(evt) = event::read() {
                    let app_event = match evt {
                        CrosstermEvent::Key(key) => Some(AppEvent::Key(key)),
                        CrosstermEvent::Resize(cols, rows) => Some(AppEvent::Resize(cols, rows)),
                        _ => None,
                    };
                    if let Some(app_event) = app_event {
                        if term_tx.send(app_event).is_err() {
                            break; // Receiver dropped, exit
                        }
                    }
                }
            } else {
                // No terminal event within the poll window — emit a Tick for animations
                if term_tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        }
    });

    (tx, rx, TerminalPoller { handle })
}

/// Handle to a RunStream forwarding task (per-run, abortable).
pub struct StreamForwarder {
    pub handle: JoinHandle<()>,
}

/// Spawn a RunStream forwarding task that sends agent events into the shared channel.
///
/// The returned handle can be aborted when the run is cancelled.
pub fn spawn_stream_forwarder(
    stream: RunStream,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> StreamForwarder {
    let handle = tokio::spawn(async move {
        futures::pin_mut!(stream);
        while let Some(event) = stream.next().await {
            if tx.send(AppEvent::AgentEvent(event)).is_err() {
                break;
            }
        }
    });

    StreamForwarder { handle }
}
