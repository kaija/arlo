//! MCP server client trait and tool conversion utilities.
//!
//! Defines the async `MCPServer` trait for connecting to MCP servers,
//! the `MCPToolDefinition` describing tools exposed by servers, and
//! the `MCPToolWrapper` that adapts MCP tools to the agent-core `Tool` trait.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use agent_core::error::ToolError;
use agent_core::tool::{ApprovalRequirement, Concurrency, Tool, ToolContext, ToolOutput};

use crate::error::MCPError;

/// A tool definition as reported by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MCPToolDefinition {
    /// The name of the tool.
    pub name: String,
    /// A description of what the tool does.
    pub description: String,
    /// The JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
}

/// A JSON-RPC request to send to an MCP server.
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    params: serde_json::Value,
}

/// A JSON-RPC response received from an MCP server.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: u64,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

/// A JSON-RPC error object.
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

/// The connection timeout for MCP servers in seconds.
pub const CONNECTION_TIMEOUT_SECS: u64 = 30;

/// The async trait that all MCP server implementations must implement.
///
/// Provides methods to connect to the server, discover available tools,
/// invoke tools, and cleanly disconnect.
#[async_trait]
pub trait MCPServer: Send + Sync {
    /// Returns the configured name of this MCP server.
    fn name(&self) -> &str;

    /// Establishes a connection to the MCP server.
    ///
    /// Must be called before `list_tools()` or `call_tool()`.
    /// Returns `MCPError::ConnectionTimeout` if connection is not established
    /// within 30 seconds.
    async fn connect(&mut self) -> Result<(), MCPError>;

    /// Lists the tools available on this MCP server.
    ///
    /// Returns `MCPError::NotConnected` if called before a successful `connect()`.
    async fn list_tools(&self) -> Result<Vec<MCPToolDefinition>, MCPError>;

    /// Invokes a tool on the MCP server via JSON-RPC.
    ///
    /// Returns `MCPError::NotConnected` if called before a successful `connect()`.
    /// Returns `MCPError::JsonRpc` if the server responds with an error.
    async fn call_tool(
        &self,
        name: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, MCPError>;

    /// Closes the connection to the MCP server.
    async fn close(&mut self) -> Result<(), MCPError>;
}

/// A wrapper that adapts an MCP tool (on a connected server) to the agent-core `Tool` trait.
///
/// This allows MCP tools to be used interchangeably with native tools in the
/// agent framework.
pub struct MCPToolWrapper {
    /// The server connection (behind a RwLock for shared access).
    server: Arc<RwLock<dyn MCPServer>>,
    /// The tool definition from the MCP server.
    definition: MCPToolDefinition,
    /// The name of the MCP server (for error context).
    server_name: String,
}

impl MCPToolWrapper {
    /// Creates a new MCPToolWrapper.
    pub fn new(
        server: Arc<RwLock<dyn MCPServer>>,
        definition: MCPToolDefinition,
        server_name: String,
    ) -> Self {
        Self {
            server,
            definition,
            server_name,
        }
    }
}

#[async_trait]
impl Tool for MCPToolWrapper {
    fn name(&self) -> &str {
        &self.definition.name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.definition.input_schema.clone()
    }

    fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
        // MCP tools are generally Safe for concurrent execution since they
        // communicate over a network boundary. Individual servers handle
        // their own concurrency internally.
        Concurrency::Safe
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // MCP tools default to Always requiring approval since they execute
        // on remote servers where the agent has limited visibility into side effects.
        ApprovalRequirement::Always
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let server = self.server.read().await;
        match server.call_tool(&self.definition.name, input).await {
            Ok(result) => Ok(ToolOutput::Structured(result)),
            Err(MCPError::NotConnected { .. }) => Err(ToolError::NotAvailable(format!(
                "MCP server '{}' is not connected",
                self.server_name
            ))),
            Err(MCPError::JsonRpc { code, message, .. }) => Ok(ToolOutput::Error(format!(
                "MCP server '{}' returned error [{}]: {}",
                self.server_name, code, message
            ))),
            Err(e) => Err(ToolError::ExecutionFailed(format!(
                "MCP tool '{}' on server '{}': {}",
                self.definition.name, self.server_name, e
            ))),
        }
    }
}

/// Converts a list of MCP tool definitions from a connected server into
/// `Arc<dyn Tool>` objects compatible with the agent-core framework.
///
/// Each tool definition is wrapped in an `MCPToolWrapper` that delegates
/// execution to the MCP server via JSON-RPC.
pub fn convert_mcp_tools(
    definitions: Vec<MCPToolDefinition>,
    server: Arc<RwLock<dyn MCPServer>>,
    server_name: &str,
) -> Vec<Arc<dyn Tool>> {
    definitions
        .into_iter()
        .map(|def| {
            let wrapper = MCPToolWrapper::new(Arc::clone(&server), def, server_name.to_string());
            Arc::new(wrapper) as Arc<dyn Tool>
        })
        .collect()
}

/// Builds a JSON-RPC request for invoking a tool.
pub fn build_call_tool_request(
    id: u64,
    tool_name: &str,
    input: serde_json::Value,
) -> serde_json::Value {
    let request = JsonRpcRequest {
        jsonrpc: "2.0",
        id,
        method: "tools/call".to_string(),
        params: serde_json::json!({
            "name": tool_name,
            "arguments": input,
        }),
    };
    serde_json::to_value(&request).expect("JsonRpcRequest is always serializable")
}

/// Builds a JSON-RPC request for listing tools.
pub fn build_list_tools_request(id: u64) -> serde_json::Value {
    let request = JsonRpcRequest {
        jsonrpc: "2.0",
        id,
        method: "tools/list".to_string(),
        params: serde_json::json!({}),
    };
    serde_json::to_value(&request).expect("JsonRpcRequest is always serializable")
}

/// Parses a JSON-RPC response, returning the result or an MCPError.
pub fn parse_json_rpc_response(
    server_name: &str,
    response_bytes: &[u8],
) -> Result<serde_json::Value, MCPError> {
    let response: JsonRpcResponse = serde_json::from_slice(response_bytes)
        .map_err(|e| MCPError::Protocol(format!("Failed to parse JSON-RPC response: {}", e)))?;

    if let Some(error) = response.error {
        return Err(MCPError::JsonRpc {
            server: server_name.to_string(),
            code: error.code,
            message: error.message,
        });
    }

    response.result.ok_or_else(|| {
        MCPError::Protocol("JSON-RPC response has neither result nor error".to_string())
    })
}

/// A stub MCP server implementation using stdio transport.
///
/// This struct defines the shape of a stdio-based MCP server client but
/// does not implement actual process communication yet.
pub struct StdioMCPServer {
    /// The configured name of this server.
    server_name: String,
    /// The command to execute.
    command: String,
    /// Arguments to pass to the command.
    args: Vec<String>,
    /// Environment variables for the child process.
    env: std::collections::HashMap<String, String>,
    /// Whether the server is currently connected.
    connected: bool,
}

impl StdioMCPServer {
    /// Creates a new StdioMCPServer configuration.
    pub fn new(
        name: String,
        command: String,
        args: Vec<String>,
        env: std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            server_name: name,
            command,
            args,
            env,
            connected: false,
        }
    }

    /// Returns the configured command.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Returns the configured arguments.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the configured environment variables.
    pub fn env(&self) -> &std::collections::HashMap<String, String> {
        &self.env
    }
}

#[async_trait]
impl MCPServer for StdioMCPServer {
    fn name(&self) -> &str {
        &self.server_name
    }

    async fn connect(&mut self) -> Result<(), MCPError> {
        // Stub: In a real implementation, this would:
        // 1. Spawn the child process with the configured command/args/env
        // 2. Set up stdin/stdout communication channels
        // 3. Send the initialize request
        // 4. Wait for the initialize response (with 30s timeout)
        //
        // For now, we just mark as connected.
        self.connected = true;
        Ok(())
    }

    async fn list_tools(&self) -> Result<Vec<MCPToolDefinition>, MCPError> {
        if !self.connected {
            return Err(MCPError::NotConnected {
                server: self.server_name.clone(),
            });
        }
        // Stub: would send tools/list JSON-RPC request via stdin and
        // read the response from stdout.
        Ok(Vec::new())
    }

    async fn call_tool(
        &self,
        name: &str,
        _input: serde_json::Value,
    ) -> Result<serde_json::Value, MCPError> {
        if !self.connected {
            return Err(MCPError::NotConnected {
                server: self.server_name.clone(),
            });
        }
        // Stub: would send tools/call JSON-RPC request via stdin and
        // read the response from stdout.
        let _ = name;
        Ok(serde_json::json!({"content": [{"type": "text", "text": "stub response"}]}))
    }

    async fn close(&mut self) -> Result<(), MCPError> {
        // Stub: would send shutdown notification and kill the child process.
        self.connected = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn stdio_server_not_connected_list_tools() {
        let server = StdioMCPServer::new(
            "test-server".to_string(),
            "node".to_string(),
            vec!["server.js".to_string()],
            HashMap::new(),
        );
        let result = server.list_tools().await;
        assert!(result.is_err());
        match result.unwrap_err() {
            MCPError::NotConnected { server } => {
                assert_eq!(server, "test-server");
            }
            other => panic!("Expected NotConnected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn stdio_server_not_connected_call_tool() {
        let server = StdioMCPServer::new(
            "my-server".to_string(),
            "python".to_string(),
            vec!["-m".to_string(), "mcp_server".to_string()],
            HashMap::new(),
        );
        let result = server
            .call_tool("some_tool", serde_json::json!({"arg": "value"}))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            MCPError::NotConnected { server } => {
                assert_eq!(server, "my-server");
            }
            other => panic!("Expected NotConnected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn stdio_server_connect_and_list_tools() {
        let mut server = StdioMCPServer::new(
            "test".to_string(),
            "echo".to_string(),
            vec![],
            HashMap::new(),
        );
        assert!(server.connect().await.is_ok());
        let tools = server.list_tools().await;
        assert!(tools.is_ok());
        assert_eq!(tools.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn stdio_server_connect_and_call_tool() {
        let mut server = StdioMCPServer::new(
            "test".to_string(),
            "echo".to_string(),
            vec![],
            HashMap::new(),
        );
        server.connect().await.unwrap();
        let result = server
            .call_tool("my_tool", serde_json::json!({"x": 1}))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn stdio_server_close() {
        let mut server = StdioMCPServer::new(
            "test".to_string(),
            "echo".to_string(),
            vec![],
            HashMap::new(),
        );
        server.connect().await.unwrap();
        assert!(server.close().await.is_ok());
        // After close, operations should fail with NotConnected
        let result = server.list_tools().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn stdio_server_name() {
        let server = StdioMCPServer::new(
            "my-mcp-server".to_string(),
            "cmd".to_string(),
            vec![],
            HashMap::new(),
        );
        assert_eq!(server.name(), "my-mcp-server");
    }

    #[tokio::test]
    async fn stdio_server_accessors() {
        let env = HashMap::from([("KEY".to_string(), "val".to_string())]);
        let server = StdioMCPServer::new(
            "s".to_string(),
            "npx".to_string(),
            vec!["-y".to_string(), "pkg".to_string()],
            env.clone(),
        );
        assert_eq!(server.command(), "npx");
        assert_eq!(server.args(), &["-y", "pkg"]);
        assert_eq!(server.env(), &env);
    }

    #[test]
    fn build_call_tool_request_structure() {
        let req = build_call_tool_request(1, "read_file", serde_json::json!({"path": "/tmp/x"}));
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "tools/call");
        assert_eq!(req["params"]["name"], "read_file");
        assert_eq!(req["params"]["arguments"]["path"], "/tmp/x");
    }

    #[test]
    fn build_list_tools_request_structure() {
        let req = build_list_tools_request(42);
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 42);
        assert_eq!(req["method"], "tools/list");
    }

    #[test]
    fn parse_json_rpc_response_success() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"tools": []}
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let result = parse_json_rpc_response("server", &bytes);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), serde_json::json!({"tools": []}));
    }

    #[test]
    fn parse_json_rpc_response_error() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32601,
                "message": "Method not found"
            }
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let result = parse_json_rpc_response("my-server", &bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            MCPError::JsonRpc {
                server,
                code,
                message,
            } => {
                assert_eq!(server, "my-server");
                assert_eq!(code, -32601);
                assert_eq!(message, "Method not found");
            }
            other => panic!("Expected JsonRpc, got {:?}", other),
        }
    }

    #[test]
    fn parse_json_rpc_response_malformed() {
        let bytes = b"not json at all";
        let result = parse_json_rpc_response("srv", bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            MCPError::Protocol(msg) => {
                assert!(msg.contains("Failed to parse"));
            }
            other => panic!("Expected Protocol, got {:?}", other),
        }
    }

    #[test]
    fn parse_json_rpc_response_no_result_no_error() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let result = parse_json_rpc_response("srv", &bytes);
        assert!(result.is_err());
        match result.unwrap_err() {
            MCPError::Protocol(msg) => {
                assert!(msg.contains("neither result nor error"));
            }
            other => panic!("Expected Protocol, got {:?}", other),
        }
    }

    #[test]
    fn mcp_tool_definition_serde() {
        let def = MCPToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file from disk".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                }
            }),
        };
        let json = serde_json::to_string(&def).unwrap();
        let deserialized: MCPToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "read_file");
        assert_eq!(deserialized.description, "Read a file from disk");
    }

    #[tokio::test]
    async fn convert_mcp_tools_produces_correct_count() {
        let mut server = StdioMCPServer::new(
            "test".to_string(),
            "echo".to_string(),
            vec![],
            HashMap::new(),
        );
        server.connect().await.unwrap();

        let definitions = vec![
            MCPToolDefinition {
                name: "tool_a".to_string(),
                description: "Tool A".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            MCPToolDefinition {
                name: "tool_b".to_string(),
                description: "Tool B".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
        ];

        let server_arc: Arc<RwLock<dyn MCPServer>> = Arc::new(RwLock::new(server));
        let tools = convert_mcp_tools(definitions, server_arc, "test");

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name(), "tool_a");
        assert_eq!(tools[1].name(), "tool_b");
        assert_eq!(tools[0].description(), "Tool A");
        assert_eq!(tools[1].description(), "Tool B");
    }

    #[tokio::test]
    async fn mcp_tool_wrapper_properties() {
        let server = StdioMCPServer::new(
            "srv".to_string(),
            "echo".to_string(),
            vec![],
            HashMap::new(),
        );
        let server_arc: Arc<RwLock<dyn MCPServer>> = Arc::new(RwLock::new(server));

        let def = MCPToolDefinition {
            name: "my_tool".to_string(),
            description: "Does something useful".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "x": { "type": "number" } }
            }),
        };

        let wrapper = MCPToolWrapper::new(Arc::clone(&server_arc), def.clone(), "srv".to_string());

        assert_eq!(wrapper.name(), "my_tool");
        assert_eq!(wrapper.description(), "Does something useful");
        assert_eq!(
            wrapper.concurrency(&serde_json::json!({})),
            Concurrency::Safe
        );
        assert_eq!(wrapper.approval_requirement(), ApprovalRequirement::Always);
        assert!(wrapper.parameters_schema().is_object());
    }

    #[tokio::test]
    async fn mcp_tool_wrapper_execute_not_connected() {
        let server = StdioMCPServer::new(
            "srv".to_string(),
            "echo".to_string(),
            vec![],
            HashMap::new(),
        );
        // Don't connect the server
        let server_arc: Arc<RwLock<dyn MCPServer>> = Arc::new(RwLock::new(server));

        let def = MCPToolDefinition {
            name: "my_tool".to_string(),
            description: "A tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        };

        let wrapper = MCPToolWrapper::new(server_arc, def, "srv".to_string());
        let ctx = ToolContext {
            session_id: "test".to_string(),
            working_dir: std::path::PathBuf::from("/tmp"),
        };

        let result = wrapper.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::NotAvailable(msg) => {
                assert!(msg.contains("srv"));
                assert!(msg.contains("not connected"));
            }
            other => panic!("Expected NotAvailable, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn mcp_tool_wrapper_execute_connected() {
        let mut server = StdioMCPServer::new(
            "srv".to_string(),
            "echo".to_string(),
            vec![],
            HashMap::new(),
        );
        server.connect().await.unwrap();
        let server_arc: Arc<RwLock<dyn MCPServer>> = Arc::new(RwLock::new(server));

        let def = MCPToolDefinition {
            name: "my_tool".to_string(),
            description: "A tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        };

        let wrapper = MCPToolWrapper::new(server_arc, def, "srv".to_string());
        let ctx = ToolContext {
            session_id: "test".to_string(),
            working_dir: std::path::PathBuf::from("/tmp"),
        };

        let result = wrapper.execute(serde_json::json!({"x": 1}), &ctx).await;
        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Structured(val) => {
                assert!(val.get("content").is_some());
            }
            other => panic!("Expected Structured output, got {:?}", other),
        }
    }
}
