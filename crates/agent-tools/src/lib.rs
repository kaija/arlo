//! agent-tools: Built-in tool implementations for the arlo-rust agent framework.
//!
//! Provides essential tools for coding agents:
//! - [`ShellTool`] — Execute shell commands
//! - [`FileReadTool`] — Read file contents
//! - [`FileWriteTool`] — Write content to files
//! - [`GlobTool`] — Find files matching glob patterns
//! - [`GrepTool`] — Search file contents with regex

pub mod file_read;
pub mod file_write;
pub mod glob_tool;
pub mod grep_tool;
pub mod shell;

pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob_tool::GlobTool;
pub use grep_tool::GrepTool;
pub use shell::ShellTool;

pub use agent_core;
