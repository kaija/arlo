//! File edit tool.
//!
//! Replaces an exact string match within an existing file, avoiding the need
//! to rewrite the entire file contents for a small change.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use agent_core::error::ToolError;
use agent_core::tool::{ApprovalRequirement, Concurrency, Tool, ToolContext, ToolOutput};

/// A tool that performs a targeted string replacement within a file.
///
/// Reads the file, replaces `old_string` with `new_string`, and writes the
/// result back. Requires `old_string` to match exactly once unless
/// `replace_all` is set.
///
/// Concurrency: Exclusive (modifies filesystem state).
pub struct FileEditTool;

impl FileEditTool {
    /// Creates a new FileEditTool.
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileEditTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string match within an existing file"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The file path to edit (relative to working directory or absolute)"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to replace. Must match exactly once unless replace_all is true."
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace old_string with"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences of old_string instead of requiring a single unique match. Defaults to false."
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn concurrency(&self, _input: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' field".to_string()))?;

        let old_string = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'old_string' field".to_string()))?;

        let new_string = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'new_string' field".to_string()))?;

        if old_string.is_empty() {
            return Err(ToolError::InvalidInput(
                "'old_string' must not be empty; use file_write to create a new file".to_string(),
            ));
        }

        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            ctx.working_dir.join(path_str)
        };

        let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read '{}': {}", path.display(), e))
        })?;

        let match_count = content.matches(old_string).count();

        if match_count == 0 {
            return Err(ToolError::ExecutionFailed(format!(
                "old_string not found in '{}'",
                path.display()
            )));
        }

        if match_count > 1 && !replace_all {
            return Err(ToolError::InvalidInput(format!(
                "old_string is not unique in '{}' ({} matches); pass replace_all or add more surrounding context",
                path.display(),
                match_count
            )));
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        tokio::fs::write(&path, new_content).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to write '{}': {}", path.display(), e))
        })?;

        Ok(ToolOutput::Text(format!("File edited: {}", path.display())))
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
    fn file_edit_tool_properties() {
        let tool = FileEditTool::new();
        assert_eq!(tool.name(), "file_edit");
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Exclusive);
    }

    #[test]
    fn file_edit_tool_schema_has_required_fields() {
        let tool = FileEditTool::new();
        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("path").is_some());
        assert!(props.get("old_string").is_some());
        assert!(props.get("new_string").is_some());
        assert!(props.get("replace_all").is_some());
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("old_string")));
        assert!(required.contains(&json!("new_string")));
        assert!(!required.contains(&json!("replace_all")));
    }

    #[tokio::test]
    async fn file_edit_tool_replaces_unique_match() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("doc.md");
        fs::write(&file_path, "# Old Heading\n\nBody text.").await.unwrap();

        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(
                json!({
                    "path": "doc.md",
                    "old_string": "# Old Heading",
                    "new_string": "# New Heading"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(msg) => assert!(msg.contains("File edited:")),
            other => panic!("Expected Text output, got {:?}", other),
        }

        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "# New Heading\n\nBody text.");
    }

    #[tokio::test]
    async fn file_edit_tool_replace_all_multiple_matches() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("doc.md");
        fs::write(&file_path, "foo bar foo baz foo").await.unwrap();

        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(
                json!({
                    "path": "doc.md",
                    "old_string": "foo",
                    "new_string": "qux",
                    "replace_all": true
                }),
                &ctx,
            )
            .await;

        assert!(result.is_ok());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "qux bar qux baz qux");
    }

    #[tokio::test]
    async fn file_edit_tool_ambiguous_match_without_replace_all() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("doc.md");
        fs::write(&file_path, "foo bar foo").await.unwrap();

        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(
                json!({
                    "path": "doc.md",
                    "old_string": "foo",
                    "new_string": "qux"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("not unique")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }

        // File must be unchanged
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "foo bar foo");
    }

    #[tokio::test]
    async fn file_edit_tool_not_found() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("doc.md");
        fs::write(&file_path, "hello world").await.unwrap();

        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(
                json!({
                    "path": "doc.md",
                    "old_string": "does not exist",
                    "new_string": "replacement"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => assert!(msg.contains("not found")),
            other => panic!("Expected ExecutionFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_edit_tool_missing_file() {
        let dir = TempDir::new().unwrap();
        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(
                json!({
                    "path": "does_not_exist.md",
                    "old_string": "a",
                    "new_string": "b"
                }),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("Failed to read"));
            }
            other => panic!("Expected ExecutionFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_edit_tool_missing_path() {
        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool
            .execute(json!({"old_string": "a", "new_string": "b"}), &ctx)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("path")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_edit_tool_missing_old_string() {
        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool
            .execute(json!({"path": "file.txt", "new_string": "b"}), &ctx)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("old_string")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_edit_tool_missing_new_string() {
        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool
            .execute(json!({"path": "file.txt", "old_string": "a"}), &ctx)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("new_string")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn file_edit_tool_rejects_empty_old_string() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("doc.md");
        fs::write(&file_path, "hello world").await.unwrap();

        let tool = FileEditTool::new();
        let ctx = make_context_with_dir(dir.path());

        let result = tool
            .execute(
                json!({"path": "doc.md", "old_string": "", "new_string": "x"}),
                &ctx,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("empty")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
}
