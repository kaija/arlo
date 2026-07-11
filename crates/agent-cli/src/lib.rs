//! Library surface for `agent-cli`.
//!
//! `main.rs` is a thin binary over this crate. The `web` module is exposed
//! here (rather than declared privately inside `main.rs`, the way `tui` is)
//! so that `crates/agent-cli/tests/*.rs` integration tests can drive it —
//! Rust integration tests can only depend on a package's library target.

pub mod web;
