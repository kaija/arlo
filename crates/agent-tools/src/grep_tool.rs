//! Grep tool for searching file contents with regex patterns.
//!
//! Searches files in the working directory for lines matching a regex pattern.

use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};

use agent_core::error::ToolError;
use agent_core::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// A tool that searches file contents with regex patterns.
///
/// Searches recursively in the working directory by default.
/// Returns matching lines in `file:line:content` format.
///
/// Concurrency: Safe (read-only operation).
pub struct GrepTool;

impl GrepTool {
    /// Creates a new GrepTool.
    pub fn new() -> Self {
        Self
    }
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents for lines matching a regex pattern"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Optional file or directory path to search in (defaults to working directory)"
                },
                "include": {
                    "type": "string",
                    "description": "Optional glob pattern to filter files (e.g., '*.rs')"
                }
            },
            "required": ["pattern"]
        })
    }

    fn concurrency(&self, _input: &Value) -> Concurrency {
        Concurrency::Safe
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(60)
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let pattern_str = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'pattern' field".to_string()))?;

        let regex = Regex::new(pattern_str).map_err(|e| {
            ToolError::InvalidInput(format!("Invalid regex pattern '{}': {}", pattern_str, e))
        })?;

        let search_path = if let Some(path_str) = input.get("path").and_then(|v| v.as_str()) {
            if std::path::Path::new(path_str).is_absolute() {
                std::path::PathBuf::from(path_str)
            } else {
                ctx.working_dir.join(path_str)
            }
        } else {
            ctx.working_dir.clone()
        };

        let include_pattern = input
            .get("include")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Run the search in a blocking task since it's filesystem-intensive
        let results = tokio::task::spawn_blocking(move || {
            search_files(&search_path, &regex, include_pattern.as_deref())
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Task join error: {}", e)))?
        .map_err(|e| ToolError::ExecutionFailed(e))?;

        Ok(ToolOutput::Text(results.join("\n")))
    }
}

/// Recursively search files in a directory for lines matching the regex.
fn search_files(
    path: &std::path::Path,
    regex: &Regex,
    include_pattern: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut results = Vec::new();

    if path.is_file() {
        search_single_file(path, regex, &mut results);
        return Ok(results);
    }

    if !path.is_dir() {
        return Err(format!("Path '{}' is not a file or directory", path.display()));
    }

    // Build the glob matcher for include filter if provided
    let include_glob = include_pattern
        .map(|p| glob::Pattern::new(p))
        .transpose()
        .map_err(|e| format!("Invalid include pattern: {}", e))?;

    walk_directory(path, regex, &include_glob, &mut results);
    Ok(results)
}

/// Walk a directory recursively, searching each file.
fn walk_directory(
    dir: &std::path::Path,
    regex: &Regex,
    include_glob: &Option<glob::Pattern>,
    results: &mut Vec<String>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Skip hidden files/directories
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }

        if path.is_dir() {
            walk_directory(&path, regex, include_glob, results);
        } else if path.is_file() {
            // Apply include filter
            if let Some(ref pattern) = include_glob {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if !pattern.matches(file_name) {
                        continue;
                    }
                }
            }

            search_single_file(&path, regex, results);
        }
    }
}

/// Search a single file for matching lines.
fn search_single_file(path: &std::path::Path, regex: &Regex, results: &mut Vec<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return, // Skip files that can't be read (binary files, permission errors)
    };

    for (line_num, line) in content.lines().enumerate() {
        if regex.is_match(line) {
            results.push(format!("{}:{}:{}", path.display(), line_num + 1, line));
        }
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
    fn grep_tool_properties() {
        let tool = GrepTool::new();
        assert_eq!(tool.name(), "grep");
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Safe);
    }

    #[test]
    fn grep_tool_schema_has_pattern() {
        let tool = GrepTool::new();
        let schema = tool.parameters_schema();
        assert!(schema.get("properties").unwrap().get("pattern").is_some());
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&json!("pattern")));
    }

    #[tokio::test]
    async fn grep_tool_finds_matching_lines() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("test.txt"),
            "hello world\nfoo bar\nhello again\n",
        )
        .await
        .unwrap();

        let tool = GrepTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool.execute(json!({"pattern": "hello"}), &ctx).await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => {
                let lines: Vec<&str> = text.lines().collect();
                assert_eq!(lines.len(), 2);
                assert!(lines[0].contains(":1:hello world"));
                assert!(lines[1].contains(":3:hello again"));
            }
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn grep_tool_regex_pattern() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("code.rs"),
            "fn main() {}\nfn helper() {}\nstruct Foo;\n",
        )
        .await
        .unwrap();

        let tool = GrepTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool.execute(json!({"pattern": "^fn \\w+"}), &ctx).await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => {
                let lines: Vec<&str> = text.lines().collect();
                assert_eq!(lines.len(), 2);
                assert!(lines[0].contains("fn main()"));
                assert!(lines[1].contains("fn helper()"));
            }
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn grep_tool_with_include_filter() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file.rs"), "fn hello()\n").await.unwrap();
        fs::write(dir.path().join("file.txt"), "fn goodbye()\n").await.unwrap();

        let tool = GrepTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool
            .execute(json!({"pattern": "fn", "include": "*.rs"}), &ctx)
            .await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => {
                assert!(text.contains("file.rs"));
                assert!(!text.contains("file.txt"));
            }
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn grep_tool_single_file_path() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("target.txt"), "match here\nno match\nmatch again\n")
            .await
            .unwrap();

        let tool = GrepTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool
            .execute(json!({"pattern": "match", "path": "target.txt"}), &ctx)
            .await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => {
                let lines: Vec<&str> = text.lines().collect();
                assert_eq!(lines.len(), 3); // "match here", "no match", "match again"
            }
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn grep_tool_no_matches() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("file.txt"), "hello world\n")
            .await
            .unwrap();

        let tool = GrepTool::new();
        let ctx = make_context_with_dir(dir.path());
        let result = tool.execute(json!({"pattern": "xyz123"}), &ctx).await;

        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(text) => assert!(text.is_empty()),
            other => panic!("Expected Text output, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn grep_tool_invalid_regex() {
        let tool = GrepTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool.execute(json!({"pattern": "[invalid"}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("Invalid regex")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn grep_tool_missing_pattern() {
        let tool = GrepTool::new();
        let ctx = make_context_with_dir(&PathBuf::from("/tmp"));
        let result = tool.execute(json!({}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("pattern")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
}
