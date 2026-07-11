//! Web UI mode: an axum server that upgrades a WebSocket connection into a
//! per-connection session driver, the web analogue of `crate::tui`.

pub mod agui;
pub mod ws_approval;
