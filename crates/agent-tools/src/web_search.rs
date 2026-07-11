//! Web search tool.
//!
//! Provides a trait-based search interface with Brave Search as the default
//! backend implementation. The `SearchProvider` trait enables alternative
//! search backends to be swapped in without modifying the tool itself.

#![allow(unused_imports, dead_code)]

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use agent_core::error::ToolError;
use agent_core::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// Trait for pluggable search backends.
///
/// Implement this trait to provide alternative search engines
/// (e.g., SearXNG, Google Custom Search, etc.).
#[async_trait]
pub trait SearchProvider: Send + Sync {
    /// Execute a search query and return results.
    async fn search(&self, query: &str, count: usize) -> Result<Vec<SearchResult>, ToolError>;
}

/// A single search result from a search provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Title of the search result.
    pub title: String,
    /// URL of the search result.
    pub url: String,
    /// Snippet/description of the search result.
    pub snippet: String,
}

/// Tool that performs web searches using a configurable search provider.
pub struct WebSearchTool {
    provider: Box<dyn SearchProvider>,
}

impl WebSearchTool {
    /// Create a new WebSearchTool with the given search provider.
    pub fn new(provider: Box<dyn SearchProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using a configured search provider"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "count": {
                    "type": "integer",
                    "description": "Number of results to return (default: 10)",
                    "default": 10
                }
            },
            "required": ["query"]
        })
    }

    fn concurrency(&self, _input: &Value) -> Concurrency {
        Concurrency::Safe
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("Missing required 'query' field".to_string()))?;

        let count = input.get("count").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let results = self.provider.search(query, count).await?;

        let json_results: Vec<Value> = results
            .iter()
            .map(|r| {
                json!({
                    "title": r.title,
                    "url": r.url,
                    "snippet": r.snippet,
                })
            })
            .collect();

        Ok(ToolOutput::Structured(Value::Array(json_results)))
    }
}

/// Brave Search API provider implementation.
pub struct BraveSearchProvider {
    api_key: String,
    client: Client,
}

impl BraveSearchProvider {
    /// Create a new BraveSearchProvider with the given API key.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: Client::new(),
        }
    }
}

#[async_trait]
impl SearchProvider for BraveSearchProvider {
    async fn search(&self, query: &str, count: usize) -> Result<Vec<SearchResult>, ToolError> {
        let response = self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("X-Subscription-Token", &self.api_key)
            .query(&[("q", query), ("count", &count.to_string())])
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Brave Search request failed: {}", e))
            })?;

        if !response.status().is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Brave Search API error: HTTP {}",
                response.status()
            )));
        }

        let json: Value = response.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Brave response: {}", e))
        })?;

        Ok(parse_brave_results(&json))
    }
}

/// Parse Brave Search API JSON response into SearchResult structs.
///
/// Extracts the `web.results` array from the response JSON and maps each
/// item to a `SearchResult` using the `title`, `url`, and `description` fields.
/// Returns an empty vec if the expected structure is missing or contains no results.
pub fn parse_brave_results(json: &Value) -> Vec<SearchResult> {
    json.get("web")
        .and_then(|web| web.get("results"))
        .and_then(|results| results.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let title = item.get("title")?.as_str()?.to_string();
                    let url = item.get("url")?.as_str()?.to_string();
                    let snippet = item
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(SearchResult {
                        title,
                        url,
                        snippet,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    /// A mock search provider for testing WebSearchTool.
    struct MockSearchProvider {
        results: Vec<SearchResult>,
    }

    impl MockSearchProvider {
        fn new(results: Vec<SearchResult>) -> Self {
            Self { results }
        }
    }

    #[async_trait]
    impl SearchProvider for MockSearchProvider {
        async fn search(&self, _query: &str, count: usize) -> Result<Vec<SearchResult>, ToolError> {
            Ok(self.results.iter().take(count).cloned().collect())
        }
    }

    /// A mock provider that always returns an error.
    struct FailingSearchProvider;

    #[async_trait]
    impl SearchProvider for FailingSearchProvider {
        async fn search(
            &self,
            _query: &str,
            _count: usize,
        ) -> Result<Vec<SearchResult>, ToolError> {
            Err(ToolError::ExecutionFailed(
                "Search API unavailable".to_string(),
            ))
        }
    }

    fn make_context() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            working_dir: PathBuf::from("/tmp/test"),
        }
    }

    #[test]
    fn parse_brave_results_with_full_response() {
        let json = json!({
            "web": {
                "results": [
                    {
                        "title": "Rust Programming Language",
                        "url": "https://www.rust-lang.org/",
                        "description": "A language empowering everyone to build reliable software."
                    },
                    {
                        "title": "Rust (programming language) - Wikipedia",
                        "url": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
                        "description": "Rust is a multi-paradigm, general-purpose programming language."
                    }
                ]
            }
        });

        let results = parse_brave_results(&json);
        assert_eq!(results.len(), 2);

        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(
            results[0].snippet,
            "A language empowering everyone to build reliable software."
        );

        assert_eq!(results[1].title, "Rust (programming language) - Wikipedia");
        assert_eq!(
            results[1].url,
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
        assert_eq!(
            results[1].snippet,
            "Rust is a multi-paradigm, general-purpose programming language."
        );
    }

    #[test]
    fn parse_brave_results_empty_results_array() {
        let json = json!({
            "web": {
                "results": []
            }
        });

        let results = parse_brave_results(&json);
        assert!(results.is_empty());
    }

    #[test]
    fn parse_brave_results_missing_web_key() {
        let json = json!({
            "query": { "original": "rust" }
        });

        let results = parse_brave_results(&json);
        assert!(results.is_empty());
    }

    #[test]
    fn parse_brave_results_missing_results_key() {
        let json = json!({
            "web": {
                "type": "search"
            }
        });

        let results = parse_brave_results(&json);
        assert!(results.is_empty());
    }

    #[test]
    fn parse_brave_results_missing_description() {
        let json = json!({
            "web": {
                "results": [
                    {
                        "title": "No Description Page",
                        "url": "https://example.com"
                    }
                ]
            }
        });

        let results = parse_brave_results(&json);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "No Description Page");
        assert_eq!(results[0].url, "https://example.com");
        assert_eq!(results[0].snippet, "");
    }

    #[test]
    fn parse_brave_results_skips_items_missing_title() {
        let json = json!({
            "web": {
                "results": [
                    {
                        "url": "https://example.com",
                        "description": "Has no title"
                    },
                    {
                        "title": "Valid Result",
                        "url": "https://valid.com",
                        "description": "This one is fine"
                    }
                ]
            }
        });

        let results = parse_brave_results(&json);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Valid Result");
    }

    #[test]
    fn parse_brave_results_skips_items_missing_url() {
        let json = json!({
            "web": {
                "results": [
                    {
                        "title": "Missing URL",
                        "description": "No url field"
                    },
                    {
                        "title": "Good Result",
                        "url": "https://good.com",
                        "description": "Has everything"
                    }
                ]
            }
        });

        let results = parse_brave_results(&json);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Good Result");
    }

    #[test]
    fn parse_brave_results_null_json() {
        let json = Value::Null;
        let results = parse_brave_results(&json);
        assert!(results.is_empty());
    }

    #[test]
    fn brave_search_provider_new_stores_api_key() {
        let provider = BraveSearchProvider::new("test-key-123".to_string());
        assert_eq!(provider.api_key, "test-key-123");
    }

    // --- WebSearchTool trait implementation tests ---

    #[test]
    fn web_search_tool_name() {
        let provider = MockSearchProvider::new(vec![]);
        let tool = WebSearchTool::new(Box::new(provider));
        assert_eq!(tool.name(), "web_search");
    }

    #[test]
    fn web_search_tool_description() {
        let provider = MockSearchProvider::new(vec![]);
        let tool = WebSearchTool::new(Box::new(provider));
        assert_eq!(
            tool.description(),
            "Search the web using a configured search provider"
        );
    }

    #[test]
    fn web_search_tool_parameters_schema_has_required_query() {
        let provider = MockSearchProvider::new(vec![]);
        let tool = WebSearchTool::new(Box::new(provider));
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert_eq!(schema["properties"]["count"]["type"], "integer");
        assert_eq!(schema["properties"]["count"]["default"], 10);
        assert_eq!(schema["required"], json!(["query"]));
    }

    #[test]
    fn web_search_tool_concurrency_is_safe() {
        let provider = MockSearchProvider::new(vec![]);
        let tool = WebSearchTool::new(Box::new(provider));
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Safe);
    }

    #[test]
    fn web_search_tool_timeout_is_30_seconds() {
        let provider = MockSearchProvider::new(vec![]);
        let tool = WebSearchTool::new(Box::new(provider));
        assert_eq!(tool.timeout(), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn web_search_tool_execute_returns_structured_json_array() {
        let results = vec![
            SearchResult {
                title: "Rust Lang".to_string(),
                url: "https://rust-lang.org".to_string(),
                snippet: "A systems programming language".to_string(),
            },
            SearchResult {
                title: "Rust Book".to_string(),
                url: "https://doc.rust-lang.org/book/".to_string(),
                snippet: "The Rust Programming Language book".to_string(),
            },
        ];
        let provider = MockSearchProvider::new(results);
        let tool = WebSearchTool::new(Box::new(provider));
        let ctx = make_context();

        let result = tool
            .execute(json!({"query": "rust programming"}), &ctx)
            .await;
        assert!(result.is_ok());

        match result.unwrap() {
            ToolOutput::Structured(Value::Array(arr)) => {
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0]["title"], "Rust Lang");
                assert_eq!(arr[0]["url"], "https://rust-lang.org");
                assert_eq!(arr[0]["snippet"], "A systems programming language");
                assert_eq!(arr[1]["title"], "Rust Book");
                assert_eq!(arr[1]["url"], "https://doc.rust-lang.org/book/");
                assert_eq!(arr[1]["snippet"], "The Rust Programming Language book");
            }
            other => panic!("Expected ToolOutput::Structured(Array), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn web_search_tool_execute_missing_query_returns_invalid_input() {
        let provider = MockSearchProvider::new(vec![]);
        let tool = WebSearchTool::new(Box::new(provider));
        let ctx = make_context();

        let result = tool.execute(json!({"count": 5}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("query")),
            other => panic!("Expected ToolError::InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn web_search_tool_execute_default_count_is_10() {
        let results: Vec<SearchResult> = (0..15)
            .map(|i| SearchResult {
                title: format!("Result {}", i),
                url: format!("https://example.com/{}", i),
                snippet: format!("Snippet {}", i),
            })
            .collect();
        let provider = MockSearchProvider::new(results);
        let tool = WebSearchTool::new(Box::new(provider));
        let ctx = make_context();

        let result = tool.execute(json!({"query": "test"}), &ctx).await.unwrap();
        match result {
            ToolOutput::Structured(Value::Array(arr)) => {
                assert_eq!(arr.len(), 10);
            }
            other => panic!("Expected ToolOutput::Structured(Array), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn web_search_tool_execute_respects_count_param() {
        let results: Vec<SearchResult> = (0..10)
            .map(|i| SearchResult {
                title: format!("Result {}", i),
                url: format!("https://example.com/{}", i),
                snippet: format!("Snippet {}", i),
            })
            .collect();
        let provider = MockSearchProvider::new(results);
        let tool = WebSearchTool::new(Box::new(provider));
        let ctx = make_context();

        let result = tool
            .execute(json!({"query": "test", "count": 3}), &ctx)
            .await
            .unwrap();
        match result {
            ToolOutput::Structured(Value::Array(arr)) => {
                assert_eq!(arr.len(), 3);
            }
            other => panic!("Expected ToolOutput::Structured(Array), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn web_search_tool_execute_empty_results() {
        let provider = MockSearchProvider::new(vec![]);
        let tool = WebSearchTool::new(Box::new(provider));
        let ctx = make_context();

        let result = tool
            .execute(json!({"query": "nothing"}), &ctx)
            .await
            .unwrap();
        match result {
            ToolOutput::Structured(Value::Array(arr)) => {
                assert!(arr.is_empty());
            }
            other => panic!("Expected ToolOutput::Structured(Array), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn web_search_tool_execute_propagates_provider_error() {
        let provider = FailingSearchProvider;
        let tool = WebSearchTool::new(Box::new(provider));
        let ctx = make_context();

        let result = tool.execute(json!({"query": "test"}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => assert!(msg.contains("Search API unavailable")),
            other => panic!("Expected ToolError::ExecutionFailed, got {:?}", other),
        }
    }
}
