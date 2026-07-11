//! Tool trait and related types for the agent framework.
//!
//! Defines the `Tool` trait that all tools must implement, along with
//! concurrency classification, execution context, output types, and
//! approval requirements.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::ToolError;

/// Concurrency classification for a tool execution.
///
/// Determines whether a tool can safely run in parallel with other tools
/// or requires exclusive access to shared resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Concurrency {
    /// The tool can run in parallel with other Safe tools.
    Safe,
    /// The tool must run alone — no other tools execute concurrently.
    Exclusive,
}

/// Context provided to a tool during execution.
///
/// Contains session-scoped information that tools may need to
/// perform their work correctly.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// The identifier for the current session.
    pub session_id: String,
    /// The working directory for the tool's execution.
    pub working_dir: PathBuf,
}

/// The output produced by a successful tool execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutput {
    /// Plain text output.
    Text(String),
    /// Structured JSON output.
    Structured(serde_json::Value),
    /// An error message to return to the model (non-fatal).
    Error(String),
}

/// Approval requirement for a tool before execution.
///
/// Controls whether the permission engine should prompt the user
/// for approval before executing a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalRequirement {
    /// The tool never requires explicit approval.
    Never,
    /// The tool always requires explicit approval before execution.
    Always,
    /// The tool requires approval under certain conditions described by the rule.
    Conditional(String),
}

/// The core trait that all tools must implement.
///
/// Tools are the primary mechanism for agents to interact with the
/// external world. Each tool declares its schema, concurrency classification,
/// and approval requirements, and implements an async `execute` method.
///
/// # Default Implementations
///
/// Several methods provide sensible defaults:
/// - `timeout()` → 300 seconds
/// - `error_cascades()` → false
/// - `is_enabled()` → true
/// - `approval_requirement()` → `ApprovalRequirement::Never`
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the unique name of this tool.
    fn name(&self) -> &str;

    /// Returns a human-readable description of what this tool does.
    fn description(&self) -> &str;

    /// Returns the JSON Schema describing the tool's input parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Returns the concurrency classification for this tool given the input.
    ///
    /// The input is provided so that classification can be dynamic. For example,
    /// a shell tool might classify `ls` as `Safe` but `rm` as `Exclusive`.
    fn concurrency(&self, input: &serde_json::Value) -> Concurrency;

    /// Executes the tool with the given input and context.
    ///
    /// Returns `Ok(ToolOutput)` on success or `Err(ToolError)` on failure.
    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError>;

    /// Returns the maximum duration this tool is allowed to execute.
    ///
    /// Defaults to 300 seconds.
    fn timeout(&self) -> Duration {
        Duration::from_secs(300)
    }

    /// Whether a failure in this tool should cancel sibling executing tools.
    ///
    /// When `true`, the `StreamingToolExecutor` will cancel all concurrently
    /// running tools if this tool returns an error.
    ///
    /// Defaults to `false`.
    fn error_cascades(&self) -> bool {
        false
    }

    /// Whether this tool is currently enabled and available for use.
    ///
    /// Disabled tools are excluded from the tool list presented to the model.
    ///
    /// Defaults to `true`.
    fn is_enabled(&self) -> bool {
        true
    }

    /// Returns the approval requirement for this tool.
    ///
    /// Controls whether the permission engine prompts the user before execution.
    ///
    /// Defaults to `ApprovalRequirement::Never`.
    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Never
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A simple test tool for verifying trait implementation.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echoes back the input text"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            })
        }

        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }

        async fn execute(
            &self,
            input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::InvalidInput("missing 'text' field".to_string()))?;
            Ok(ToolOutput::Text(text.to_string()))
        }
    }

    /// A tool that uses Exclusive concurrency for dangerous operations.
    struct DangerousTool;

    #[async_trait]
    impl Tool for DangerousTool {
        fn name(&self) -> &str {
            "dangerous"
        }

        fn description(&self) -> &str {
            "A tool that requires exclusive execution"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            json!({ "type": "object" })
        }

        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Exclusive
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Text("done".to_string()))
        }

        fn timeout(&self) -> Duration {
            Duration::from_secs(60)
        }

        fn error_cascades(&self) -> bool {
            true
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn approval_requirement(&self) -> ApprovalRequirement {
            ApprovalRequirement::Always
        }
    }

    /// A tool with dynamic concurrency based on input.
    struct ShellTool;

    #[async_trait]
    impl Tool for ShellTool {
        fn name(&self) -> &str {
            "shell"
        }

        fn description(&self) -> &str {
            "Execute shell commands"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            })
        }

        fn concurrency(&self, input: &serde_json::Value) -> Concurrency {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.starts_with("ls") || cmd.starts_with("cat") || cmd.starts_with("echo") {
                Concurrency::Safe
            } else {
                Concurrency::Exclusive
            }
        }

        async fn execute(
            &self,
            input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            let cmd = input
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::InvalidInput("missing 'command' field".to_string()))?;
            Ok(ToolOutput::Text(format!("executed: {}", cmd)))
        }
    }

    fn make_context() -> ToolContext {
        ToolContext {
            session_id: "test-session-123".to_string(),
            working_dir: PathBuf::from("/tmp/test"),
        }
    }

    #[test]
    fn concurrency_enum_debug_clone_copy_eq() {
        let safe = Concurrency::Safe;
        let exclusive = Concurrency::Exclusive;

        // Clone + Copy
        let safe_copy = safe;
        assert_eq!(safe, safe_copy);

        // PartialEq
        assert_ne!(safe, exclusive);

        // Debug
        let debug = format!("{:?}", safe);
        assert!(debug.contains("Safe"));
        let debug = format!("{:?}", exclusive);
        assert!(debug.contains("Exclusive"));
    }

    #[test]
    fn tool_context_debug_clone() {
        let ctx = make_context();
        let cloned = ctx.clone();
        assert_eq!(ctx.session_id, cloned.session_id);
        assert_eq!(ctx.working_dir, cloned.working_dir);

        let debug = format!("{:?}", ctx);
        assert!(debug.contains("test-session-123"));
        assert!(debug.contains("/tmp/test"));
    }

    #[test]
    fn tool_output_text_variant() {
        let output = ToolOutput::Text("hello".to_string());
        let cloned = output.clone();
        assert_eq!(output, cloned);
    }

    #[test]
    fn tool_output_structured_variant() {
        let output = ToolOutput::Structured(json!({"key": "value"}));
        let cloned = output.clone();
        assert_eq!(output, cloned);
    }

    #[test]
    fn tool_output_error_variant() {
        let output = ToolOutput::Error("something went wrong".to_string());
        let cloned = output.clone();
        assert_eq!(output, cloned);
    }

    #[test]
    fn tool_output_inequality() {
        let a = ToolOutput::Text("hello".to_string());
        let b = ToolOutput::Text("world".to_string());
        assert_ne!(a, b);

        let c = ToolOutput::Error("err".to_string());
        assert_ne!(a, c);
    }

    #[test]
    fn approval_requirement_never() {
        let req = ApprovalRequirement::Never;
        assert_eq!(req.clone(), ApprovalRequirement::Never);
    }

    #[test]
    fn approval_requirement_always() {
        let req = ApprovalRequirement::Always;
        assert_eq!(req.clone(), ApprovalRequirement::Always);
    }

    #[test]
    fn approval_requirement_conditional() {
        let req = ApprovalRequirement::Conditional("when writing to /etc".to_string());
        let cloned = req.clone();
        assert_eq!(req, cloned);

        let debug = format!("{:?}", req);
        assert!(debug.contains("when writing to /etc"));
    }

    #[test]
    fn approval_requirement_inequality() {
        assert_ne!(ApprovalRequirement::Never, ApprovalRequirement::Always);
        assert_ne!(
            ApprovalRequirement::Always,
            ApprovalRequirement::Conditional("rule".to_string())
        );
    }

    #[tokio::test]
    async fn echo_tool_basic_properties() {
        let tool = EchoTool;
        assert_eq!(tool.name(), "echo");
        assert_eq!(tool.description(), "Echoes back the input text");
        assert!(tool.parameters_schema().is_object());
        assert_eq!(tool.concurrency(&json!({"text": "hi"})), Concurrency::Safe);
    }

    #[tokio::test]
    async fn echo_tool_default_implementations() {
        let tool = EchoTool;
        // Default timeout is 300 seconds
        assert_eq!(tool.timeout(), Duration::from_secs(300));
        // Default error_cascades is false
        assert!(!tool.error_cascades());
        // Default is_enabled is true
        assert!(tool.is_enabled());
        // Default approval_requirement is Never
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Never);
    }

    #[tokio::test]
    async fn echo_tool_execute_success() {
        let tool = EchoTool;
        let ctx = make_context();
        let result = tool.execute(json!({"text": "hello"}), &ctx).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ToolOutput::Text("hello".to_string()));
    }

    #[tokio::test]
    async fn echo_tool_execute_invalid_input() {
        let tool = EchoTool;
        let ctx = make_context();
        let result = tool.execute(json!({"wrong": "field"}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("text")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn dangerous_tool_overrides_defaults() {
        let tool = DangerousTool;
        assert_eq!(tool.timeout(), Duration::from_secs(60));
        assert!(tool.error_cascades());
        assert!(tool.is_enabled());
        assert_eq!(tool.approval_requirement(), ApprovalRequirement::Always);
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Exclusive);
    }

    #[tokio::test]
    async fn shell_tool_dynamic_concurrency() {
        let tool = ShellTool;

        // Safe commands
        assert_eq!(
            tool.concurrency(&json!({"command": "ls -la"})),
            Concurrency::Safe
        );
        assert_eq!(
            tool.concurrency(&json!({"command": "cat file.txt"})),
            Concurrency::Safe
        );
        assert_eq!(
            tool.concurrency(&json!({"command": "echo hello"})),
            Concurrency::Safe
        );

        // Exclusive commands
        assert_eq!(
            tool.concurrency(&json!({"command": "rm -rf /tmp/test"})),
            Concurrency::Exclusive
        );
        assert_eq!(
            tool.concurrency(&json!({"command": "git push"})),
            Concurrency::Exclusive
        );
    }

    #[tokio::test]
    async fn shell_tool_execute() {
        let tool = ShellTool;
        let ctx = make_context();
        let result = tool.execute(json!({"command": "ls"}), &ctx).await;
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            ToolOutput::Text("executed: ls".to_string())
        );
    }

    #[test]
    fn tool_trait_is_object_safe() {
        // Verify that Tool can be used as a trait object (dyn Tool)
        fn _accepts_dyn_tool(_tool: &dyn Tool) {}
        fn _accepts_arc_dyn_tool(_tool: std::sync::Arc<dyn Tool>) {}
    }

    #[test]
    fn tool_trait_send_sync_bounds() {
        // Verify that Arc<dyn Tool> is Send + Sync
        fn _assert_send<T: Send>() {}
        fn _assert_sync<T: Sync>() {}
        _assert_send::<std::sync::Arc<dyn Tool>>();
        _assert_sync::<std::sync::Arc<dyn Tool>>();
    }
}
