//! Error hierarchy for the agent framework.
//!
//! Provides structured, typed errors using `thiserror` for ergonomic `?` propagation.
//! `ModelError` and `ToolError` convert into `RunError` via `From` implementations.

use thiserror::Error;

/// Top-level error type for agent run failures.
///
/// Each variant includes context fields so that Display output is descriptive
/// without requiring downcasting.
#[derive(Error, Debug)]
pub enum RunError {
    /// An error originating from the model/LLM layer.
    #[error("Model error: {0}")]
    Model(#[from] ModelError),

    /// An error originating from tool execution.
    #[error("Tool error: {0}")]
    Tool(#[from] ToolError),

    /// The agent exceeded its configured maximum turn limit.
    #[error("Max turns exceeded: {0}")]
    MaxTurns(u32),

    /// The agent's accumulated cost exceeded the configured budget.
    #[error("Budget exceeded: ${0:.4}")]
    BudgetExceeded(f64),

    /// A guardrail check failed, halting the run.
    #[error("Guardrail triggered: {0}")]
    Guardrail(String),

    /// A serialization or deserialization error occurred.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// An error from an MCP server connection or call.
    #[error("MCP error: {0}")]
    MCP(String),

    /// The run was explicitly aborted.
    #[error("Aborted: {0}")]
    Aborted(String),

    /// Recovery strategies were exhausted after repeated attempts.
    #[error("Recovery exhausted after {0} attempts")]
    RecoveryExhausted(u32),
}

/// Errors originating from the model/LLM provider layer.
#[derive(Error, Debug)]
pub enum ModelError {
    /// The API returned a non-success HTTP status.
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },

    /// The provider rate-limited the request.
    #[error("Rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    /// The prompt exceeded the model's context window.
    #[error("Prompt too long: {tokens} tokens")]
    PromptTooLong { tokens: usize },

    /// The model's response hit the maximum output token limit.
    #[error("Max output tokens reached")]
    MaxOutputTokens,

    /// A network or connection error occurred.
    #[error("Connection error: {0}")]
    Connection(String),

    /// The streaming response was interrupted before completion.
    #[error("Stream interrupted: {0}")]
    StreamInterrupted(String),
}

/// Errors originating from tool execution.
#[derive(Error, Debug)]
pub enum ToolError {
    /// The tool received invalid input that failed validation.
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// The tool execution failed with an error.
    #[error("Execution failed: {0}")]
    ExecutionFailed(String),

    /// The tool execution exceeded its configured timeout.
    #[error("Timeout")]
    Timeout,

    /// The requested tool is not available or not registered.
    #[error("Not available: {0}")]
    NotAvailable(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn run_error_display_includes_model_error_context() {
        let err = RunError::Model(ModelError::Api {
            status: 429,
            body: "rate limit exceeded".to_string(),
        });
        let display = format!("{}", err);
        assert!(display.contains("429"));
        assert!(display.contains("rate limit exceeded"));
    }

    #[test]
    fn run_error_display_includes_tool_error_context() {
        let err = RunError::Tool(ToolError::ExecutionFailed("disk full".to_string()));
        let display = format!("{}", err);
        assert!(display.contains("disk full"));
    }

    #[test]
    fn run_error_display_max_turns() {
        let err = RunError::MaxTurns(25);
        let display = format!("{}", err);
        assert!(display.contains("25"));
    }

    #[test]
    fn run_error_display_budget_exceeded() {
        let err = RunError::BudgetExceeded(1.2345);
        let display = format!("{}", err);
        assert!(display.contains("1.2345"));
    }

    #[test]
    fn run_error_display_guardrail() {
        let err = RunError::Guardrail("content_filter".to_string());
        let display = format!("{}", err);
        assert!(display.contains("content_filter"));
    }

    #[test]
    fn run_error_display_serialization() {
        let err = RunError::Serialization("invalid JSON at line 5".to_string());
        let display = format!("{}", err);
        assert!(display.contains("invalid JSON at line 5"));
    }

    #[test]
    fn run_error_display_mcp() {
        let err = RunError::MCP("server 'tools-server' disconnected".to_string());
        let display = format!("{}", err);
        assert!(display.contains("tools-server"));
    }

    #[test]
    fn run_error_display_aborted() {
        let err = RunError::Aborted("user_cancelled".to_string());
        let display = format!("{}", err);
        assert!(display.contains("user_cancelled"));
    }

    #[test]
    fn run_error_display_recovery_exhausted() {
        let err = RunError::RecoveryExhausted(3);
        let display = format!("{}", err);
        assert!(display.contains("3"));
    }

    #[test]
    fn model_error_display_api() {
        let err = ModelError::Api {
            status: 500,
            body: "internal server error".to_string(),
        };
        let display = format!("{}", err);
        assert!(display.contains("500"));
        assert!(display.contains("internal server error"));
    }

    #[test]
    fn model_error_display_rate_limited() {
        let err = ModelError::RateLimited {
            retry_after_ms: 5000,
        };
        let display = format!("{}", err);
        assert!(display.contains("5000"));
    }

    #[test]
    fn model_error_display_prompt_too_long() {
        let err = ModelError::PromptTooLong { tokens: 128000 };
        let display = format!("{}", err);
        assert!(display.contains("128000"));
    }

    #[test]
    fn model_error_display_max_output_tokens() {
        let err = ModelError::MaxOutputTokens;
        let display = format!("{}", err);
        assert!(display.contains("Max output tokens"));
    }

    #[test]
    fn model_error_display_connection() {
        let err = ModelError::Connection("timeout after 30s".to_string());
        let display = format!("{}", err);
        assert!(display.contains("timeout after 30s"));
    }

    #[test]
    fn model_error_display_stream_interrupted() {
        let err = ModelError::StreamInterrupted("connection reset".to_string());
        let display = format!("{}", err);
        assert!(display.contains("connection reset"));
    }

    #[test]
    fn tool_error_display_invalid_input() {
        let err = ToolError::InvalidInput("missing 'path' field".to_string());
        let display = format!("{}", err);
        assert!(display.contains("missing 'path' field"));
    }

    #[test]
    fn tool_error_display_execution_failed() {
        let err = ToolError::ExecutionFailed("process exited with code 1".to_string());
        let display = format!("{}", err);
        assert!(display.contains("process exited with code 1"));
    }

    #[test]
    fn tool_error_display_timeout() {
        let err = ToolError::Timeout;
        let display = format!("{}", err);
        assert!(display.contains("Timeout"));
    }

    #[test]
    fn tool_error_display_not_available() {
        let err = ToolError::NotAvailable("shell_tool".to_string());
        let display = format!("{}", err);
        assert!(display.contains("shell_tool"));
    }

    #[test]
    fn from_model_error_to_run_error() {
        let model_err = ModelError::MaxOutputTokens;
        let run_err: RunError = model_err.into();
        assert!(matches!(run_err, RunError::Model(ModelError::MaxOutputTokens)));
    }

    #[test]
    fn from_tool_error_to_run_error() {
        let tool_err = ToolError::Timeout;
        let run_err: RunError = tool_err.into();
        assert!(matches!(run_err, RunError::Tool(ToolError::Timeout)));
    }

    #[test]
    fn question_mark_operator_with_model_error() {
        fn may_fail() -> Result<(), RunError> {
            let result: Result<(), ModelError> = Err(ModelError::Connection("refused".to_string()));
            result?;
            Ok(())
        }
        let err = may_fail().unwrap_err();
        assert!(matches!(err, RunError::Model(ModelError::Connection(_))));
    }

    #[test]
    fn question_mark_operator_with_tool_error() {
        fn may_fail() -> Result<(), RunError> {
            let result: Result<(), ToolError> =
                Err(ToolError::NotAvailable("grep_tool".to_string()));
            result?;
            Ok(())
        }
        let err = may_fail().unwrap_err();
        assert!(matches!(err, RunError::Tool(ToolError::NotAvailable(_))));
    }

    // Feature: rust-agent-framework, Property 16: RunError Display includes context
    // **Validates: Requirements 17.5**
    //
    // For any RunError variant constructed with specific field values, the Display
    // output shall contain those variant-specific context values.

    proptest! {
        #[test]
        fn prop_run_error_model_api_display_contains_context(
            status in 100u16..600u16,
            body in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = RunError::Model(ModelError::Api { status, body: body.clone() });
            let display = format!("{}", err);
            prop_assert!(display.contains(&status.to_string()),
                "Display '{}' should contain status '{}'", display, status);
            prop_assert!(display.contains(&body),
                "Display '{}' should contain body '{}'", display, body);
        }

        #[test]
        fn prop_run_error_model_rate_limited_display_contains_context(
            retry_after_ms in 0u64..100_000u64,
        ) {
            let err = RunError::Model(ModelError::RateLimited { retry_after_ms });
            let display = format!("{}", err);
            prop_assert!(display.contains(&retry_after_ms.to_string()),
                "Display '{}' should contain retry_after_ms '{}'", display, retry_after_ms);
        }

        #[test]
        fn prop_run_error_model_prompt_too_long_display_contains_context(
            tokens in 1usize..1_000_000usize,
        ) {
            let err = RunError::Model(ModelError::PromptTooLong { tokens });
            let display = format!("{}", err);
            prop_assert!(display.contains(&tokens.to_string()),
                "Display '{}' should contain tokens '{}'", display, tokens);
        }

        #[test]
        fn prop_run_error_model_connection_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = RunError::Model(ModelError::Connection(msg.clone()));
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_run_error_model_stream_interrupted_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = RunError::Model(ModelError::StreamInterrupted(msg.clone()));
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_run_error_tool_invalid_input_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = RunError::Tool(ToolError::InvalidInput(msg.clone()));
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_run_error_tool_execution_failed_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = RunError::Tool(ToolError::ExecutionFailed(msg.clone()));
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_run_error_tool_not_available_display_contains_context(
            name in "[a-zA-Z0-9_]{1,30}",
        ) {
            let err = RunError::Tool(ToolError::NotAvailable(name.clone()));
            let display = format!("{}", err);
            prop_assert!(display.contains(&name),
                "Display '{}' should contain tool name '{}'", display, name);
        }

        #[test]
        fn prop_run_error_max_turns_display_contains_context(
            turns in 1u32..10_000u32,
        ) {
            let err = RunError::MaxTurns(turns);
            let display = format!("{}", err);
            prop_assert!(display.contains(&turns.to_string()),
                "Display '{}' should contain turns '{}'", display, turns);
        }

        #[test]
        fn prop_run_error_budget_exceeded_display_contains_context(
            budget in 0.0001f64..1000.0f64,
        ) {
            let err = RunError::BudgetExceeded(budget);
            let display = format!("{}", err);
            // The format is ${0:.4}, so check the formatted value is present
            let formatted = format!("{:.4}", budget);
            prop_assert!(display.contains(&formatted),
                "Display '{}' should contain formatted budget '{}'", display, formatted);
        }

        #[test]
        fn prop_run_error_guardrail_display_contains_context(
            reason in "[a-zA-Z0-9_]{1,30}",
        ) {
            let err = RunError::Guardrail(reason.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&reason),
                "Display '{}' should contain reason '{}'", display, reason);
        }

        #[test]
        fn prop_run_error_serialization_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = RunError::Serialization(msg.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_run_error_mcp_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = RunError::MCP(msg.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_run_error_aborted_display_contains_context(
            reason in "[a-zA-Z0-9_]{1,30}",
        ) {
            let err = RunError::Aborted(reason.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&reason),
                "Display '{}' should contain reason '{}'", display, reason);
        }

        #[test]
        fn prop_run_error_recovery_exhausted_display_contains_context(
            attempts in 1u32..100u32,
        ) {
            let err = RunError::RecoveryExhausted(attempts);
            let display = format!("{}", err);
            prop_assert!(display.contains(&attempts.to_string()),
                "Display '{}' should contain attempts '{}'", display, attempts);
        }

        // ModelError Display property tests

        #[test]
        fn prop_model_error_api_display_contains_context(
            status in 100u16..600u16,
            body in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = ModelError::Api { status, body: body.clone() };
            let display = format!("{}", err);
            prop_assert!(display.contains(&status.to_string()),
                "Display '{}' should contain status '{}'", display, status);
            prop_assert!(display.contains(&body),
                "Display '{}' should contain body '{}'", display, body);
        }

        #[test]
        fn prop_model_error_rate_limited_display_contains_context(
            retry_after_ms in 0u64..100_000u64,
        ) {
            let err = ModelError::RateLimited { retry_after_ms };
            let display = format!("{}", err);
            prop_assert!(display.contains(&retry_after_ms.to_string()),
                "Display '{}' should contain retry_after_ms '{}'", display, retry_after_ms);
        }

        #[test]
        fn prop_model_error_prompt_too_long_display_contains_context(
            tokens in 1usize..1_000_000usize,
        ) {
            let err = ModelError::PromptTooLong { tokens };
            let display = format!("{}", err);
            prop_assert!(display.contains(&tokens.to_string()),
                "Display '{}' should contain tokens '{}'", display, tokens);
        }

        #[test]
        fn prop_model_error_connection_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = ModelError::Connection(msg.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_model_error_stream_interrupted_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = ModelError::StreamInterrupted(msg.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        // ToolError Display property tests

        #[test]
        fn prop_tool_error_invalid_input_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = ToolError::InvalidInput(msg.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_tool_error_execution_failed_display_contains_context(
            msg in "[a-zA-Z0-9 _-]{1,50}",
        ) {
            let err = ToolError::ExecutionFailed(msg.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&msg),
                "Display '{}' should contain message '{}'", display, msg);
        }

        #[test]
        fn prop_tool_error_not_available_display_contains_context(
            name in "[a-zA-Z0-9_]{1,30}",
        ) {
            let err = ToolError::NotAvailable(name.clone());
            let display = format!("{}", err);
            prop_assert!(display.contains(&name),
                "Display '{}' should contain tool name '{}'", display, name);
        }
    }
}
