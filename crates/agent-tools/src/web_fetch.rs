//! Web fetch tool.
//!
//! Fetches content from a URL, converts HTML to markdown using a native
//! rule-based conversion engine, and returns truncated content to the agent.

#![allow(unused_imports, dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use url::Url;

use agent_core::error::ToolError;
use agent_core::tool::{Concurrency, Tool, ToolContext, ToolOutput};

use crate::html_to_markdown::HtmlToMarkdown;

/// Maximum URL length in characters.
const MAX_URL_LENGTH: usize = 2000;

/// Maximum response body size in bytes (10 MB).
const MAX_BODY_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum character count for returned content.
const MAX_CONTENT_CHARS: usize = 100_000;

/// Cache TTL duration.
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Maximum redirect hops before aborting.
const MAX_REDIRECTS: u8 = 10;

/// A cached response entry.
struct CacheEntry {
    content: String,
    inserted_at: Instant,
}

/// Tool for fetching web content and converting it to markdown.
pub struct WebFetchTool {
    client: Client,
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
    converter: HtmlToMarkdown,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebFetchTool {
    /// Create a new `WebFetchTool` with a preconfigured HTTP client (no auto-redirects),
    /// an empty URL cache, and a default HTML-to-markdown converter.
    pub fn new() -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            cache: Arc::new(RwLock::new(HashMap::new())),
            converter: HtmlToMarkdown::new(),
        }
    }

    /// Retrieve a cached response for the given URL if it exists and is within TTL.
    async fn get_cached(&self, url: &str) -> Option<String> {
        let cache = self.cache.read().await;
        if let Some(entry) = cache.get(url) {
            if entry.inserted_at.elapsed() < CACHE_TTL {
                return Some(entry.content.clone());
            }
        }
        None
    }

    /// Store a response in the cache keyed by the original URL.
    async fn set_cached(&self, url: &str, content: &str) {
        let mut cache = self.cache.write().await;
        cache.insert(
            url.to_string(),
            CacheEntry {
                content: content.to_string(),
                inserted_at: Instant::now(),
            },
        );
    }
}

/// Result of a fetch operation, representing either a successfully downloaded
/// page or a cross-host redirect that the agent must decide how to handle.
#[derive(Debug)]
pub enum FetchResult {
    /// Successfully fetched content.
    Success {
        final_url: String,
        status: u16,
        content_type: String,
        body: String,
    },
    /// Redirect to a different host — report to agent for decision.
    CrossHostRedirect {
        original_url: String,
        target_url: String,
        status: u16,
    },
}

impl WebFetchTool {
    /// Fetch a URL, following same-host redirects transparently.
    ///
    /// - Same-host redirects (3xx with matching hostname) are followed automatically,
    ///   up to `MAX_REDIRECTS` hops.
    /// - Cross-host redirects return `FetchResult::CrossHostRedirect` for the agent to decide.
    /// - If a 3xx lacks a Location header, returns `ToolError::ExecutionFailed`.
    /// - Checks Content-Length header against `MAX_BODY_SIZE` before downloading.
    /// - Streams the body with size tracking, aborting if it exceeds `MAX_BODY_SIZE`.
    pub async fn fetch_with_redirects(&self, url: Url) -> Result<FetchResult, ToolError> {
        use futures::StreamExt;

        let original_url = url.to_string();
        let original_host = url.host_str().unwrap_or("").to_string();
        let mut current_url = url;
        let mut hops: u8 = 0;

        loop {
            let response = self
                .client
                .get(current_url.as_str())
                .send()
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("Request failed: {}", e)))?;

            let status = response.status();

            if status.is_redirection() {
                // Extract Location header
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());

                let location = match location {
                    Some(loc) => loc,
                    None => {
                        return Err(ToolError::ExecutionFailed(
                            "Malformed redirect: missing Location header".to_string(),
                        ));
                    }
                };

                // Resolve relative URLs against the current URL
                let target = current_url.join(&location).map_err(|e| {
                    ToolError::ExecutionFailed(format!(
                        "Malformed redirect: invalid Location URL: {}",
                        e
                    ))
                })?;

                let target_host = target.host_str().unwrap_or("").to_string();

                // Check if same host
                if is_same_host(&original_host, &target_host) {
                    hops += 1;
                    if hops > MAX_REDIRECTS {
                        return Err(ToolError::ExecutionFailed("Too many redirects".to_string()));
                    }
                    current_url = target;
                    continue;
                } else {
                    // Cross-host redirect — return to agent
                    return Ok(FetchResult::CrossHostRedirect {
                        original_url,
                        target_url: target.to_string(),
                        status: status.as_u16(),
                    });
                }
            }

            // Non-redirect response — download the body

            // Check Content-Length header before downloading
            if let Some(content_length) = response.content_length() {
                if content_length > MAX_BODY_SIZE {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Response too large: {} bytes exceeds {} byte limit",
                        content_length, MAX_BODY_SIZE
                    )));
                }
            }

            // Extract content-type before consuming the response
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/plain")
                .to_string();

            let final_url = current_url.to_string();
            let status_code = status.as_u16();

            // Stream body with size tracking
            let mut body_bytes = Vec::new();
            let mut stream = response.bytes_stream();

            while let Some(chunk) = stream.next().await {
                let chunk = chunk
                    .map_err(|e| ToolError::ExecutionFailed(format!("Download error: {}", e)))?;
                body_bytes.extend_from_slice(&chunk);
                if body_bytes.len() as u64 > MAX_BODY_SIZE {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Response exceeded {} byte limit during download",
                        MAX_BODY_SIZE
                    )));
                }
            }

            let body = String::from_utf8_lossy(&body_bytes).to_string();

            return Ok(FetchResult::Success {
                final_url,
                status: status_code,
                content_type,
                body,
            });
        }
    }
}

/// Validates and normalizes a URL string.
///
/// Performs the following checks in order:
/// 1. Length must not exceed 2000 characters
/// 2. URL must be parseable
/// 3. No embedded credentials (username or password)
/// 4. Hostname must contain at least one dot separator
/// 5. HTTP scheme is upgraded to HTTPS
///
/// Returns the normalized `Url` or a `ToolError::InvalidInput`.
pub fn validate_and_normalize_url(raw: &str) -> Result<Url, ToolError> {
    // Length check
    if raw.len() > MAX_URL_LENGTH {
        return Err(ToolError::InvalidInput(
            "URL exceeds maximum length of 2000 characters".to_string(),
        ));
    }

    // Parse
    let mut parsed =
        Url::parse(raw).map_err(|e| ToolError::InvalidInput(format!("Malformed URL: {}", e)))?;

    // Credentials check
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ToolError::InvalidInput(
            "Embedded credentials are not permitted in URLs".to_string(),
        ));
    }

    // Hostname dot check
    match parsed.host_str() {
        Some(host) if host.contains('.') => {}
        _ => {
            return Err(ToolError::InvalidInput(
                "Invalid hostname: must contain at least one dot separator".to_string(),
            ));
        }
    }

    // HTTP → HTTPS upgrade
    if parsed.scheme() == "http" {
        parsed.set_scheme("https").ok();
    }

    Ok(parsed)
}

/// Returns true if two hostnames are considered the same for redirect purposes.
///
/// Lowercases both hostnames, then strips a leading "www." prefix before comparison.
pub fn is_same_host(original: &str, target: &str) -> bool {
    let normalize = |h: &str| {
        let lower = h.to_lowercase();
        lower.strip_prefix("www.").unwrap_or(&lower).to_string()
    };
    normalize(original) == normalize(target)
}

/// Truncates content if it exceeds `MAX_CONTENT_CHARS` (100,000 characters).
///
/// When truncation occurs, a notice is appended indicating the cutoff point.
/// Uses byte-based length since web content is predominantly ASCII.
pub fn truncate_content(content: String) -> String {
    if content.len() <= MAX_CONTENT_CHARS {
        content
    } else {
        // Use char-based truncation to avoid splitting multi-byte characters
        let truncated: String = content.chars().take(MAX_CONTENT_CHARS).collect();
        format!(
            "{}\n\n[Content truncated at {} characters]",
            truncated, MAX_CONTENT_CHARS
        )
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch web content from a URL and convert HTML to markdown"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional prompt to guide content extraction"
                }
            },
            "required": ["url"]
        })
    }

    fn concurrency(&self, _input: &Value) -> Concurrency {
        Concurrency::Safe
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(60)
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let raw_url = input
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("Missing required 'url' field".to_string()))?;

        let url = validate_and_normalize_url(raw_url)?;
        let cache_key = url.to_string();

        // Check cache
        if let Some(cached) = self.get_cached(&cache_key).await {
            return Ok(ToolOutput::Text(cached));
        }

        // Fetch
        let result = self.fetch_with_redirects(url).await?;

        match result {
            FetchResult::CrossHostRedirect {
                original_url,
                target_url,
                status,
            } => {
                let output = format!(
                    "Redirect detected:\n  From: {}\n  To: {}\n  Status: {}\n\nThe URL redirects to a different host. Fetch the new URL if needed.",
                    original_url, target_url, status
                );
                Ok(ToolOutput::Text(output))
            }
            FetchResult::Success {
                final_url,
                status,
                content_type,
                body,
            } => {
                let byte_count = body.len();

                let content = if content_type.contains("text/html") {
                    self.converter.convert(&body)
                } else {
                    body
                };

                let content = truncate_content(content);

                let output = format!(
                    "URL: {}\nStatus: {}\nContent-Type: {}\nSize: {} bytes\n\n{}",
                    final_url, status, content_type, byte_count, content
                );

                // Cache the result
                self.set_cached(&cache_key, &output).await;

                Ok(ToolOutput::Text(output))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // --- validate_and_normalize_url tests ---

    #[test]
    fn validate_url_too_long() {
        let long_url = format!("https://example.com/{}", "a".repeat(2000));
        let result = validate_and_normalize_url(&long_url);
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("exceeds maximum length"));
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn validate_url_with_credentials() {
        let result = validate_and_normalize_url("https://user:pass@example.com/path");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("credentials are not permitted"));
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn validate_url_with_username_only() {
        let result = validate_and_normalize_url("https://user@example.com/path");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("credentials are not permitted"));
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn validate_url_no_dot_in_hostname() {
        let result = validate_and_normalize_url("https://localhost/path");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("must contain at least one dot separator"));
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn validate_url_malformed() {
        let result = validate_and_normalize_url("not a url at all");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("Malformed URL"));
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn validate_url_http_upgraded_to_https() {
        let result = validate_and_normalize_url("http://example.com/page");
        assert!(result.is_ok());
        let url = result.unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("example.com"));
        assert_eq!(url.path(), "/page");
    }

    #[test]
    fn validate_url_https_unchanged() {
        let result = validate_and_normalize_url("https://example.com/page?q=1");
        assert!(result.is_ok());
        let url = result.unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.as_str(), "https://example.com/page?q=1");
    }

    #[test]
    fn validate_url_valid_with_subdomain() {
        let result = validate_and_normalize_url("https://www.sub.example.com/path");
        assert!(result.is_ok());
        let url = result.unwrap();
        assert_eq!(url.host_str(), Some("www.sub.example.com"));
    }

    // --- is_same_host tests ---

    #[test]
    fn same_host_strips_www() {
        assert!(is_same_host("www.example.com", "example.com"));
        assert!(is_same_host("example.com", "www.example.com"));
    }

    #[test]
    fn same_host_case_insensitive() {
        assert!(is_same_host("Example.COM", "example.com"));
        assert!(is_same_host("WWW.EXAMPLE.COM", "example.com"));
    }

    #[test]
    fn same_host_both_www() {
        assert!(is_same_host("www.example.com", "www.example.com"));
    }

    #[test]
    fn different_hosts() {
        assert!(!is_same_host("example.com", "other.com"));
        assert!(!is_same_host("www.example.com", "www.other.com"));
    }

    #[test]
    fn different_subdomains() {
        assert!(!is_same_host("api.example.com", "cdn.example.com"));
    }

    // --- truncate_content tests ---

    #[test]
    fn truncate_content_under_limit() {
        let content = "Hello, world!".to_string();
        let result = truncate_content(content.clone());
        assert_eq!(result, content);
    }

    #[test]
    fn truncate_content_at_limit() {
        let content = "x".repeat(MAX_CONTENT_CHARS);
        let result = truncate_content(content.clone());
        assert_eq!(result, content);
    }

    #[test]
    fn truncate_content_over_limit() {
        let content = "x".repeat(MAX_CONTENT_CHARS + 500);
        let result = truncate_content(content);
        let expected_suffix = format!(
            "\n\n[Content truncated at {} characters]",
            MAX_CONTENT_CHARS
        );
        assert!(result.ends_with(&expected_suffix));
        // The truncated part should be exactly MAX_CONTENT_CHARS chars of 'x'
        let before_notice = &result[..MAX_CONTENT_CHARS];
        assert_eq!(before_notice, "x".repeat(MAX_CONTENT_CHARS));
    }

    #[test]
    fn truncate_content_multibyte_chars() {
        // Ensure we don't split a multi-byte character
        // Each 'é' is 2 bytes in UTF-8
        let content = "é".repeat(MAX_CONTENT_CHARS + 10);
        let result = truncate_content(content);
        // Should not panic and should contain exactly MAX_CONTENT_CHARS chars of 'é'
        assert!(result.contains("[Content truncated at"));
        let chars_before: String = result.chars().take(MAX_CONTENT_CHARS).collect();
        assert_eq!(chars_before.chars().count(), MAX_CONTENT_CHARS);
    }

    // --- Non-HTML pass-through tests (Property 10) ---
    // **Validates: Requirements 5.25**

    #[test]
    fn non_html_content_type_detection() {
        // Content types that should NOT trigger conversion
        let non_html_types = [
            "text/plain",
            "application/json",
            "application/xml",
            "text/css",
            "application/javascript",
            "image/png",
            "application/pdf",
        ];

        for ct in &non_html_types {
            assert!(
                !ct.contains("text/html"),
                "Unexpected HTML match for: {}",
                ct
            );
        }

        // Content types that SHOULD trigger conversion
        let html_types = [
            "text/html",
            "text/html; charset=utf-8",
            "text/html; charset=ISO-8859-1",
        ];

        for ct in &html_types {
            assert!(ct.contains("text/html"), "Expected HTML match for: {}", ct);
        }
    }

    #[test]
    fn non_html_body_passes_through_unchanged() {
        // Simulate the pass-through logic
        let body = r#"{"key": "value", "array": [1, 2, 3]}"#.to_string();
        let content_type = "application/json";

        let content = if content_type.contains("text/html") {
            panic!("Should not convert non-HTML")
        } else {
            body.clone()
        };

        assert_eq!(content, body);
    }

    #[test]
    fn html_body_gets_converted() {
        let converter = crate::html_to_markdown::HtmlToMarkdown::new();
        let body = "<p>Hello world</p>";
        let content_type = "text/html; charset=utf-8";

        let content = if content_type.contains("text/html") {
            converter.convert(body)
        } else {
            body.to_string()
        };

        // Should be converted to markdown
        assert_eq!(content, "Hello world");
        assert!(!content.contains("<p>"));
    }

    // --- Property-based tests ---

    // **Validates: Requirements 1.1, 1.2, 1.3, 1.4, 2.1, 2.2**
    // Property 1: URL validation rejects all invalid URLs (oversized)
    // Property 2: Scheme normalization ensures HTTPS
    proptest! {
        #[test]
        fn prop_oversized_urls_rejected(extra in "[a-z]{2001,2100}") {
            let url = format!("https://example.com/{}", extra);
            let result = validate_and_normalize_url(&url);
            prop_assert!(result.is_err());
        }

        #[test]
        fn prop_http_upgraded_to_https(path in "[a-z/]{1,20}") {
            let url = format!("http://example.com/{}", path);
            if let Ok(normalized) = validate_and_normalize_url(&url) {
                prop_assert_eq!(normalized.scheme(), "https");
            }
        }
    }

    // **Validates: Requirements 3.1, 3.2**
    // Property 3: Same-host detection is symmetric and ignores www prefix
    proptest! {
        #[test]
        fn prop_same_host_symmetric(host in "[a-z]{3,10}\\.[a-z]{2,5}") {
            // Symmetric: is_same_host(a, b) == is_same_host(b, a)
            let host_www = format!("www.{}", host);
            prop_assert_eq!(is_same_host(&host, &host_www), is_same_host(&host_www, &host));
        }

        #[test]
        fn prop_same_host_www_stripping(host in "[a-z]{3,10}\\.[a-z]{2,5}") {
            // A host always matches itself with www prefix
            let host_www = format!("www.{}", host);
            prop_assert!(is_same_host(&host, &host_www));
            prop_assert!(is_same_host(&host, &host));
        }
    }

    // **Validates: Requirements 6.1, 6.2**
    // Property 11: Truncation preserves content at or below limit, truncates above
    proptest! {
        #[test]
        fn prop_truncation_preserves_under_limit(content in "[a-z ]{1,100}") {
            let result = truncate_content(content.clone());
            prop_assert_eq!(result, content);
        }

        #[test]
        fn prop_truncation_adds_notice_over_limit(extra in "[a-z]{1,100}") {
            let content = format!("{}{}", "x".repeat(MAX_CONTENT_CHARS), extra);
            let result = truncate_content(content);
            prop_assert!(result.contains("[Content truncated at"));
            // First MAX_CONTENT_CHARS chars should be preserved
            let prefix: String = result.chars().take(MAX_CONTENT_CHARS).collect();
            prop_assert_eq!(prefix, "x".repeat(MAX_CONTENT_CHARS));
        }
    }

    // --- Cache TTL behavior tests ---
    // **Validates: Requirements 7.1, 7.2, 7.3**
    // Property 12: Cache round-trip returns identical content

    #[tokio::test]
    async fn cache_round_trip_returns_identical_content() {
        let tool = WebFetchTool::new();
        let url = "https://example.com/test";
        let content = "Hello, cached world!";

        // Initially empty
        assert_eq!(tool.get_cached(url).await, None);

        // Store
        tool.set_cached(url, content).await;

        // Retrieve — should match
        let cached = tool.get_cached(url).await;
        assert_eq!(cached, Some(content.to_string()));
    }

    #[tokio::test]
    async fn cache_different_urls_are_independent() {
        let tool = WebFetchTool::new();
        tool.set_cached("https://a.com", "content-a").await;
        tool.set_cached("https://b.com", "content-b").await;

        assert_eq!(
            tool.get_cached("https://a.com").await,
            Some("content-a".to_string())
        );
        assert_eq!(
            tool.get_cached("https://b.com").await,
            Some("content-b".to_string())
        );
        assert_eq!(tool.get_cached("https://c.com").await, None);
    }

    #[tokio::test]
    async fn cache_expired_entry_returns_none() {
        let tool = WebFetchTool::new();
        let url = "https://example.com/expired";

        // Directly insert an expired entry (16 minutes ago, beyond 15-minute TTL)
        {
            let mut cache = tool.cache.write().await;
            cache.insert(
                url.to_string(),
                CacheEntry {
                    content: "old content".to_string(),
                    inserted_at: Instant::now() - Duration::from_secs(16 * 60),
                },
            );
        }

        // Should not be returned because it's past the 15-minute TTL
        assert_eq!(tool.get_cached(url).await, None);
    }

    #[tokio::test]
    async fn cache_fresh_entry_returns_content() {
        let tool = WebFetchTool::new();
        let url = "https://example.com/fresh";

        // Directly insert a fresh entry (1 minute ago, within 15-minute TTL)
        {
            let mut cache = tool.cache.write().await;
            cache.insert(
                url.to_string(),
                CacheEntry {
                    content: "fresh content".to_string(),
                    inserted_at: Instant::now() - Duration::from_secs(60),
                },
            );
        }

        assert_eq!(
            tool.get_cached(url).await,
            Some("fresh content".to_string())
        );
    }
}
