//! Model override wrapper for applying context_window and max_output_tokens overrides.
//!
//! Uses the decorator pattern to wrap an existing `Model` implementation,
//! delegating all methods to the inner model except `context_window()` and
//! `max_output_tokens()` when override values are present.

use std::sync::Arc;

use async_trait::async_trait;

use agent_core::error::ModelError;
use agent_core::model::{Model, ModelRequest, ModelResponse, ModelStream};

/// Wraps an existing `Model` implementation to override `context_window`
/// and/or `max_output_tokens` without modifying the concrete model type.
pub struct ModelOverrideWrapper {
    inner: Arc<dyn Model>,
    context_window_override: Option<usize>,
    max_output_tokens_override: Option<usize>,
}

impl ModelOverrideWrapper {
    /// Create a new override wrapper around the given model.
    pub fn new(
        inner: Arc<dyn Model>,
        context_window_override: Option<usize>,
        max_output_tokens_override: Option<usize>,
    ) -> Self {
        Self {
            inner,
            context_window_override,
            max_output_tokens_override,
        }
    }

    /// Only wraps if at least one override is present.
    /// Otherwise returns the inner model directly (zero-cost passthrough).
    pub fn wrap_if_needed(
        inner: Arc<dyn Model>,
        context_window: Option<usize>,
        max_output_tokens: Option<usize>,
    ) -> Arc<dyn Model> {
        if context_window.is_some() || max_output_tokens.is_some() {
            Arc::new(Self::new(inner, context_window, max_output_tokens))
        } else {
            inner
        }
    }
}

#[async_trait]
impl Model for ModelOverrideWrapper {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
        self.inner.stream(request).await
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.inner.complete(request).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn provider(&self) -> &str {
        self.inner.provider()
    }

    fn context_window(&self) -> usize {
        self.context_window_override
            .unwrap_or_else(|| self.inner.context_window())
    }

    fn max_output_tokens(&self) -> usize {
        self.max_output_tokens_override
            .unwrap_or_else(|| self.inner.max_output_tokens())
    }

    fn supports_tools(&self) -> bool {
        self.inner.supports_tools()
    }

    fn input_cost_per_million(&self) -> f64 {
        self.inner.input_cost_per_million()
    }

    fn output_cost_per_million(&self) -> f64 {
        self.inner.output_cost_per_million()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::message::Usage;
    use agent_core::model::ModelResponse;
    use agent_core::stream::StopReason;
    use proptest::prelude::*;

    /// A minimal mock model for unit testing the wrapper.
    #[derive(Debug)]
    struct MockModel {
        name: String,
        provider: String,
        context_window: usize,
        max_output_tokens: usize,
        supports_tools: bool,
        input_cost: f64,
        output_cost: f64,
    }

    impl MockModel {
        fn new() -> Self {
            Self {
                name: "mock-model".to_string(),
                provider: "mock-provider".to_string(),
                context_window: 100_000,
                max_output_tokens: 8_000,
                supports_tools: true,
                input_cost: 3.0,
                output_cost: 15.0,
            }
        }
    }

    #[async_trait]
    impl Model for MockModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            Err(ModelError::Connection("mock: stream not implemented".to_string()))
        }

        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                content: vec![],
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: None,
                },
                stop_reason: StopReason::EndTurn,
            })
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn provider(&self) -> &str {
            &self.provider
        }

        fn context_window(&self) -> usize {
            self.context_window
        }

        fn max_output_tokens(&self) -> usize {
            self.max_output_tokens
        }

        fn supports_tools(&self) -> bool {
            self.supports_tools
        }

        fn input_cost_per_million(&self) -> f64 {
            self.input_cost
        }

        fn output_cost_per_million(&self) -> f64 {
            self.output_cost
        }
    }

    #[test]
    fn wrap_if_needed_returns_inner_when_no_overrides() {
        let inner: Arc<dyn Model> = Arc::new(MockModel::new());
        let ptr_before = Arc::as_ptr(&inner);
        let result = ModelOverrideWrapper::wrap_if_needed(inner, None, None);
        // Should be the same Arc (not wrapped)
        let ptr_after = Arc::as_ptr(&result);
        assert_eq!(ptr_before, ptr_after);
    }

    #[test]
    fn wrap_if_needed_wraps_when_context_window_override_present() {
        let inner: Arc<dyn Model> = Arc::new(MockModel::new());
        let result = ModelOverrideWrapper::wrap_if_needed(inner, Some(200_000), None);
        assert_eq!(result.context_window(), 200_000);
        assert_eq!(result.max_output_tokens(), 8_000); // inner default
    }

    #[test]
    fn wrap_if_needed_wraps_when_max_output_tokens_override_present() {
        let inner: Arc<dyn Model> = Arc::new(MockModel::new());
        let result = ModelOverrideWrapper::wrap_if_needed(inner, None, Some(16_000));
        assert_eq!(result.context_window(), 100_000); // inner default
        assert_eq!(result.max_output_tokens(), 16_000);
    }

    #[test]
    fn wrap_if_needed_wraps_when_both_overrides_present() {
        let inner: Arc<dyn Model> = Arc::new(MockModel::new());
        let result = ModelOverrideWrapper::wrap_if_needed(inner, Some(50_000), Some(4_000));
        assert_eq!(result.context_window(), 50_000);
        assert_eq!(result.max_output_tokens(), 4_000);
    }

    #[test]
    fn delegated_methods_pass_through() {
        let inner: Arc<dyn Model> = Arc::new(MockModel::new());
        let wrapped = ModelOverrideWrapper::wrap_if_needed(inner, Some(50_000), Some(4_000));
        assert_eq!(wrapped.name(), "mock-model");
        assert_eq!(wrapped.provider(), "mock-provider");
        assert!(wrapped.supports_tools());
        assert_eq!(wrapped.input_cost_per_million(), 3.0);
        assert_eq!(wrapped.output_cost_per_million(), 15.0);
    }

    #[tokio::test]
    async fn complete_delegates_to_inner() {
        let inner: Arc<dyn Model> = Arc::new(MockModel::new());
        let wrapped = ModelOverrideWrapper::wrap_if_needed(inner, Some(50_000), None);
        let request = ModelRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            output_schema: None,
        };
        let response = wrapped.complete(request).await.unwrap();
        assert_eq!(response.stop_reason, StopReason::EndTurn);
    }

    // --- Property-Based Tests ---

    /// Strategy to generate arbitrary MockModel parameters.
    fn arb_mock_model() -> impl Strategy<Value = MockModel> {
        (
            "[a-z][a-z0-9-]{0,20}",       // name
            "[a-z][a-z0-9-]{0,15}",       // provider
            1usize..=1_000_000usize,       // context_window
            1usize..=100_000usize,         // max_output_tokens
            any::<bool>(),                 // supports_tools
            0.0f64..1000.0f64,             // input_cost
            0.0f64..1000.0f64,             // output_cost
        )
            .prop_map(
                |(name, provider, context_window, max_output_tokens, supports_tools, input_cost, output_cost)| {
                    MockModel {
                        name,
                        provider,
                        context_window,
                        max_output_tokens,
                        supports_tools,
                        input_cost,
                        output_cost,
                    }
                },
            )
    }

    /// Strategy to generate arbitrary override values (Option<usize>).
    fn arb_override() -> impl Strategy<Value = Option<usize>> {
        prop_oneof![
            Just(None),
            (1usize..=2_000_000usize).prop_map(Some),
        ]
    }

    proptest! {
        /// **Property 10: ModelOverrideWrapper passes through non-overridden methods**
        ///
        /// For any Model instance wrapped with ModelOverrideWrapper, calling name(),
        /// provider(), supports_tools(), input_cost_per_million(), and
        /// output_cost_per_million() SHALL return the same values as the inner model.
        ///
        /// **Validates: Requirements 7.2, 7.3**
        #[test]
        fn prop_passthrough_non_overridden_methods(
            mock in arb_mock_model(),
            ctx_override in arb_override(),
            max_override in arb_override(),
        ) {
            let expected_name = mock.name.clone();
            let expected_provider = mock.provider.clone();
            let expected_supports_tools = mock.supports_tools;
            let expected_input_cost = mock.input_cost;
            let expected_output_cost = mock.output_cost;

            let inner: Arc<dyn Model> = Arc::new(mock);
            let wrapped = ModelOverrideWrapper::new(inner, ctx_override, max_override);

            prop_assert_eq!(wrapped.name(), expected_name.as_str());
            prop_assert_eq!(wrapped.provider(), expected_provider.as_str());
            prop_assert_eq!(wrapped.supports_tools(), expected_supports_tools);
            prop_assert_eq!(wrapped.input_cost_per_million(), expected_input_cost);
            prop_assert_eq!(wrapped.output_cost_per_million(), expected_output_cost);
        }

        /// **Property 11: ModelOverrideWrapper applies context_window override**
        ///
        /// For any Model instance and any context_window override value, wrapping the
        /// model with that override SHALL cause context_window() to return the override
        /// value.
        ///
        /// **Validates: Requirements 7.2, 7.3**
        #[test]
        fn prop_context_window_override_applied(
            mock in arb_mock_model(),
            override_value in 1usize..=2_000_000usize,
        ) {
            let inner: Arc<dyn Model> = Arc::new(mock);
            let wrapped = ModelOverrideWrapper::new(
                inner,
                Some(override_value),
                None,
            );

            prop_assert_eq!(wrapped.context_window(), override_value);
        }

        /// **Property 12: ModelOverrideWrapper applies max_output_tokens override**
        ///
        /// For any Model instance and any max_output_tokens override value, wrapping
        /// the model with that override SHALL cause max_output_tokens() to return the
        /// override value.
        ///
        /// **Validates: Requirements 7.2, 7.3**
        #[test]
        fn prop_max_output_tokens_override_applied(
            mock in arb_mock_model(),
            override_value in 1usize..=2_000_000usize,
        ) {
            let inner: Arc<dyn Model> = Arc::new(mock);
            let wrapped = ModelOverrideWrapper::new(
                inner,
                None,
                Some(override_value),
            );

            prop_assert_eq!(wrapped.max_output_tokens(), override_value);
        }
    }
}
