//! Unified LLM provider with feature-flag-gated backends.
//!
//! `UnifiedProvider` implements the `ModelProvider` trait from `agent-core`,
//! routing model name strings to the appropriate backend based on provider
//! prefixes (e.g., "anthropic:claude-sonnet-4-20250514") or the configured default.
//!
//! Provider availability is controlled by feature flags:
//! - "openai" (default): OpenAI-compatible API
//! - "anthropic" (default): Anthropic Messages API
//! - "ollama" (optional): Local Ollama server

use std::sync::Arc;

use async_trait::async_trait;

use agent_core::config_resolver::ResolvedProfile;
use agent_core::error::ModelError;
use agent_core::model::{Model, ModelProvider, ModelRequest, ModelResponse, ModelStream};

use crate::anthropic_http::AnthropicHttpModel;
use crate::openai_http::OpenAIHttpModel;

/// A unified provider that routes model requests to the appropriate backend.
///
/// # Construction
///
/// Use `UnifiedProvider::from_env()` to auto-detect available providers from
/// environment variables, or use the builder methods for manual configuration.
///
/// # Routing
///
/// Model names are routed based on prefix:
/// - `"openai:gpt-4"` → OpenAI provider
/// - `"anthropic:claude-sonnet-4-20250514"` → Anthropic provider
/// - `"ollama:llama2"` → Ollama provider
/// - `"gpt-4"` (no prefix) → default provider
#[derive(Debug, Clone)]
pub struct UnifiedProvider {
    /// Which provider to use for unprefixed model names.
    default_provider: Option<String>,

    /// OpenAI API key (present means OpenAI provider is available).
    #[cfg(feature = "openai")]
    openai_key: Option<String>,

    /// Custom base URL for OpenAI-compatible endpoints.
    #[cfg(feature = "openai")]
    openai_base_url: Option<String>,

    /// Anthropic API key (present means Anthropic provider is available).
    #[cfg(feature = "anthropic")]
    anthropic_key: Option<String>,

    /// Custom base URL for Anthropic-compatible endpoints.
    #[cfg(feature = "anthropic")]
    anthropic_base_url: Option<String>,

    /// Ollama host URL (present means Ollama provider is available).
    #[cfg(feature = "ollama")]
    ollama_host: Option<String>,
}

impl UnifiedProvider {
    /// Create a `UnifiedProvider` by reading environment variables.
    ///
    /// Reads:
    /// - `OPENAI_API_KEY` (when "openai" feature is enabled)
    /// - `ANTHROPIC_API_KEY` (when "anthropic" feature is enabled)
    /// - `OLLAMA_HOST` (when "ollama" feature is enabled)
    ///
    /// The first available provider (in order: openai, anthropic, ollama) becomes
    /// the default provider for unprefixed model names.
    ///
    /// # Errors
    ///
    /// Returns `ModelError::Connection` if no recognized API keys or host
    /// variables are found in the environment.
    pub fn from_env() -> Result<Self, ModelError> {
        #[cfg(feature = "openai")]
        let openai_key = std::env::var("OPENAI_API_KEY").ok();

        #[cfg(feature = "openai")]
        let openai_base_url = std::env::var("OPENAI_BASE_URL").ok();

        #[cfg(feature = "anthropic")]
        let anthropic_key = std::env::var("ANTHROPIC_API_KEY").ok();

        #[cfg(feature = "anthropic")]
        let anthropic_base_url = std::env::var("ANTHROPIC_BASE_URL").ok();

        #[cfg(feature = "ollama")]
        let ollama_host = std::env::var("OLLAMA_HOST").ok();

        // Determine default provider: first available in priority order
        let default_provider = Self::detect_default(
            #[cfg(feature = "openai")]
            &openai_key,
            #[cfg(feature = "anthropic")]
            &anthropic_key,
            #[cfg(feature = "ollama")]
            &ollama_host,
        );

        let provider = Self {
            default_provider,
            #[cfg(feature = "openai")]
            openai_key,
            #[cfg(feature = "openai")]
            openai_base_url,
            #[cfg(feature = "anthropic")]
            anthropic_key,
            #[cfg(feature = "anthropic")]
            anthropic_base_url,
            #[cfg(feature = "ollama")]
            ollama_host,
        };

        if !provider.has_any_provider() {
            return Err(ModelError::Connection(
                "No provider configured: set OPENAI_API_KEY, ANTHROPIC_API_KEY, or OLLAMA_HOST"
                    .to_string(),
            ));
        }

        Ok(provider)
    }

    /// Create a `UnifiedProvider` with explicit configuration.
    ///
    /// This is useful for testing or when you want to configure providers
    /// without relying on environment variables.
    pub fn new(
        default_provider: Option<String>,
        #[cfg(feature = "openai")] openai_key: Option<String>,
        #[cfg(feature = "openai")] openai_base_url: Option<String>,
        #[cfg(feature = "anthropic")] anthropic_key: Option<String>,
        #[cfg(feature = "anthropic")] anthropic_base_url: Option<String>,
        #[cfg(feature = "ollama")] ollama_host: Option<String>,
    ) -> Result<Self, ModelError> {
        let provider = Self {
            default_provider,
            #[cfg(feature = "openai")]
            openai_key,
            #[cfg(feature = "openai")]
            openai_base_url,
            #[cfg(feature = "anthropic")]
            anthropic_key,
            #[cfg(feature = "anthropic")]
            anthropic_base_url,
            #[cfg(feature = "ollama")]
            ollama_host,
        };

        if !provider.has_any_provider() {
            return Err(ModelError::Connection(
                "No provider configured: at least one provider must have credentials".to_string(),
            ));
        }

        Ok(provider)
    }

    /// Construct a `UnifiedProvider` from a resolved profile.
    ///
    /// Uses the profile's `provider`, `api_key`, and `base_url` to configure
    /// the appropriate backend, bypassing environment variable detection.
    ///
    /// # Errors
    ///
    /// Returns `ModelError::Connection` if:
    /// - The provider string is not recognized ("openai", "anthropic", or "ollama")
    /// - No provider credentials are available after construction
    pub fn from_profile(profile: &ResolvedProfile) -> Result<Self, ModelError> {
        let default_provider = Some(profile.provider.clone());

        let provider = match profile.provider.as_str() {
            #[cfg(feature = "openai")]
            "openai" => Self {
                default_provider,
                openai_key: profile.api_key.clone(),
                openai_base_url: profile.base_url.clone(),
                #[cfg(feature = "anthropic")]
                anthropic_key: None,
                #[cfg(feature = "anthropic")]
                anthropic_base_url: None,
                #[cfg(feature = "ollama")]
                ollama_host: None,
            },
            #[cfg(feature = "anthropic")]
            "anthropic" => Self {
                default_provider,
                #[cfg(feature = "openai")]
                openai_key: None,
                #[cfg(feature = "openai")]
                openai_base_url: None,
                anthropic_key: profile.api_key.clone(),
                anthropic_base_url: profile.base_url.clone(),
                #[cfg(feature = "ollama")]
                ollama_host: None,
            },
            #[cfg(feature = "ollama")]
            "ollama" => Self {
                default_provider,
                #[cfg(feature = "openai")]
                openai_key: None,
                #[cfg(feature = "openai")]
                openai_base_url: None,
                #[cfg(feature = "anthropic")]
                anthropic_key: None,
                #[cfg(feature = "anthropic")]
                anthropic_base_url: None,
                ollama_host: profile
                    .base_url
                    .clone()
                    .or_else(|| Some("http://localhost:11434".to_string())),
            },
            other => {
                return Err(ModelError::Connection(format!(
                    "Unknown provider '{}' in profile",
                    other
                )));
            }
        };

        if !provider.has_any_provider() {
            return Err(ModelError::Connection(
                "Profile resolved but no provider credentials available".to_string(),
            ));
        }

        Ok(provider)
    }

    /// Check if any provider is configured and available.
    fn has_any_provider(&self) -> bool {
        #[allow(unused_mut)]
        let mut has_any = false;

        #[cfg(feature = "openai")]
        {
            has_any = has_any || self.openai_key.is_some();
        }

        #[cfg(feature = "anthropic")]
        {
            has_any = has_any || self.anthropic_key.is_some();
        }

        #[cfg(feature = "ollama")]
        {
            has_any = has_any || self.ollama_host.is_some();
        }

        has_any
    }

    /// Determine the default provider based on available credentials.
    /// Priority: openai > anthropic > ollama
    fn detect_default(
        #[cfg(feature = "openai")] openai_key: &Option<String>,
        #[cfg(feature = "anthropic")] anthropic_key: &Option<String>,
        #[cfg(feature = "ollama")] ollama_host: &Option<String>,
    ) -> Option<String> {
        #[cfg(feature = "openai")]
        if openai_key.is_some() {
            return Some("openai".to_string());
        }

        #[cfg(feature = "anthropic")]
        if anthropic_key.is_some() {
            return Some("anthropic".to_string());
        }

        #[cfg(feature = "ollama")]
        if ollama_host.is_some() {
            return Some("ollama".to_string());
        }

        None
    }

    /// Parse a model name into (provider_prefix, model_name).
    ///
    /// Examples:
    /// - "anthropic:claude-sonnet-4-20250514" → Some(("anthropic", "claude-sonnet-4-20250514"))
    /// - "openai:gpt-4" → Some(("openai", "gpt-4"))
    /// - "gpt-4" → None
    fn parse_model_name(model_name: &str) -> Option<(&str, &str)> {
        let known_prefixes = ["openai", "anthropic", "ollama"];
        if let Some(colon_pos) = model_name.find(':') {
            let prefix = &model_name[..colon_pos];
            if known_prefixes.contains(&prefix) {
                let name = &model_name[colon_pos + 1..];
                return Some((prefix, name));
            }
        }
        None
    }

    /// Check if a given provider prefix is available (feature enabled + credentials present).
    fn is_provider_available(&self, provider: &str) -> bool {
        match provider {
            #[cfg(feature = "openai")]
            "openai" => self.openai_key.is_some(),

            #[cfg(feature = "anthropic")]
            "anthropic" => self.anthropic_key.is_some(),

            #[cfg(feature = "ollama")]
            "ollama" => self.ollama_host.is_some(),

            _ => false,
        }
    }

    /// Check if a provider prefix is recognized (feature-flag compiled in),
    /// regardless of whether credentials are present.
    fn is_provider_recognized(provider: &str) -> bool {
        match provider {
            #[cfg(feature = "openai")]
            "openai" => true,

            #[cfg(feature = "anthropic")]
            "anthropic" => true,

            #[cfg(feature = "ollama")]
            "ollama" => true,

            _ => false,
        }
    }
}

#[async_trait]
impl ModelProvider for UnifiedProvider {
    /// Resolve a model name to a concrete Model implementation.
    ///
    /// Routing logic:
    /// 1. If model_name has a recognized prefix (e.g., "anthropic:..."), route to that provider
    /// 2. If model_name has no prefix, route to the configured default_provider
    /// 3. Return errors for unavailable providers or missing defaults
    async fn resolve(&self, model_name: &str) -> Result<Arc<dyn Model>, ModelError> {
        let (provider, bare_name) = if let Some((prefix, name)) = Self::parse_model_name(model_name)
        {
            // Check if the provider feature is compiled in
            if !Self::is_provider_recognized(prefix) {
                return Err(ModelError::Connection(format!(
                    "Provider '{}' is not available (feature not enabled)",
                    prefix
                )));
            }
            // Check if credentials are configured
            if !self.is_provider_available(prefix) {
                return Err(ModelError::Connection(format!(
                    "Provider '{}' is not configured (missing credentials)",
                    prefix
                )));
            }
            (prefix.to_string(), name.to_string())
        } else {
            // No prefix — use default provider
            let default = self.default_provider.as_ref().ok_or_else(|| {
                ModelError::Connection(
                    "No default provider configured and model name has no provider prefix"
                        .to_string(),
                )
            })?;
            (default.clone(), model_name.to_string())
        };

        // Route to real HTTP implementations based on provider
        match provider.as_str() {
            #[cfg(feature = "openai")]
            "openai" => {
                let api_key = self.openai_key.as_ref().ok_or_else(|| {
                    ModelError::Connection("OpenAI API key not configured".to_string())
                })?;
                let base_url = self
                    .openai_base_url
                    .as_deref()
                    .unwrap_or("https://api.openai.com/v1")
                    .to_string();
                Ok(Arc::new(OpenAIHttpModel::new(
                    bare_name,
                    api_key.clone(),
                    base_url,
                )))
            }
            #[cfg(feature = "anthropic")]
            "anthropic" => {
                let api_key = self.anthropic_key.as_ref().ok_or_else(|| {
                    ModelError::Connection("Anthropic API key not configured".to_string())
                })?;
                let base_url = self
                    .anthropic_base_url
                    .as_deref()
                    .unwrap_or("https://api.anthropic.com/v1")
                    .to_string();
                Ok(Arc::new(AnthropicHttpModel::new(
                    bare_name,
                    api_key.clone(),
                    base_url,
                )))
            }
            #[cfg(feature = "ollama")]
            "ollama" => {
                // Ollama uses OpenAI-compatible API format
                let host = self.ollama_host.as_deref().unwrap_or("http://localhost:11434");
                let base_url = format!("{}/v1", host.trim_end_matches('/'));
                Ok(Arc::new(OpenAIHttpModel::new(
                    bare_name,
                    String::new(), // Ollama typically doesn't need an API key
                    base_url,
                )))
            }
            _ => Err(ModelError::Connection(format!(
                "Provider '{}' has no HTTP implementation",
                provider
            ))),
        }
    }

    /// List all available models (currently returns empty; real implementations in 14.2).
    fn available_models(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A stub Model implementation for providers that don't have HTTP implementations yet.
/// Currently unused — kept for potential future use with new provider backends.
#[derive(Debug)]
#[allow(dead_code)]
struct StubModel {
    model_name: String,
    provider_name: String,
}

#[async_trait]
impl Model for StubModel {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
        Err(ModelError::Connection(format!(
            "Provider '{}' model '{}' is a stub — real implementation pending",
            self.provider_name, self.model_name
        )))
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        Err(ModelError::Connection(format!(
            "Provider '{}' model '{}' is a stub — real implementation pending",
            self.provider_name, self.model_name
        )))
    }

    fn name(&self) -> &str {
        &self.model_name
    }

    fn provider(&self) -> &str {
        &self.provider_name
    }

    fn context_window(&self) -> usize {
        // Default context window sizes by provider
        match self.provider_name.as_str() {
            "anthropic" => 200_000,
            "openai" => 128_000,
            "ollama" => 8_192,
            _ => 4_096,
        }
    }

    fn max_output_tokens(&self) -> usize {
        match self.provider_name.as_str() {
            "anthropic" => 8_192,
            "openai" => 16_384,
            "ollama" => 4_096,
            _ => 4_096,
        }
    }

    fn supports_tools(&self) -> bool {
        matches!(self.provider_name.as_str(), "anthropic" | "openai")
    }

    fn input_cost_per_million(&self) -> f64 {
        match self.provider_name.as_str() {
            "anthropic" => 3.0,
            "openai" => 5.0,
            "ollama" => 0.0,
            _ => 0.0,
        }
    }

    fn output_cost_per_million(&self) -> f64 {
        match self.provider_name.as_str() {
            "anthropic" => 15.0,
            "openai" => 15.0,
            "ollama" => 0.0,
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a provider with explicit config for testing.
    // We avoid from_env() in tests to not depend on real env vars.
    #[allow(dead_code)]
    fn make_provider(
        default: Option<&str>,
        #[cfg(feature = "openai")] openai: Option<&str>,
        #[cfg(feature = "anthropic")] anthropic: Option<&str>,
        #[cfg(feature = "ollama")] ollama: Option<&str>,
    ) -> UnifiedProvider {
        UnifiedProvider {
            default_provider: default.map(|s| s.to_string()),
            #[cfg(feature = "openai")]
            openai_key: openai.map(|s| s.to_string()),
            #[cfg(feature = "openai")]
            openai_base_url: None,
            #[cfg(feature = "anthropic")]
            anthropic_key: anthropic.map(|s| s.to_string()),
            #[cfg(feature = "anthropic")]
            anthropic_base_url: None,
            #[cfg(feature = "ollama")]
            ollama_host: ollama.map(|s| s.to_string()),
        }
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    fn test_provider() -> UnifiedProvider {
        make_provider(
            Some("openai"),
            #[cfg(feature = "openai")]
            Some("sk-test-key"),
            #[cfg(feature = "anthropic")]
            Some("sk-ant-test-key"),
            #[cfg(feature = "ollama")]
            None,
        )
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn resolve_prefixed_anthropic_model() {
        let provider = test_provider();
        let model = provider.resolve("anthropic:claude-sonnet-4-20250514").await.unwrap();
        assert_eq!(model.name(), "claude-sonnet-4-20250514");
        assert_eq!(model.provider(), "anthropic");
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn resolve_prefixed_openai_model() {
        let provider = test_provider();
        let model = provider.resolve("openai:gpt-4").await.unwrap();
        assert_eq!(model.name(), "gpt-4");
        assert_eq!(model.provider(), "openai");
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn resolve_unprefixed_model_uses_default() {
        let provider = test_provider();
        let model = provider.resolve("gpt-4").await.unwrap();
        assert_eq!(model.name(), "gpt-4");
        assert_eq!(model.provider(), "openai");
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn resolve_no_default_returns_error() {
        let provider = make_provider(
            None, // no default
            #[cfg(feature = "openai")]
            Some("sk-test"),
            #[cfg(feature = "anthropic")]
            Some("sk-ant-test"),
            #[cfg(feature = "ollama")]
            None,
        );
        let result = provider.resolve("gpt-4").await;
        assert!(result.is_err());
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        let display = format!("{}", err);
        assert!(display.contains("No default provider"));
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn resolve_unavailable_prefix_returns_error() {
        let provider = make_provider(
            Some("openai"),
            #[cfg(feature = "openai")]
            Some("sk-test"),
            #[cfg(feature = "anthropic")]
            None, // anthropic not configured
            #[cfg(feature = "ollama")]
            None,
        );
        let result = provider.resolve("anthropic:claude-sonnet-4-20250514").await;
        assert!(result.is_err());
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        let display = format!("{}", err);
        assert!(display.contains("anthropic"));
        assert!(display.contains("not configured"));
    }

    #[test]
    fn has_any_provider_with_no_keys_returns_false() {
        let provider = UnifiedProvider {
            default_provider: None,
            #[cfg(feature = "openai")]
            openai_key: None,
            #[cfg(feature = "openai")]
            openai_base_url: None,
            #[cfg(feature = "anthropic")]
            anthropic_key: None,
            #[cfg(feature = "anthropic")]
            anthropic_base_url: None,
            #[cfg(feature = "ollama")]
            ollama_host: None,
        };
        assert!(!provider.has_any_provider());
    }

    #[cfg(feature = "openai")]
    #[test]
    fn has_any_provider_with_openai_key_returns_true() {
        let provider = UnifiedProvider {
            default_provider: Some("openai".to_string()),
            openai_key: Some("sk-test".to_string()),
            openai_base_url: None,
            #[cfg(feature = "anthropic")]
            anthropic_key: None,
            #[cfg(feature = "anthropic")]
            anthropic_base_url: None,
            #[cfg(feature = "ollama")]
            ollama_host: None,
        };
        assert!(provider.has_any_provider());
    }

    #[test]
    fn parse_model_name_with_prefix() {
        let result = UnifiedProvider::parse_model_name("anthropic:claude-sonnet-4-20250514");
        assert_eq!(result, Some(("anthropic", "claude-sonnet-4-20250514")));
    }

    #[test]
    fn parse_model_name_with_openai_prefix() {
        let result = UnifiedProvider::parse_model_name("openai:gpt-4o");
        assert_eq!(result, Some(("openai", "gpt-4o")));
    }

    #[test]
    fn parse_model_name_with_ollama_prefix() {
        let result = UnifiedProvider::parse_model_name("ollama:llama2");
        assert_eq!(result, Some(("ollama", "llama2")));
    }

    #[test]
    fn parse_model_name_no_prefix() {
        let result = UnifiedProvider::parse_model_name("gpt-4");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_model_name_unknown_prefix_treated_as_no_prefix() {
        let result = UnifiedProvider::parse_model_name("unknown:some-model");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_model_name_empty_string() {
        let result = UnifiedProvider::parse_model_name("");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_model_name_colon_only() {
        let result = UnifiedProvider::parse_model_name(":");
        assert_eq!(result, None);
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[test]
    fn new_with_no_credentials_returns_error() {
        let result = UnifiedProvider::new(
            None,
            #[cfg(feature = "openai")]
            None,
            #[cfg(feature = "openai")]
            None,
            #[cfg(feature = "anthropic")]
            None,
            #[cfg(feature = "anthropic")]
            None,
            #[cfg(feature = "ollama")]
            None,
        );
        assert!(result.is_err());
    }

    #[cfg(feature = "openai")]
    #[test]
    fn new_with_openai_key_succeeds() {
        let result = UnifiedProvider::new(
            Some("openai".to_string()),
            #[cfg(feature = "openai")]
            Some("sk-test".to_string()),
            #[cfg(feature = "openai")]
            None,
            #[cfg(feature = "anthropic")]
            None,
            #[cfg(feature = "anthropic")]
            None,
            #[cfg(feature = "ollama")]
            None,
        );
        assert!(result.is_ok());
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn openai_model_stream_returns_connection_error_with_bad_endpoint() {
        let provider = make_provider(
            Some("openai"),
            #[cfg(feature = "openai")]
            Some("sk-test-key"),
            #[cfg(feature = "anthropic")]
            Some("sk-ant-test-key"),
            #[cfg(feature = "ollama")]
            None,
        );
        let model = provider.resolve("openai:gpt-4").await.unwrap();
        let request = ModelRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            output_schema: None,
        };
        // With a fake API key pointing at the real OpenAI endpoint, we'll get
        // either a Connection error (DNS/timeout) or an Api error (401).
        let result = model.stream(request).await;
        assert!(result.is_err());
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn available_models_returns_empty() {
        let provider = test_provider();
        assert!(provider.available_models().is_empty());
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn stub_model_metadata() {
        let provider = test_provider();
        let model = provider.resolve("anthropic:claude-sonnet-4-20250514").await.unwrap();
        assert_eq!(model.context_window(), 200_000);
        assert_eq!(model.max_output_tokens(), 8_192);
        assert!(model.supports_tools());
        assert_eq!(model.input_cost_per_million(), 3.0);
        assert_eq!(model.output_cost_per_million(), 15.0);
    }

    #[cfg(all(feature = "openai", feature = "anthropic"))]
    #[tokio::test]
    async fn resolve_model_with_colons_in_name() {
        // Model names like "openai:gpt-4:2024-01-01" should work
        // The first colon separates provider from name
        let provider = test_provider();
        let model = provider.resolve("openai:gpt-4:2024-01-01").await.unwrap();
        assert_eq!(model.name(), "gpt-4:2024-01-01");
        assert_eq!(model.provider(), "openai");
    }

    // --- from_profile() unit tests ---

    /// Helper to create a minimal ResolvedProfile for testing.
    fn make_resolved_profile(
        provider: &str,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> ResolvedProfile {
        ResolvedProfile {
            provider: provider.to_string(),
            api_key: api_key.map(|s| s.to_string()),
            base_url: base_url.map(|s| s.to_string()),
            model: "test-model".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: std::collections::HashMap::new(),
        }
    }

    #[cfg(feature = "openai")]
    #[test]
    fn from_profile_openai_succeeds() {
        let profile = make_resolved_profile("openai", Some("sk-test-key"), None);
        let result = UnifiedProvider::from_profile(&profile);
        assert!(result.is_ok());
        let provider = result.unwrap();
        assert_eq!(provider.default_provider.as_deref(), Some("openai"));
        assert_eq!(provider.openai_key.as_deref(), Some("sk-test-key"));
    }

    #[cfg(feature = "anthropic")]
    #[test]
    fn from_profile_anthropic_succeeds() {
        let profile = make_resolved_profile("anthropic", Some("sk-ant-key"), None);
        let result = UnifiedProvider::from_profile(&profile);
        assert!(result.is_ok());
        let provider = result.unwrap();
        assert_eq!(provider.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(provider.anthropic_key.as_deref(), Some("sk-ant-key"));
    }

    #[cfg(feature = "ollama")]
    #[test]
    fn from_profile_ollama_succeeds_with_default_base_url() {
        let profile = make_resolved_profile("ollama", None, None);
        let result = UnifiedProvider::from_profile(&profile);
        assert!(result.is_ok());
        let provider = result.unwrap();
        assert_eq!(provider.default_provider.as_deref(), Some("ollama"));
        // Should default to localhost if no base_url is given
        assert_eq!(
            provider.ollama_host.as_deref(),
            Some("http://localhost:11434")
        );
    }

    #[cfg(feature = "ollama")]
    #[test]
    fn from_profile_ollama_uses_provided_base_url() {
        let profile =
            make_resolved_profile("ollama", None, Some("http://custom-host:11434"));
        let result = UnifiedProvider::from_profile(&profile);
        assert!(result.is_ok());
        let provider = result.unwrap();
        assert_eq!(
            provider.ollama_host.as_deref(),
            Some("http://custom-host:11434")
        );
    }

    #[test]
    fn from_profile_unknown_provider_returns_error() {
        let profile = make_resolved_profile("unknown", Some("some-key"), None);
        let result = UnifiedProvider::from_profile(&profile);
        assert!(result.is_err());
    }

    #[test]
    fn from_profile_unknown_provider_error_contains_name() {
        let profile = make_resolved_profile("not-a-real-provider", Some("key"), None);
        let result = UnifiedProvider::from_profile(&profile);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("not-a-real-provider"),
            "Error message should contain the unknown provider name, got: {}",
            msg
        );
    }
}

/// Property-based tests for model name routing.
///
/// **Validates: Requirements 16.4, 16.5**
#[cfg(test)]
#[cfg(all(feature = "openai", feature = "anthropic"))]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy to generate valid model name segments (alphanumeric with hyphens).
    fn model_name_segment() -> impl Strategy<Value = String> {
        // Generate alphanumeric strings with hyphens, length 1-30
        "[a-z0-9][a-z0-9\\-]{0,29}".prop_filter("must not be empty", |s| !s.is_empty())
    }

    /// Strategy for known provider prefixes.
    fn known_prefix() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("openai"), Just("anthropic"), Just("ollama"),]
    }

    /// Strategy for unknown prefixes (not openai/anthropic/ollama).
    fn unknown_prefix() -> impl Strategy<Value = String> {
        "[a-z]{2,10}".prop_filter("must not be a known prefix", |s| {
            s != "openai" && s != "anthropic" && s != "ollama"
        })
    }

    /// Helper: create a test provider with openai and anthropic configured.
    fn prop_test_provider() -> UnifiedProvider {
        UnifiedProvider {
            default_provider: Some("openai".to_string()),
            #[cfg(feature = "openai")]
            openai_key: Some("sk-test-key".to_string()),
            #[cfg(feature = "openai")]
            openai_base_url: None,
            #[cfg(feature = "anthropic")]
            anthropic_key: Some("sk-ant-test-key".to_string()),
            #[cfg(feature = "anthropic")]
            anthropic_base_url: None,
            #[cfg(feature = "ollama")]
            ollama_host: None,
        }
    }

    proptest! {
        /// Property 14: Prefixed model names route to the specified provider.
        ///
        /// For any model name with a recognized "openai:" or "anthropic:" prefix,
        /// resolve() returns a model whose provider() matches the prefix and whose
        /// name() equals the portion after the colon.
        #[test]
        fn prefixed_model_routes_to_correct_provider(
            prefix in prop_oneof![Just("openai"), Just("anthropic")],
            name in model_name_segment(),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let provider = prop_test_provider();
            let full_name = format!("{}:{}", prefix, name);

            let model = rt.block_on(async {
                provider.resolve(&full_name).await.unwrap()
            });

            prop_assert_eq!(model.provider(), prefix);
            prop_assert_eq!(model.name(), name.as_str());
        }

        /// Property 14: Unprefixed model names route to default provider.
        ///
        /// For any model name without a recognized prefix, resolve() returns
        /// a model whose provider() equals the configured default_provider and
        /// whose name() equals the full input string.
        #[test]
        fn unprefixed_model_routes_to_default_provider(
            name in model_name_segment(),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let provider = prop_test_provider();

            let model = rt.block_on(async {
                provider.resolve(&name).await.unwrap()
            });

            prop_assert_eq!(model.provider(), "openai"); // default provider
            prop_assert_eq!(model.name(), name.as_str());
        }

        /// Property 14: Unknown prefix is treated as part of model name.
        ///
        /// For any model name with an unrecognized prefix (not openai/anthropic/ollama),
        /// the entire string (including the colon) is treated as the model name and
        /// routed to the default provider.
        #[test]
        fn unknown_prefix_treated_as_model_name(
            prefix in unknown_prefix(),
            suffix in model_name_segment(),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let provider = prop_test_provider();
            let full_name = format!("{}:{}", prefix, suffix);

            let model = rt.block_on(async {
                provider.resolve(&full_name).await.unwrap()
            });

            // Unknown prefix means entire string is the model name, routed to default
            prop_assert_eq!(model.provider(), "openai"); // default provider
            prop_assert_eq!(model.name(), full_name.as_str());
        }

        /// Property 14: parse_model_name correctly identifies known prefixes.
        ///
        /// For any known prefix combined with any model name segment,
        /// parse_model_name returns Some((prefix, name)).
        #[test]
        fn parse_identifies_known_prefixes(
            prefix in known_prefix(),
            name in model_name_segment(),
        ) {
            let full_name = format!("{}:{}", prefix, name);
            let result = UnifiedProvider::parse_model_name(&full_name);
            prop_assert_eq!(result, Some((prefix, name.as_str())));
        }

        /// Property 14: parse_model_name rejects unknown prefixes.
        ///
        /// For any string with an unrecognized prefix before the colon,
        /// parse_model_name returns None (treating it as unprefixed).
        #[test]
        fn parse_rejects_unknown_prefixes(
            prefix in unknown_prefix(),
            name in model_name_segment(),
        ) {
            let full_name = format!("{}:{}", prefix, name);
            let result = UnifiedProvider::parse_model_name(&full_name);
            prop_assert_eq!(result, None);
        }
    }
}
