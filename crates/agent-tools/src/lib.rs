//! agent-tools: Built-in tool implementations for the arlo-rust agent framework.
//!
//! Provides essential tools for coding agents:
//! - [`ShellTool`] — Execute shell commands
//! - [`FileReadTool`] — Read file contents
//! - [`FileWriteTool`] — Write content to files
//! - [`FileEditTool`] — Replace an exact string match within an existing file
//! - [`GlobTool`] — Find files matching glob patterns
//! - [`GrepTool`] — Search file contents with regex
//! - [`WebFetchTool`] — Fetch web content and convert to markdown
//! - [`WebSearchTool`] — Search the web using configurable providers
//! - [`HtmlToMarkdown`] — Convert HTML to CommonMark markdown

pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod glob_tool;
pub mod grep_tool;
pub mod html_to_markdown;
pub mod shell;
pub mod web_fetch;
pub mod web_search;

pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob_tool::GlobTool;
pub use grep_tool::GrepTool;
pub use html_to_markdown::HtmlToMarkdown;
pub use shell::ShellTool;
pub use web_fetch::WebFetchTool;
pub use web_search::{BraveSearchProvider, SearchProvider, SearchResult, WebSearchTool};

pub use agent_core;
