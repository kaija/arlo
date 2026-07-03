//! Retry and fallback logic for LLM provider calls.
//!
//! Implements exponential backoff with jitter, retryable error classification,
//! and a fallback chain that attempts alternative models when the primary is exhausted.
//!
//! # Backoff Formula
//!
//! ```text
//! delay = min(initial_backoff_ms × backoff_multiplier^(attempt-1), max_backoff_ms)
//! jitter = random(0.0..0.25) × delay
//! total_delay = delay + jitter
//! ```
//!
//! # Retry-After Header
//!
//! When a `RateLimited` error includes a `retry_after_ms` value, that value
//! is used instead of the computed backoff, but capped at `max_backoff_ms`.

use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use tracing::{debug, warn};

use agent_core::error::ModelError;
use agent_core::model::{Model, ModelRequest, ModelStream};

/// Configuration for retry behavior and fallback chain.
///
/// Controls how many times a failed request is retried, backoff timing,
/// and which HTTP status codes are considered retryable.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts per model (not counting the initial attempt).
    pub max_retries: u32,

    /// Initial backoff delay in milliseconds for the first retry.
    pub initial_backoff_ms: u64,

    /// Maximum backoff delay in milliseconds (cap for exponential growth).
    pub max_backoff_ms: u64,

    /// Multiplier applied to the backoff on each successive attempt.
    pub backoff_multiplier: f64,

    /// HTTP status codes that are considered retryable.
    pub retryable_statuses: Vec<u16>,

    /// Ordered list of fallback model names to try when the primary is exhausted.
    pub fallback_models: Vec<String>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 1000,
            max_backoff_ms: 30000,
            backoff_multiplier: 2.0,
            retryable_statuses: vec![429, 500, 502, 503, 529],
            fallback_models: Vec::new(),
        }
    }
}

/// Compute the backoff duration for a given attempt number.
///
/// The attempt number is 1-indexed (first retry = attempt 1).
///
/// Formula: `min(initial_backoff_ms × backoff_multiplier^(attempt-1), max_backoff_ms)`
/// with random jitter of 0–25% added on top.
pub fn compute_backoff(attempt: u32, config: &RetryConfig) -> Duration {
    let base = config.initial_backoff_ms as f64
        * config.backoff_multiplier.powi(attempt.saturating_sub(1) as i32);
    let capped = base.min(config.max_backoff_ms as f64);

    let mut rng = rand::thread_rng();
    let jitter_factor: f64 = rng.gen_range(0.0..0.25);
    let jitter = jitter_factor * capped;

    Duration::from_millis((capped + jitter) as u64)
}

/// Compute the backoff duration for a given attempt, using an explicit jitter factor.
///
/// This is useful for deterministic testing where you want to control the jitter.
/// `jitter_factor` should be in range [0.0, 0.25].
pub fn compute_backoff_with_jitter(attempt: u32, config: &RetryConfig, jitter_factor: f64) -> Duration {
    let base = config.initial_backoff_ms as f64
        * config.backoff_multiplier.powi(attempt.saturating_sub(1) as i32);
    let capped = base.min(config.max_backoff_ms as f64);
    let jitter = jitter_factor.clamp(0.0, 0.25) * capped;

    Duration::from_millis((capped + jitter) as u64)
}

/// Determine whether a `ModelError` is retryable under the given configuration.
///
/// Always retryable:
/// - `RateLimited` (regardless of status code list)
/// - `Connection` (transient network issues)
/// - `StreamInterrupted` (transient stream failures)
///
/// Conditionally retryable:
/// - `Api { status, .. }` — retryable if `status` is in `config.retryable_statuses`
///
/// Never retryable:
/// - `PromptTooLong` (requires prompt modification, not retry)
/// - `MaxOutputTokens` (requires config change, not retry)
pub fn is_retryable(error: &ModelError, config: &RetryConfig) -> bool {
    match error {
        ModelError::RateLimited { .. } => true,
        ModelError::Connection(_) => true,
        ModelError::StreamInterrupted(_) => true,
        ModelError::Api { status, .. } => config.retryable_statuses.contains(status),
        ModelError::PromptTooLong { .. } => false,
        ModelError::MaxOutputTokens => false,
    }
}

/// Get the effective backoff duration, respecting the Retry-After header.
///
/// If the error is `RateLimited` with a non-zero `retry_after_ms`, that value
/// is used (capped at `max_backoff_ms`). Otherwise, the standard exponential
/// backoff is computed.
pub fn effective_backoff(attempt: u32, error: &ModelError, config: &RetryConfig) -> Duration {
    if let ModelError::RateLimited { retry_after_ms } = error {
        if *retry_after_ms > 0 {
            let capped = (*retry_after_ms).min(config.max_backoff_ms);
            return Duration::from_millis(capped);
        }
    }
    compute_backoff(attempt, config)
}

/// Attempt a model stream request with retries and fallback chain.
///
/// Tries the primary model first with up to `config.max_retries` retries.
/// If all retries are exhausted and fallback models are provided, each
/// fallback is attempted with its own fresh set of retries.
///
/// # Arguments
///
/// * `config` - Retry and fallback configuration
/// * `models` - Ordered list of models: primary at index 0, fallbacks at 1..N
/// * `request` - The model request to execute
///
/// # Returns
///
/// The `ModelStream` from the first successful attempt, or a comprehensive
/// `ModelError` describing all failures when everything is exhausted.
pub async fn retry_with_fallback(
    config: &RetryConfig,
    models: &[Arc<dyn Model>],
    request: &ModelRequest,
) -> Result<ModelStream, ModelError> {
    if models.is_empty() {
        return Err(ModelError::Connection(
            "No models available for retry/fallback".to_string(),
        ));
    }

    let mut all_errors: Vec<(String, ModelError)> = Vec::new();

    for (model_idx, model) in models.iter().enumerate() {
        let model_name = model.name().to_string();
        let is_primary = model_idx == 0;
        let label = if is_primary {
            "primary".to_string()
        } else {
            format!("fallback[{}]", model_idx - 1)
        };

        // Attempt initial request + retries for this model
        for attempt in 0..=config.max_retries {
            if attempt > 0 {
                debug!(
                    model = %model_name,
                    attempt = attempt,
                    max_retries = config.max_retries,
                    role = %label,
                    "Retrying model request"
                );
            }

            match model.stream(request.clone()).await {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    if attempt == config.max_retries || !is_retryable(&err, config) {
                        // Exhausted retries or non-retryable error
                        warn!(
                            model = %model_name,
                            attempt = attempt,
                            error = %err,
                            retryable = is_retryable(&err, config),
                            role = %label,
                            "Model request failed, moving on"
                        );
                        all_errors.push((model_name.clone(), err));
                        break;
                    }

                    // Compute backoff and sleep
                    let backoff = effective_backoff(attempt + 1, &err, config);
                    debug!(
                        model = %model_name,
                        attempt = attempt,
                        backoff_ms = backoff.as_millis() as u64,
                        error = %err,
                        role = %label,
                        "Retryable error, backing off"
                    );
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    // All models and retries exhausted — build comprehensive error
    let error_details: Vec<String> = all_errors
        .iter()
        .map(|(model, err)| format!("{}: {}", model, err))
        .collect();

    Err(ModelError::Connection(format!(
        "All models and retries exhausted. Errors: [{}]",
        error_details.join("; ")
    )))
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use std::time::Duration;

    /// **Validates: Requirements 25.2**
    ///
    /// Property 21: Exponential backoff formula
    /// Generate attempt counts and retry configs, assert computed delay matches
    /// formula within jitter bounds.
    fn arb_retry_config() -> impl Strategy<Value = RetryConfig> {
        (
            100u64..=10000u64,    // initial_backoff_ms
            5000u64..=60000u64,   // max_backoff_ms
            150u32..=300u32,      // backoff_multiplier * 100 (1.5-3.0)
        )
            .prop_map(|(initial, max_raw, mult_100)| {
                // Ensure max_backoff >= initial_backoff to avoid degenerate configs
                let max_backoff = max_raw.max(initial);
                let multiplier = mult_100 as f64 / 100.0;
                RetryConfig {
                    initial_backoff_ms: initial,
                    max_backoff_ms: max_backoff,
                    backoff_multiplier: multiplier,
                    ..RetryConfig::default()
                }
            })
    }

    proptest! {
        #[test]
        fn backoff_within_jitter_bounds(
            config in arb_retry_config(),
            attempt in 1u32..=10u32,
        ) {
            // Compute expected base delay (without jitter)
            let base = config.initial_backoff_ms as f64
                * config.backoff_multiplier.powi(attempt.saturating_sub(1) as i32);
            let base_delay = base.min(config.max_backoff_ms as f64);

            // Call compute_backoff multiple times to exercise jitter randomness
            for _ in 0..5 {
                let result = compute_backoff(attempt, &config);
                let result_ms = result.as_millis() as f64;

                // Assert: result >= base_delay (no negative jitter)
                prop_assert!(
                    result_ms >= base_delay.floor(),
                    "result {}ms < base_delay {}ms (attempt={}, initial={}, max={}, mult={})",
                    result_ms, base_delay, attempt,
                    config.initial_backoff_ms, config.max_backoff_ms, config.backoff_multiplier
                );

                // Assert: result <= base_delay * 1.25 (jitter adds at most 25%)
                let max_with_jitter = base_delay * 1.25;
                prop_assert!(
                    result_ms <= max_with_jitter.ceil() + 1.0, // +1 for float→u64 rounding
                    "result {}ms > base_delay*1.25 = {}ms (attempt={}, initial={}, max={}, mult={})",
                    result_ms, max_with_jitter, attempt,
                    config.initial_backoff_ms, config.max_backoff_ms, config.backoff_multiplier
                );

                // Assert: result <= max_backoff_ms * 1.25 (cap is respected, plus max jitter)
                let max_cap_with_jitter = config.max_backoff_ms as f64 * 1.25;
                prop_assert!(
                    result_ms <= max_cap_with_jitter.ceil() + 1.0,
                    "result {}ms > max_backoff*1.25 = {}ms (attempt={}, initial={}, max={}, mult={})",
                    result_ms, max_cap_with_jitter, attempt,
                    config.initial_backoff_ms, config.max_backoff_ms, config.backoff_multiplier
                );
            }
        }

        #[test]
        fn zero_jitter_is_deterministic(
            config in arb_retry_config(),
            attempt in 1u32..=10u32,
        ) {
            // compute_backoff_with_jitter(attempt, config, 0.0) should equal base_delay exactly
            let base = config.initial_backoff_ms as f64
                * config.backoff_multiplier.powi(attempt.saturating_sub(1) as i32);
            let expected_ms = base.min(config.max_backoff_ms as f64) as u64;

            let result = compute_backoff_with_jitter(attempt, &config, 0.0);
            prop_assert_eq!(
                result,
                Duration::from_millis(expected_ms),
                "zero jitter should produce deterministic base delay"
            );

            // Multiple calls should give the same result (deterministic)
            let result2 = compute_backoff_with_jitter(attempt, &config, 0.0);
            prop_assert_eq!(result, result2, "zero jitter should be repeatable");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn default_retry_config() {
        let config = RetryConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.initial_backoff_ms, 1000);
        assert_eq!(config.max_backoff_ms, 30000);
        assert_eq!(config.backoff_multiplier, 2.0);
        assert_eq!(config.retryable_statuses, vec![429, 500, 502, 503, 529]);
        assert!(config.fallback_models.is_empty());
    }

    #[test]
    fn compute_backoff_first_attempt() {
        let config = RetryConfig::default();
        // attempt 1: 1000 * 2.0^0 = 1000ms base, plus 0-25% jitter
        let backoff = compute_backoff(1, &config);
        assert!(backoff >= Duration::from_millis(1000));
        assert!(backoff <= Duration::from_millis(1250));
    }

    #[test]
    fn compute_backoff_second_attempt() {
        let config = RetryConfig::default();
        // attempt 2: 1000 * 2.0^1 = 2000ms base, plus 0-25% jitter
        let backoff = compute_backoff(2, &config);
        assert!(backoff >= Duration::from_millis(2000));
        assert!(backoff <= Duration::from_millis(2500));
    }

    #[test]
    fn compute_backoff_third_attempt() {
        let config = RetryConfig::default();
        // attempt 3: 1000 * 2.0^2 = 4000ms base, plus 0-25% jitter
        let backoff = compute_backoff(3, &config);
        assert!(backoff >= Duration::from_millis(4000));
        assert!(backoff <= Duration::from_millis(5000));
    }

    #[test]
    fn compute_backoff_capped_at_max() {
        let config = RetryConfig {
            max_backoff_ms: 5000,
            ..RetryConfig::default()
        };
        // attempt 10: 1000 * 2.0^9 = 512000ms, capped to 5000ms
        let backoff = compute_backoff(10, &config);
        assert!(backoff >= Duration::from_millis(5000));
        assert!(backoff <= Duration::from_millis(6250)); // 5000 + 25%
    }

    #[test]
    fn compute_backoff_with_jitter_deterministic() {
        let config = RetryConfig::default();

        // Zero jitter
        let backoff = compute_backoff_with_jitter(1, &config, 0.0);
        assert_eq!(backoff, Duration::from_millis(1000));

        // Max jitter (25%)
        let backoff = compute_backoff_with_jitter(1, &config, 0.25);
        assert_eq!(backoff, Duration::from_millis(1250));

        // 10% jitter
        let backoff = compute_backoff_with_jitter(2, &config, 0.1);
        // base = 2000, jitter = 200
        assert_eq!(backoff, Duration::from_millis(2200));
    }

    #[test]
    fn compute_backoff_with_jitter_clamped() {
        let config = RetryConfig::default();
        // Jitter factor > 0.25 should be clamped
        let backoff = compute_backoff_with_jitter(1, &config, 0.5);
        // Clamped to 0.25, so 1000 + 250 = 1250
        assert_eq!(backoff, Duration::from_millis(1250));

        // Negative jitter should be clamped to 0
        let backoff = compute_backoff_with_jitter(1, &config, -0.1);
        assert_eq!(backoff, Duration::from_millis(1000));
    }

    #[test]
    fn is_retryable_rate_limited() {
        let config = RetryConfig::default();
        let err = ModelError::RateLimited { retry_after_ms: 5000 };
        assert!(is_retryable(&err, &config));
    }

    #[test]
    fn is_retryable_connection() {
        let config = RetryConfig::default();
        let err = ModelError::Connection("timeout".to_string());
        assert!(is_retryable(&err, &config));
    }

    #[test]
    fn is_retryable_stream_interrupted() {
        let config = RetryConfig::default();
        let err = ModelError::StreamInterrupted("connection reset".to_string());
        assert!(is_retryable(&err, &config));
    }

    #[test]
    fn is_retryable_api_with_retryable_status() {
        let config = RetryConfig::default();
        for status in &[429u16, 500, 502, 503, 529] {
            let err = ModelError::Api {
                status: *status,
                body: "error".to_string(),
            };
            assert!(is_retryable(&err, &config), "status {} should be retryable", status);
        }
    }

    #[test]
    fn is_retryable_api_with_non_retryable_status() {
        let config = RetryConfig::default();
        for status in &[400u16, 401, 403, 404, 422] {
            let err = ModelError::Api {
                status: *status,
                body: "error".to_string(),
            };
            assert!(!is_retryable(&err, &config), "status {} should NOT be retryable", status);
        }
    }

    #[test]
    fn is_retryable_prompt_too_long() {
        let config = RetryConfig::default();
        let err = ModelError::PromptTooLong { tokens: 200000 };
        assert!(!is_retryable(&err, &config));
    }

    #[test]
    fn is_retryable_max_output_tokens() {
        let config = RetryConfig::default();
        let err = ModelError::MaxOutputTokens;
        assert!(!is_retryable(&err, &config));
    }

    #[test]
    fn is_retryable_custom_status_list() {
        let config = RetryConfig {
            retryable_statuses: vec![418, 503],
            ..RetryConfig::default()
        };
        let err_418 = ModelError::Api { status: 418, body: "teapot".to_string() };
        assert!(is_retryable(&err_418, &config));

        let err_500 = ModelError::Api { status: 500, body: "error".to_string() };
        assert!(!is_retryable(&err_500, &config));
    }

    #[test]
    fn effective_backoff_respects_retry_after() {
        let config = RetryConfig::default();
        let err = ModelError::RateLimited { retry_after_ms: 10000 };
        let backoff = effective_backoff(1, &err, &config);
        assert_eq!(backoff, Duration::from_millis(10000));
    }

    #[test]
    fn effective_backoff_caps_retry_after() {
        let config = RetryConfig {
            max_backoff_ms: 5000,
            ..RetryConfig::default()
        };
        let err = ModelError::RateLimited { retry_after_ms: 60000 };
        let backoff = effective_backoff(1, &err, &config);
        assert_eq!(backoff, Duration::from_millis(5000));
    }

    #[test]
    fn effective_backoff_zero_retry_after_uses_exponential() {
        let config = RetryConfig::default();
        let err = ModelError::RateLimited { retry_after_ms: 0 };
        let backoff = effective_backoff(1, &err, &config);
        // Should use compute_backoff instead
        assert!(backoff >= Duration::from_millis(1000));
        assert!(backoff <= Duration::from_millis(1250));
    }

    #[test]
    fn effective_backoff_non_rate_limited_uses_exponential() {
        let config = RetryConfig::default();
        let err = ModelError::Connection("timeout".to_string());
        let backoff = effective_backoff(2, &err, &config);
        // attempt 2: 2000ms base + jitter
        assert!(backoff >= Duration::from_millis(2000));
        assert!(backoff <= Duration::from_millis(2500));
    }

    #[tokio::test]
    async fn retry_with_fallback_no_models_returns_error() {
        let config = RetryConfig::default();
        let models: Vec<Arc<dyn Model>> = vec![];
        let request = ModelRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            output_schema: None,
        };

        let result = retry_with_fallback(&config, &models, &request).await;
        match result {
            Err(err) => assert!(format!("{}", err).contains("No models available")),
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn retry_with_fallback_first_attempt_succeeds() {
        let config = RetryConfig::default();
        let model: Arc<dyn Model> = Arc::new(SuccessModel { name: "test-model".to_string() });
        let models = vec![model];
        let request = ModelRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            output_schema: None,
        };

        let result = retry_with_fallback(&config, &models, &request).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn retry_with_fallback_non_retryable_error_no_retry() {
        let config = RetryConfig {
            max_retries: 3,
            ..RetryConfig::default()
        };
        let model: Arc<dyn Model> = Arc::new(FailModel {
            name: "fail-model".to_string(),
            error_fn: || ModelError::PromptTooLong { tokens: 200000 },
        });
        let models = vec![model];
        let request = ModelRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            output_schema: None,
        };

        let result = retry_with_fallback(&config, &models, &request).await;
        match result {
            Err(err) => {
                let err_msg = format!("{}", err);
                assert!(err_msg.contains("All models and retries exhausted"));
            }
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn retry_with_fallback_uses_fallback_on_exhaustion() {
        let config = RetryConfig {
            max_retries: 0, // no retries, go straight to fallback
            ..RetryConfig::default()
        };
        let primary: Arc<dyn Model> = Arc::new(FailModel {
            name: "primary".to_string(),
            error_fn: || ModelError::Api { status: 500, body: "down".to_string() },
        });
        let fallback: Arc<dyn Model> = Arc::new(SuccessModel { name: "fallback".to_string() });
        let models = vec![primary, fallback];
        let request = ModelRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            output_schema: None,
        };

        let result = retry_with_fallback(&config, &models, &request).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn retry_with_fallback_all_models_exhausted() {
        let config = RetryConfig {
            max_retries: 0,
            ..RetryConfig::default()
        };
        let model1: Arc<dyn Model> = Arc::new(FailModel {
            name: "model-1".to_string(),
            error_fn: || ModelError::Api { status: 500, body: "error1".to_string() },
        });
        let model2: Arc<dyn Model> = Arc::new(FailModel {
            name: "model-2".to_string(),
            error_fn: || ModelError::Connection("refused".to_string()),
        });
        let models = vec![model1, model2];
        let request = ModelRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            output_schema: None,
        };

        let result = retry_with_fallback(&config, &models, &request).await;
        match result {
            Err(err) => {
                let err_msg = format!("{}", err);
                assert!(err_msg.contains("model-1"));
                assert!(err_msg.contains("model-2"));
                assert!(err_msg.contains("All models and retries exhausted"));
            }
            Ok(_) => panic!("expected error"),
        }
    }

    // --- Test helpers ---

    /// A model that always succeeds with an empty stream.
    #[derive(Debug)]
    struct SuccessModel {
        name: String,
    }

    #[async_trait::async_trait]
    impl Model for SuccessModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            use futures::stream;
            use agent_core::stream::{StreamChunk, StopReason};
            use agent_core::message::Usage;
            let chunks = vec![Ok(StreamChunk::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            })];
            Ok(Box::pin(stream::iter(chunks)))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<agent_core::model::ModelResponse, ModelError> {
            Err(ModelError::Connection("not implemented".to_string()))
        }

        fn name(&self) -> &str { &self.name }
        fn provider(&self) -> &str { "test" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 0.0 }
        fn output_cost_per_million(&self) -> f64 { 0.0 }
    }

    /// A model that always fails with a configurable error.
    #[derive(Debug)]
    struct FailModel {
        name: String,
        error_fn: fn() -> ModelError,
    }

    #[async_trait::async_trait]
    impl Model for FailModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            Err((self.error_fn)())
        }

        async fn complete(&self, _request: ModelRequest) -> Result<agent_core::model::ModelResponse, ModelError> {
            Err((self.error_fn)())
        }

        fn name(&self) -> &str { &self.name }
        fn provider(&self) -> &str { "test" }
        fn context_window(&self) -> usize { 128000 }
        fn max_output_tokens(&self) -> usize { 4096 }
        fn supports_tools(&self) -> bool { true }
        fn input_cost_per_million(&self) -> f64 { 0.0 }
        fn output_cost_per_million(&self) -> f64 { 0.0 }
    }
}
