//! agent-mcp: MCP (Model Context Protocol) client integration for the arlo-rust agent framework.
//!
//! This crate provides the `MCPServer` trait for connecting to MCP servers,
//! transport configuration types, error types, and utilities for converting
//! MCP tool definitions into agent-core compatible `Tool` objects.
//!
//! # Architecture
//!
//! MCP servers expose tools via JSON-RPC over various transports (stdio, HTTP, SSE).
//! This crate defines the abstraction layer that allows the agent framework to
//! discover and invoke these remote tools as if they were native tools.
//!
//! # Example
//!
//! ```ignore
//! use agent_mcp::client::{MCPServer, StdioMCPServer, convert_mcp_tools};
//! use std::sync::Arc;
//! use tokio::sync::RwLock;
//!
//! let mut server = StdioMCPServer::new(
//!     "my-server".to_string(),
//!     "npx".to_string(),
//!     vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
//!     Default::default(),
//! );
//! server.connect().await?;
//! let definitions = server.list_tools().await?;
//! let server_arc = Arc::new(RwLock::new(server));
//! let tools = convert_mcp_tools(definitions, server_arc, "my-server");
//! ```

pub mod client;
pub mod error;
pub mod transport;

// Re-export key types at crate root for convenience.
pub use client::{
    convert_mcp_tools, MCPServer, MCPToolDefinition, MCPToolWrapper, StdioMCPServer,
    CONNECTION_TIMEOUT_SECS,
};
pub use error::MCPError;
pub use transport::MCPTransport;

pub use agent_core;
