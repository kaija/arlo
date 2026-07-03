//! File write tool.
//!
//! Writes content to a file, creating parent directories if needed.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use agent_core::error::ToolError;
use agent_core::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// A tool that writes content to files.
///
/// Creates parent directories if they don't exist. Resolves paths
/// relative to the working directory in ToolContext.
///
/// Concurrency: Exclusive (modifies filesystem state).
pub struct FileWriteTool;

impl FileWriteTool {
    /// Creates a new FileWriteTool.
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileWriteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating parent directories if needed"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to write to (relative to working directory or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn concurrency(&self, _input: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' field".to_string()))?;

        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'content' field".to_string()))?;

        let path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            ctx.working_dir.join(path_str)
        };

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to create directories for '{}': {}",
                    path.display(),
                    e
                ))
            })?;
        }

        tokio::fs::write(&path, content).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path.display(), e))
        })?;

        Ok(ToolOutput::Text(format!("File written: {}", path.display())))
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
    fn file_write_tool_properties() {
        let tool = FileWriteTool::new();
        assert_eq!(tool.name(), "file_write");
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Exclusive);
    }

    #[test]
    fn file_write_tool_schema_has_required_fields() {
        let tool = FileWriteTool::new();
        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("path").is_some());
        assert!(props.get("content").is_some());
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("content")));
    }

    #[tokio::test]
    async fn file_write_tool_writes_file() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriteTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(json!({"path": "output.txt", "content": "hello world"}), &ctx)
            .await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(msg) => assert!(msg.contains("File written:")),
            other => panic!("Expected Text output, got {:?}", other),
        }

        let content = fs::read_to_string(dir.path().join("output.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn file_write_tool_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let tool = FileWriteTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(
                json!({"path": "nested/dir/file.txt", "content": "deep content"}),
                &ctx,
            )
            .await;

        assert!(result.is_ok());
        let content = fs::read_to_string(dir.path().join("nested/dir/file.txt"))
            .await
            .unwrap();
        assert_eq!(content, "deep content");
    }

    #[tokio::test]
    async fn file_write_tool_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("existing.txt");
        fs::write(&file_path, "old content").await.unwrap();

        let tool = FileWriteTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(
                json!({"path": "existing.txt", "content": "new content"}),
                &ctx,
            )
            .await;

        assert!(result.is_ok());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "new content");
    }

    #[tokio::test]
    async fn file_write_tool_missing_path() {
        let tool = FileWriteTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool.execute(json!({"content": "stuff"}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("path")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_write_tool_missing_content() {
        let tool = FileWriteTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool.execute(json!({"path": "file.txt"}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("content")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
}
