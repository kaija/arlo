//! Glob pattern matching tool.
//!
//! Finds files matching a glob pattern relative to the working directory.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use agent_core::error::ToolError;
use agent_core::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// A tool that finds files matching glob patterns.
///
/// Uses the `glob` crate to match file patterns. Patterns are resolved
/// relative to the working directory in ToolContext.
///
/// Concurrency: Safe (read-only filesystem operation).
pub struct GlobTool;

impl GlobTool {
    /// Creates a new GlobTool.
    pub fn new() -> Self {
        Self
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match (e.g., '**/*.rs', 'src/*.txt')"
                }
            },
            "required": ["pattern"]
        })
    }

    fn concurrency(&self, _input: &Value) -> Concurrency {
        Concurrency::Safe
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'pattern' field".to_string()))?;

        // Resolve pattern relative to working directory
        let full_pattern = if std::path::Path::new(pattern).is_absolute() {
            pattern.to_string()
        } else {
            format!("{}/{}", ctx.working_dir.display(), pattern)
        };

        // Run glob in a blocking task since it does filesystem I/O
        let matches = tokio::task::spawn_blocking(move || {
            glob::glob(&full_pattern)
                .map_err(|e| ToolError::InvalidInput(format!("Invalid glob pattern: {}", e)))
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok())
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                })
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Task join error: {}", e)))??;

        let result = matches.join("\n");
        Ok(ToolOutput::Text(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;

    fn make_context_with_dir(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            working_dir: dir.to_path_buf(),
        }
    }

    #[test]
    fn glob_tool_properties() {
        let tool = GlobTool::new();
        assert_eq!(tool.name(), "glob");
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Safe);
    }

    #[test]
    fn glob_tool_schema_has_pattern() {
        let tool = GlobTool::new();
        let schema = tool.parameters_schema();
        assert!(schema.get("properties").unwrap().get("pattern").is_some());
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&json!("pattern")));
    }

    #[tokio::test]
    async fn glob_tool_finds_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file1.txt"), "a").await.unwrap();
        fs::write(dir.path().join("file2.txt"), "b").await.unwrap();
        fs::write(dir.path().join("file3.rs"), "c").await.unwrap();

        let tool = GlobTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool.execute(json!({"pattern": "*.txt"}), &ctx).await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => {
                assert!(text.contains("file1.txt"));
                assert!(text.contains("file2.txt"));
                assert!(!text.contains("file3.rs"));
            }
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn glob_tool_recursive_pattern() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("sub")).await.unwrap();
        fs::write(dir.path().join("top.rs"), "a").await.unwrap();
        fs::write(dir.path().join("sub/nested.rs"), "b")
            .await
            .unwrap();

        let tool = GlobTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool.execute(json!({"pattern": "**/*.rs"}), &ctx).await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => {
                assert!(text.contains("top.rs"));
                assert!(text.contains("nested.rs"));
            }
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn glob_tool_no_matches() {
        let dir = TempDir::new().unwrap();
        let tool = GlobTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool.execute(json!({"pattern": "*.xyz"}), &ctx).await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => assert!(text.is_empty()),
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn glob_tool_missing_pattern() {
        let tool = GlobTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool.execute(json!({}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("pattern")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
}
