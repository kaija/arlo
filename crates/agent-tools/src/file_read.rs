//! File read tool.
//!
//! Reads the contents of a file from the filesystem.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use agent_core::error::ToolError;
use agent_core::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// A tool that reads file contents.
///
/// Resolves paths relative to the working directory in ToolContext.
/// Returns the file content as ToolOutput::Text on success.
///
/// Concurrency: Safe (read-only operation).
pub struct FileReadTool;

impl FileReadTool {
    /// Creates a new FileReadTool.
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to read (relative to working directory or absolute)"
                }
            },
            "required": ["path"]
        })
    }

    fn concurrency(&self, _input: &Value) -> Concurrency {
        Concurrency::Safe
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' field".to_string()))?;

        let path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            ctx.working_dir.join(path_str)
        };

        let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read '{}': {}", path.display(), e))
        })?;

        Ok(ToolOutput::Text(content))
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
    fn file_read_tool_properties() {
        let tool = FileReadTool::new();
        assert_eq!(tool.name(), "file_read");
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Safe);
    }

    #[test]
    fn file_read_tool_schema_has_path() {
        let tool = FileReadTool::new();
        let schema = tool.parameters_schema();
        assert!(schema.get("properties").unwrap().get("path").is_some());
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&json!("path")));
    }

    #[tokio::test]
    async fn file_read_tool_reads_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello world").await.unwrap();

        let tool = FileReadTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool.execute(json!({"path": "test.txt"}), &ctx).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), ToolOutput::Text("hello world".to_string()));
    }

    #[tokio::test]
    async fn file_read_tool_absolute_path() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("abs_test.txt");
        fs::write(&file_path, "absolute content").await.unwrap();

        let tool = FileReadTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool
            .execute(json!({"path": file_path.to_str().unwrap()}), &ctx)
            .await;

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            ToolOutput::Text("absolute content".to_string())
        );
    }

    #[tokio::test]
    async fn file_read_tool_nonexistent_file() {
        let dir = TempDir::new().unwrap();
        let tool = FileReadTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool
            .execute(json!({"path": "does_not_exist.txt"}), &ctx)
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("does_not_exist.txt"));
            }
            other => panic!("Expected ExecutionFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_read_tool_missing_path_field() {
        let tool = FileReadTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool.execute(json!({}), &ctx).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("path")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
}
