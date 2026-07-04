// Event loop multiplexer for terminal and agent events.

use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use agent_core::RunStream;

use super::app::AppEvent;
use super::approval::ApprovalRequest;

/// Handle to event sources that can be stopped by aborting the join handles.
pub struct EventSources {
    /// Handle for the terminal event polling task.
    pub terminal_handle: JoinHandle<()>,
    /// Handle for the RunStream forwarding task (if active).
    pub stream_handle: Option<JoinHandle<()>>,
    /// Handle for the approval request forwarding task (if active).
    pub approval_handle: Option<JoinHandle<()>>,
}

/// Spawn event source tasks and return a receiver for AppEvents.
///
/// - Spawns a blocking task that polls crossterm at ~30fps (33ms poll timeout)
/// - If a RunStream is provided, spawns an async task to forward its events
/// - If an approval_rx is provided, spawns an async task to forward approval requests
///
/// Returns (event_receiver, event_sources)
pub fn spawn_event_sources(
    run_stream: Option<RunStream>,
    approval_rx: Option<mpsc::Receiver<ApprovalRequest>>,
) -> (mpsc::UnboundedReceiver<AppEvent>, EventSources) {
    let (tx, rx) = mpsc::unbounded_channel();

    // Terminal input polling task (blocking)
    let term_tx = tx.clone();
    let terminal_handle = tokio::task::spawn_blocking(move || {
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
            }
        }
    });

    // RunStream forwarding task (async)
    let stream_handle = run_stream.map(|stream| {
        let stream_tx = tx.clone();
        tokio::spawn(async move {
            futures::pin_mut!(stream);
            while let Some(event) = stream.next().await {
                if stream_tx.send(AppEvent::AgentEvent(event)).is_err() {
                    break;
                }
            }
        })
    });

    // Approval request forwarding task (async)
    let approval_handle = approval_rx.map(|mut rx| {
        let approval_tx = tx;
        tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                if approval_tx.send(AppEvent::ApprovalEvent(request)).is_err() {
                    break;
                }
            }
        })
    });

    (rx, EventSources { terminal_handle, stream_handle, approval_handle })
}
