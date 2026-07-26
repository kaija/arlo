//! AG-UI protocol server — exposes the agent over HTTP/SSE.

#![allow(dead_code)] // Wired into main.rs in task 11.1

pub mod approval;
pub mod bridge;
pub mod session;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use agent_core::agent::Agent;
use agent_core::config::RunConfig;

/// Parse the `--serve` flag value into a `SocketAddr`.
///
/// - `None` → `127.0.0.1:8080`
/// - Port-only (all digits) → `127.0.0.1:{port}`
/// - `host:port` → parsed directly
pub fn parse_serve_addr(input: Option<&str>) -> Result<SocketAddr, String> {
    let s = match input {
        None => return Ok(SocketAddr::from(([127, 0, 0, 1], 8080))),
        Some(s) => s,
    };

    if s.chars().all(|c| c.is_ascii_digit()) {
        let port: u16 = s.parse().map_err(|_| format!("invalid port: {s}"))?;
        Ok(SocketAddr::from(([127, 0, 0, 1], port)))
    } else {
        s.parse::<SocketAddr>()
            .map_err(|e| format!("invalid address '{s}': {e}"))
    }
}

/// Start the AG-UI HTTP server.
///
/// Binds to `bind_addr`, serves the AG-UI SSE endpoint, and shuts down
/// gracefully on SIGINT/SIGTERM (30s drain).
pub async fn start_server(
    bind_addr: SocketAddr,
    agent: Agent,
    config: RunConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let sessions = Arc::new(session::SessionStore::new());
    let bridge = bridge::ArloBridge::new(agent, config, Arc::clone(&sessions));
    let router = ag_ui_server::axum::agent_router(bridge);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("port {} is already in use", bind_addr.port())
            } else {
                format!("failed to bind {bind_addr}: {e}")
            }
        })?;

    let local_addr = listener.local_addr()?;
    tracing::info!("AG-UI server listening on {local_addr}");

    // Background session reaper: every 60s, drop sessions idle > 10 min
    let reaper_sessions = Arc::clone(&sessions);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            reaper_sessions.reap_expired(Duration::from_secs(600));
        }
    });

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }

    tracing::info!("shutdown signal received, draining connections");
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn none_defaults_to_localhost_8080() {
        let addr = parse_serve_addr(None).unwrap();
        assert_eq!(addr, SocketAddr::from(([127, 0, 0, 1], 8080)));
    }

    #[test]
    fn port_only() {
        let addr = parse_serve_addr(Some("9000")).unwrap();
        assert_eq!(addr, SocketAddr::from(([127, 0, 0, 1], 9000)));
    }

    #[test]
    fn host_port() {
        let addr = parse_serve_addr(Some("0.0.0.0:3000")).unwrap();
        assert_eq!(addr, SocketAddr::from(([0, 0, 0, 0], 3000)));
    }

    #[test]
    fn invalid_non_numeric_port() {
        let err = parse_serve_addr(Some("abc")).unwrap_err();
        assert!(err.contains("invalid address"), "got: {err}");
    }

    #[test]
    fn port_overflow() {
        let err = parse_serve_addr(Some("99999")).unwrap_err();
        assert!(err.contains("invalid port"), "got: {err}");
    }

    #[test]
    fn malformed_host() {
        let err = parse_serve_addr(Some("not_valid:xyz")).unwrap_err();
        assert!(err.contains("invalid address"), "got: {err}");
    }

    // Feature: ag-ui-server, Property 7: Serve address parsing
    // **Validates: Requirements 1.1, 1.2, 1.3**
    proptest! {
        #[test]
        fn prop_port_only_parses_to_localhost(port in 1u16..=65535) {
            let input = port.to_string();
            let addr = parse_serve_addr(Some(&input)).unwrap();
            prop_assert_eq!(addr, SocketAddr::from(([127, 0, 0, 1], port)));
        }

        #[test]
        fn prop_valid_ipv4_host_port_parses(
            a in 0u8..=255,
            b in 0u8..=255,
            c in 0u8..=255,
            d in 0u8..=255,
            port in 1u16..=65535,
        ) {
            let input = format!("{a}.{b}.{c}.{d}:{port}");
            let addr = parse_serve_addr(Some(&input)).unwrap();
            prop_assert_eq!(addr, SocketAddr::from(([a, b, c, d], port)));
        }

        #[test]
        fn prop_invalid_inputs_return_err(
            s in "[a-z]{1,5}:[a-z]{1,5}"
        ) {
            // Strings like "abc:def" are never valid socket addrs
            let result = parse_serve_addr(Some(&s));
            prop_assert!(result.is_err());
            let err = result.unwrap_err();
            prop_assert!(err.contains("invalid"), "expected descriptive error, got: {err}");
        }
    }
}
