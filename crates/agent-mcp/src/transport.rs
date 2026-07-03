//! MCP transport configuration types.

use std::collections::HashMap;

/// The transport mechanism used to communicate with an MCP server.
#[derive(Debug, Clone)]
pub enum MCPTransport {
    /// Stdio transport — communicates via stdin/stdout of a child process.
    Stdio {
        /// The command to execute.
        command: String,
        /// Arguments to pass to the command.
        args: Vec<String>,
        /// Optional environment variables for the child process.
        env: HashMap<String, String>,
    },

    /// HTTP transport — communicates via HTTP requests.
    Http {
        /// The base URL of the MCP server.
        url: String,
        /// Optional HTTP headers to include in requests.
        headers: HashMap<String, String>,
    },

    /// Server-Sent Events transport — communicates via SSE stream.
    Sse {
        /// The SSE endpoint URL.
        url: String,
    },
}

impl MCPTransport {
    /// Returns a human-readable name for the transport type.
    pub fn transport_type(&self) -> &str {
        match self {
            MCPTransport::Stdio { .. } => "stdio",
            MCPTransport::Http { .. } => "http",
            MCPTransport::Sse { .. } => "sse",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_transport_type() {
        let transport = MCPTransport::Stdio {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "server".to_string()],
            env: HashMap::new(),
        };
        assert_eq!(transport.transport_type(), "stdio");
    }

    #[test]
    fn http_transport_type() {
        let transport = MCPTransport::Http {
            url: "http://localhost:3000".to_string(),
            headers: HashMap::new(),
        };
        assert_eq!(transport.transport_type(), "http");
    }

    #[test]
    fn sse_transport_type() {
        let transport = MCPTransport::Sse {
            url: "http://localhost:3000/events".to_string(),
        };
        assert_eq!(transport.transport_type(), "sse");
    }

    #[test]
    fn transport_is_clone() {
        let transport = MCPTransport::Stdio {
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            env: HashMap::from([("KEY".to_string(), "value".to_string())]),
        };
        let cloned = transport.clone();
        assert_eq!(cloned.transport_type(), "stdio");
    }

    #[test]
    fn transport_is_debug() {
        let transport = MCPTransport::Http {
            url: "http://example.com".to_string(),
            headers: HashMap::new(),
        };
        let debug = format!("{:?}", transport);
        assert!(debug.contains("Http"));
        assert!(debug.contains("http://example.com"));
    }
}
