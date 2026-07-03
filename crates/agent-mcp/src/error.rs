//! Error types for MCP client operations.

use thiserror::Error;

/// Errors that can occur during MCP server interactions.
#[derive(Error, Debug)]
pub enum MCPError {
    /// Connection to the MCP server timed out.
    #[error("Connection timeout to MCP server '{server}' via {transport} transport")]
    ConnectionTimeout {
        /// The name of the MCP server.
        server: String,
        /// The transport type that was used (e.g., "stdio", "http", "sse").
        transport: String,
    },

    /// Operation attempted before a successful connection was established.
    #[error("MCP server '{server}' is not connected")]
    NotConnected {
        /// The name of the MCP server.
        server: String,
    },

    /// The MCP server returned a JSON-RPC error response.
    #[error("JSON-RPC error from MCP server '{server}': [{code}] {message}")]
    JsonRpc {
        /// The name of the MCP server.
        server: String,
        /// The JSON-RPC error code.
        code: i32,
        /// The error message from the server.
        message: String,
    },

    /// An I/O error occurred during communication.
    #[error("I/O error: {0}")]
    Io(String),

    /// A protocol-level error occurred (malformed messages, unexpected responses).
    #[error("Protocol error: {0}")]
    Protocol(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_timeout_display() {
        let err = MCPError::ConnectionTimeout {
            server: "my-server".to_string(),
            transport: "stdio".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("my-server"));
        assert!(display.contains("stdio"));
        assert!(display.contains("timeout"));
    }

    #[test]
    fn not_connected_display() {
        let err = MCPError::NotConnected {
            server: "test-server".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("test-server"));
        assert!(display.contains("not connected"));
    }

    #[test]
    fn json_rpc_display() {
        let err = MCPError::JsonRpc {
            server: "rpc-server".to_string(),
            code: -32600,
            message: "Invalid Request".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("rpc-server"));
        assert!(display.contains("-32600"));
        assert!(display.contains("Invalid Request"));
    }

    #[test]
    fn io_error_display() {
        let err = MCPError::Io("connection refused".to_string());
        let display = format!("{}", err);
        assert!(display.contains("connection refused"));
    }

    #[test]
    fn protocol_error_display() {
        let err = MCPError::Protocol("unexpected message format".to_string());
        let display = format!("{}", err);
        assert!(display.contains("unexpected message format"));
    }
}
