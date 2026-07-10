//! Shell command execution tool.
//!
//! Runs commands via the system shell with a configurable timeout.
//! Returns ToolOutput::Error on non-zero exit codes.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;

use agent_core::error::ToolError;
use agent_core::tool::{ApprovalRequirement, Concurrency, Tool, ToolContext, ToolOutput};

/// A tool that executes shell commands.
///
/// Commands run via `/bin/sh -c` on Unix systems. The tool returns combined
/// stdout and stderr on success (exit code 0), or a ToolOutput::Error with
/// exit code and stderr on non-zero exit.
///
/// Concurrency: Always Exclusive (shell commands modify state).
/// Timeout: 300 seconds by default.
pub struct ShellTool {
    timeout_secs: u64,
}

impl ShellTool {
    /// Creates a new ShellTool with default timeout of 300 seconds.
    pub fn new() -> Self {
        Self { timeout_secs: 300 }
    }

    /// Creates a new ShellTool with a custom timeout in seconds.
    pub fn with_timeout(timeout_secs: u64) -> Self {
        Self { timeout_secs }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return its output"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    fn concurrency(&self, _input: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'command' field".to_string()))?;

        let result = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&ctx.working_dir)
                // `output()` only pipes stdout/stderr — stdin defaults to
                // inherit, which hands the raw-mode TUI terminal's stdin to
                // the child. Explicitly null it so spawned commands can never
                // contend for or hijack the interactive terminal's input.
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await;

        match result {
            Err(_) => Err(ToolError::Timeout),
            Ok(Err(e)) => Err(ToolError::ExecutionFailed(format!(
                "Failed to spawn process: {}",
                e
            ))),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    let mut combined = stdout;
                    if !stderr.is_empty() {
                        if !combined.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str(&stderr);
                    }
                    Ok(ToolOutput::Text(combined))
                } else {
                    let exit_code = output.status.code().unwrap_or(-1);
                    let mut error_msg = format!("Exit code: {}", exit_code);
                    if !stdout.is_empty() {
                        error_msg.push_str("\nStdout: ");
                        error_msg.push_str(&stdout);
                    }
                    if !stderr.is_empty() {
                        error_msg.push_str("\nStderr: ");
                        error_msg.push_str(&stderr);
                    }
                    Ok(ToolOutput::Error(error_msg))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_context() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            working_dir: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn shell_tool_properties() {
        let tool = ShellTool::new();
        assert_eq!(tool.name(), "shell");
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Exclusive);
        assert_eq!(tool.timeout(), Duration::from_secs(300));
    }

    #[test]
    fn shell_tool_custom_timeout() {
        let tool = ShellTool::with_timeout(60);
        assert_eq!(tool.timeout(), Duration::from_secs(60));
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new();
        let schema = tool.parameters_schema();
        assert!(schema.get("properties").unwrap().get("command").is_some());
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&json!("command")));
    }

    #[tokio::test]
    async fn shell_tool_execute_success() {
        let tool = ShellTool::new();
        let ctx = make_context();
        let result = tool.execute(json!({"command": "echo hello"}), &ctx).await;
        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => assert!(text.contains("hello")),
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn shell_tool_execute_nonzero_exit() {
        let tool = ShellTool::new();
        let ctx = make_context();
        let result = tool.execute(json!({"command": "exit 1"}), &ctx).await;
        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Error(msg) => assert!(msg.contains("Exit code: 1")),
            other => panic!("Expected Error output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn shell_tool_execute_missing_command() {
        let tool = ShellTool::new();
        let ctx = make_context();
        let result = tool.execute(json!({}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("command")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn shell_tool_uses_working_dir() {
        let tool = ShellTool::new();
        let ctx = ToolContext {
            session_id: "test".to_string(),
            working_dir: PathBuf::from("/tmp"),
        };
        let result = tool.execute(json!({"command": "pwd"}), &ctx).await;
        assert!(result.is_ok());
        match result.unwrap() {
            // On macOS /tmp is a symlink to /private/tmp
            ToolOutput::Text(text) => assert!(
                text.trim() == "/tmp" || text.trim() == "/private/tmp",
                "Expected /tmp or /private/tmp, got: {}",
                text.trim()
            ),
            other => panic!("Expected Text output, got {:?}", other),
        }
    }
}
