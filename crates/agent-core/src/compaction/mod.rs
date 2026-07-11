//! 3-layer memory compaction system.
//!
//! A trait-based pipeline that executes layers in order from lightest to heaviest:
//!
//! 1. **Tools Compact** — clears stale tool results (zero cost)
//! 2. **Session Memory** — injects session memory file (zero cost)
//! 3. **Full Summarize** — model-based structured summary (one LLM call)
//!
//! The `CompactionPipeline` orchestrator is invoked once per turn and stops
//! at the first layer that successfully reduces token count below the trigger threshold.

pub mod config;
pub mod full_summarize;
pub mod layer;
pub mod session_memory;
pub mod tokens;
pub mod tools_compact;

/// Event returned when compaction modifies the message history.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionEvent {
    /// Name of the stage that was applied (e.g., "tools_compact", "full_summarize").
    pub stage: String,
    /// Number of messages removed or affected.
    pub messages_affected: usize,
    /// Estimated token count before compaction.
    pub tokens_before: usize,
    /// Estimated token count after compaction.
    pub tokens_after: usize,
}

use crate::message::Message;
use crate::model::Model;
use crate::state::CompactionState;

use self::config::CompactionLayerConfig;
use self::full_summarize::FullSummarizeLayer;
use self::layer::{CompactionContext, CompactionLayer, LayerResult};
use self::session_memory::SessionMemoryLayer;
use self::tools_compact::ToolsCompactLayer;

/// The main pipeline that coordinates all compaction layers.
///
/// Executes layers in order from lightest to heaviest, stopping at the first
/// layer that successfully reduces token count below the trigger threshold.
/// Includes a circuit breaker that disables compaction after 3 consecutive failures.
pub struct CompactionPipeline {
    /// The synchronous layers executed in order (ToolsCompact, SessionMemory).
    sync_layers: Vec<Box<dyn CompactionLayer>>,
    /// The async Full Summarize layer (executed only if sync layers are insufficient).
    full_summarize: Option<FullSummarizeLayer>,
    /// Consecutive failure count for the circuit breaker.
    consecutive_failures: u32,
    /// Whether the circuit breaker has tripped (disabling further compaction).
    circuit_broken: bool,
    /// Pipeline configuration.
    config: CompactionLayerConfig,
}

impl CompactionPipeline {
    /// Create a new pipeline with the given configuration.
    ///
    /// Builds the sync layers vector (ToolsCompact, SessionMemory) and includes
    /// the async FullSummarize layer.
    pub fn new(config: CompactionLayerConfig) -> Self {
        let sync_layers: Vec<Box<dyn CompactionLayer>> =
            vec![Box::new(ToolsCompactLayer), Box::new(SessionMemoryLayer)];
        Self {
            sync_layers,
            full_summarize: Some(FullSummarizeLayer),
            consecutive_failures: 0,
            circuit_broken: false,
            config,
        }
    }

    /// Run the compaction pipeline. This is the method called from the run loop.
    ///
    /// Executes layers in order:
    /// 1. Check circuit breaker — if tripped, skip compaction entirely.
    /// 2. Compute trigger threshold — if below, skip compaction.
    /// 3. Execute sync layers (ToolsCompact, SessionMemory) in order, short-circuit on Applied.
    /// 4. Execute FullSummarize (async) if model is available and sync layers were insufficient.
    /// 5. If all layers fail, increment failure counter (circuit breaker trips at 3).
    #[allow(clippy::too_many_arguments)]
    pub async fn compact(
        &mut self,
        messages: &mut Vec<Message>,
        state: &mut CompactionState,
        token_count: usize,
        context_window: usize,
        max_output_tokens: usize,
        current_turn: u32,
        model: Option<&dyn Model>,
    ) -> Option<CompactionEvent> {
        // Circuit breaker check
        if self.circuit_broken {
            tracing::warn!(
                "compaction disabled: circuit breaker active after 3 consecutive failures"
            );
            return None;
        }

        // Compute trigger threshold:
        // effective_context = context_window - max(max_output_tokens, 20_000)
        // trigger_threshold = effective_context - trigger_buffer
        let effective_context = context_window.saturating_sub(max_output_tokens.max(20_000));
        let trigger_threshold = effective_context.saturating_sub(self.config.trigger_buffer);

        // Skip if below threshold
        if token_count <= trigger_threshold {
            return None;
        }

        let context = CompactionContext {
            token_count,
            trigger_threshold,
            current_turn,
            config: self.config.clone(),
        };

        // Execute sync layers in order
        for layer in &self.sync_layers {
            match layer.apply(messages, &context) {
                LayerResult::Applied(event) => {
                    self.on_success(state, &event, current_turn);
                    return Some(event);
                }
                LayerResult::Noop => {
                    // Continue to next layer
                }
                LayerResult::Failed(reason) => {
                    tracing::debug!(layer = layer.name(), reason = %reason, "compaction layer failed");
                    // Continue to next layer
                }
            }
        }

        // Execute Full Summarize (async) if sync layers were insufficient
        if let (Some(ref summarizer), Some(model)) = (&self.full_summarize, model) {
            match summarizer.apply_async(messages, &context, model).await {
                LayerResult::Applied(event) => {
                    self.on_success(state, &event, current_turn);
                    return Some(event);
                }
                LayerResult::Noop => {}
                LayerResult::Failed(reason) => {
                    tracing::warn!(reason = %reason, "full_summarize_failed");
                }
            }
        }

        // All layers failed to reduce below threshold
        self.on_failure();
        None
    }

    /// Called when a compaction layer succeeds.
    /// Resets the failure counter and updates compaction state.
    fn on_success(&mut self, state: &mut CompactionState, event: &CompactionEvent, turn: u32) {
        self.consecutive_failures = 0;
        state.total_compactions += 1;
        state.messages_removed += event.messages_affected;
        state.last_compaction_turn = Some(turn);
    }

    /// Called when all layers fail to compact.
    /// Increments consecutive failures and trips the circuit breaker at 3.
    fn on_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= 3 {
            self.circuit_broken = true;
        }
    }

    /// Returns whether the circuit breaker is currently tripped.
    pub fn is_circuit_broken(&self) -> bool {
        self.circuit_broken
    }

    /// Returns the number of consecutive failures tracked.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Returns a reference to the pipeline's configuration.
    pub fn config(&self) -> &CompactionLayerConfig {
        &self.config
    }
}

// ============================================================================
// Property-based tests for CompactionPipeline
// Validates: Requirements 1.2, 1.3, 1.4, 5.1, 5.2, 7.1, 7.2, 7.3
// ============================================================================

#[cfg(test)]
mod pipeline_tests {
    use super::*;
    use crate::error::ModelError;
    use crate::message::{ContentBlock, ToolUseBlock};
    use crate::model::{Model, ModelRequest, ModelResponse, ModelStream};
    use async_trait::async_trait;
    use proptest::prelude::*;
    use serde_json::json;

    // ========================================================================
    // Mock Model implementations
    // ========================================================================

    /// A mock model that always fails (to test circuit breaker and layer fallthrough).
    struct FailMockModel;

    #[async_trait]
    impl Model for FailMockModel {
        async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
            unimplemented!()
        }
        async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
            Err(ModelError::Connection("simulated failure".to_string()))
        }
        fn name(&self) -> &str {
            "fail-mock"
        }
        fn provider(&self) -> &str {
            "mock"
        }
        fn context_window(&self) -> usize {
            200_000
        }
        fn max_output_tokens(&self) -> usize {
            8192
        }
        fn supports_tools(&self) -> bool {
            false
        }
        fn input_cost_per_million(&self) -> f64 {
            0.0
        }
        fn output_cost_per_million(&self) -> f64 {
            0.0
        }
    }

    // ========================================================================
    // Helpers
    // ========================================================================

    fn user_msg(text: &str) -> Message {
        Message::User {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant {
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            usage: None,
        }
    }

    fn assistant_tool_use(tool_id: &str, tool_name: &str) -> Message {
        Message::Assistant {
            content: vec![ContentBlock::ToolUse {
                block: ToolUseBlock {
                    id: tool_id.to_string(),
                    name: tool_name.to_string(),
                    input: json!({}),
                },
            }],
            usage: None,
        }
    }

    fn tool_result_msg(tool_use_id: &str, content: &str) -> Message {
        Message::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error: false,
        }
    }

    fn system_msg(content: &str) -> Message {
        Message::System {
            content: content.to_string(),
        }
    }

    /// Create messages that make ToolsCompact able to clear enough to go below threshold.
    /// Uses many old tool results from compactable tools with large content.
    fn messages_with_clearable_tool_results(
        num_old_turns: u32,
        content_size: usize,
    ) -> Vec<Message> {
        let big_content = "x".repeat(content_size);
        let mut messages = Vec::new();

        // Old turns with compactable tool results
        for i in 0..num_old_turns {
            messages.push(user_msg(&format!("Turn {}", i)));
            let tid = format!("t_{}", i);
            messages.push(assistant_tool_use(&tid, "file_read"));
            messages.push(tool_result_msg(&tid, &big_content));
        }

        // Recent exempt turns (5 by default)
        for i in num_old_turns..(num_old_turns + 6) {
            messages.push(user_msg(&format!("Recent turn {}", i)));
            messages.push(assistant_msg(&format!("Response {}", i)));
        }

        messages
    }

    // ========================================================================
    // Property 1: Pipeline executes layers in order and stops early on success
    //
    // For any message history that exceeds the trigger threshold, the compaction
    // pipeline SHALL execute layers from lightest (ToolsCompact) to heaviest (Full Summarize),
    // and SHALL stop executing subsequent layers as soon as any layer successfully
    // reduces the token count below the trigger threshold.
    //
    // **Validates: Requirements 1.2, 1.3, 1.4**
    // ========================================================================

    #[tokio::test]
    async fn test_pipeline_stops_at_tools_compact_when_sufficient() {
        // Build messages where tools_compact alone can reduce below threshold.
        // 10 old turns with 2000 chars each = 10 * 2000 / 4 = 5000 tokens in tool results alone.
        let mut messages = messages_with_clearable_tool_results(10, 2000);
        let mut state = CompactionState::default();

        let config = CompactionLayerConfig::default();
        let mut pipeline = CompactionPipeline::new(config);

        let token_count = tokens::estimate_tokens(&messages);
        // Set a threshold that clearing tool results will achieve.
        // The large tool results contribute most of the tokens. After clearing them,
        // we should be well below the trigger.
        let trigger_threshold_target = token_count - 3000; // need to shed ~3000 tokens

        // context_window and max_output_tokens chosen so threshold equals our target.
        // threshold = context_window - max(max_output_tokens, 20000) - 13000
        // trigger_threshold_target = context_window - 20000 - 13000
        // context_window = trigger_threshold_target + 33000
        let context_window = trigger_threshold_target + 33_000;
        let max_output_tokens = 8192;

        let result = pipeline
            .compact(
                &mut messages,
                &mut state,
                token_count,
                context_window,
                max_output_tokens,
                10,
                None, // no model — should succeed via ToolsCompact without needing FullSummarize
            )
            .await;

        // Should have applied tools_compact
        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.stage, "tools_compact");
        // Pipeline stopped at the first layer — state reflects success
        assert_eq!(state.total_compactions, 1);
        assert_eq!(pipeline.consecutive_failures(), 0);
    }

    #[tokio::test]
    async fn test_pipeline_fails_when_all_layers_insufficient() {
        // Messages that can't be cleared by tools_compact (no compactable tools)
        // and no session memory file. With only 2 non-system messages, FullSummarize
        // also won't trigger. Since snip no longer exists, the pipeline should
        // return None and increment the failure counter.
        let big = "z".repeat(4000); // 1000 tokens
        let mut messages = vec![
            system_msg("instruction"),
            user_msg(&big),
            assistant_msg(&big),
            user_msg("recent"),
        ];
        let mut state = CompactionState::default();
        let config = CompactionLayerConfig::default();
        let mut pipeline = CompactionPipeline::new(config);

        let token_count = tokens::estimate_tokens(&messages);
        // threshold below current tokens but no layer can reduce it
        let context_window = (token_count - 500) + 33_000;
        let max_output_tokens = 8192;

        let result = pipeline
            .compact(
                &mut messages,
                &mut state,
                token_count,
                context_window,
                max_output_tokens,
                5,
                Some(&FailMockModel), // model fails, so FullSummarize won't help
            )
            .await;

        // All layers failed — pipeline returns None and increments failure counter
        assert!(result.is_none());
        assert_eq!(pipeline.consecutive_failures(), 1);
        assert!(!pipeline.is_circuit_broken());
    }

    // ========================================================================
    // Property 9: Trigger threshold is correctly computed
    //
    // For any context_window value C and max_output_tokens value M, the trigger
    // threshold SHALL equal `C - max(M, 20000) - 13000`. Compaction SHALL
    // initiate if and only if the token count exceeds this threshold.
    //
    // **Validates: Requirements 5.1, 5.2**
    // ========================================================================

    proptest! {
        #[test]
        fn prop_trigger_threshold_correctly_computed(
            context_window in 50_000usize..=300_000,
            max_output_tokens in 1_000usize..=50_000,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let config = CompactionLayerConfig::default();
                let mut pipeline = CompactionPipeline::new(config);
                let mut state = CompactionState::default();

                // Expected threshold computation
                let effective_context = context_window.saturating_sub(max_output_tokens.max(20_000));
                let expected_threshold = effective_context.saturating_sub(13_000);

                // Case 1: token_count exactly AT threshold — should NOT compact (uses <=)
                let mut messages_at = vec![user_msg("hello")];
                let result = pipeline
                    .compact(
                        &mut messages_at,
                        &mut state,
                        expected_threshold, // exactly at threshold
                        context_window,
                        max_output_tokens,
                        1,
                        None,
                    )
                    .await;
                prop_assert!(result.is_none(),
                    "Compaction should NOT fire when token_count == threshold ({} == {})",
                    expected_threshold, expected_threshold);

                // Case 2: token_count below threshold — should NOT compact
                if expected_threshold > 0 {
                    let below = expected_threshold - 1;
                    let mut messages_below = vec![user_msg("hello")];
                    let result = pipeline
                        .compact(
                            &mut messages_below,
                            &mut state,
                            below,
                            context_window,
                            max_output_tokens,
                            1,
                            None,
                        )
                        .await;
                    prop_assert!(result.is_none(),
                        "Compaction should NOT fire when token_count < threshold ({} < {})",
                        below, expected_threshold);
                }

                // Case 3: token_count above threshold — should attempt compact
                // (It may still return None if layers can't reduce, but it won't
                // be skipped due to threshold check. We verify the pipeline state
                // changes to confirm it attempted compaction.)
                let above = expected_threshold + 1;
                // Create messages big enough to match the claimed token count.
                // Use a big system message that can't be compacted.
                let big_content = "a".repeat(above * 4);
                let mut messages_above = vec![
                    system_msg(&big_content),
                    user_msg("final user msg"),
                ];
                // With these messages, layers won't reduce (no compactable tool results,
                // no session memory, full_summarize needs > preserve_recent non-system msgs).
                // For this test we just need to confirm the pipeline DID try
                // (i.e., didn't skip due to threshold). We can detect this:
                // If the pipeline skips, consecutive_failures stays 0 and no event.
                // If it attempts and all layers fail, consecutive_failures increments.
                let mut pipeline2 = CompactionPipeline::new(CompactionLayerConfig::default());
                let mut state2 = CompactionState::default();
                let _result = pipeline2
                    .compact(
                        &mut messages_above,
                        &mut state2,
                        above,
                        context_window,
                        max_output_tokens,
                        1,
                        None,
                    )
                    .await;
                // Either it succeeded (some event) or it attempted and failed.
                // If token_count > threshold, it MUST have attempted.
                // System msg can't be removed, and no layer can reduce.
                // So all layers fail => consecutive_failures increments.
                // But we already verified Cases 1 and 2 (skip). Case 3 just needs to
                // confirm it didn't skip — i.e., it attempted compaction.
                // Consecutive failures increments to 1 or we get an event back.
                let attempted = _result.is_some() || pipeline2.consecutive_failures() > 0;
                prop_assert!(attempted,
                    "Compaction should attempt when token_count > threshold ({} > {})",
                    above, expected_threshold);

                Ok(())
            })?;
        }
    }

    // ========================================================================
    // Property 11: Circuit breaker activates after 3 consecutive failures and
    // resets on success
    //
    // For any sequence of compaction attempts, if 3 consecutive attempts fail to
    // reduce below threshold, the circuit breaker SHALL disable further compaction.
    // If any attempt succeeds (reduces below threshold), the consecutive failure
    // counter SHALL reset to zero.
    //
    // **Validates: Requirements 7.1, 7.2, 7.3**
    // ========================================================================

    #[tokio::test]
    async fn test_circuit_breaker_trips_after_3_failures() {
        // Create messages that no layer can compact:
        // - Only system + last user message (snip can't remove either)
        // - No compactable tool results (tools_compact noop)
        // - No session memory file
        // - Too few non-system messages for full_summarize
        let big = "q".repeat(8000); // 2000 tokens each
        let mut messages = vec![system_msg(&big), user_msg(&big)];
        let mut state = CompactionState::default();
        let config = CompactionLayerConfig::default();
        let mut pipeline = CompactionPipeline::new(config);

        let token_count = tokens::estimate_tokens(&messages); // 4000
                                                              // Set context_window so threshold is BELOW token_count
                                                              // threshold = context_window - max(max_output_tokens, 20000) - 13000
                                                              // We want threshold < 4000, e.g., threshold = 3000
                                                              // 3000 = context_window - 20000 - 13000 => context_window = 36000
        let context_window = 36_000;
        let max_output_tokens = 8192;
        // threshold = 36000 - 20000 - 13000 = 3000
        // token_count = 4000 > 3000 => pipeline fires

        // Attempt 1
        let r1 = pipeline
            .compact(
                &mut messages,
                &mut state,
                token_count,
                context_window,
                max_output_tokens,
                1,
                None,
            )
            .await;
        assert!(r1.is_none());
        assert_eq!(pipeline.consecutive_failures(), 1);
        assert!(!pipeline.is_circuit_broken());

        // Attempt 2
        let r2 = pipeline
            .compact(
                &mut messages,
                &mut state,
                token_count,
                context_window,
                max_output_tokens,
                2,
                None,
            )
            .await;
        assert!(r2.is_none());
        assert_eq!(pipeline.consecutive_failures(), 2);
        assert!(!pipeline.is_circuit_broken());

        // Attempt 3 — should trip circuit breaker
        let r3 = pipeline
            .compact(
                &mut messages,
                &mut state,
                token_count,
                context_window,
                max_output_tokens,
                3,
                None,
            )
            .await;
        assert!(r3.is_none());
        assert_eq!(pipeline.consecutive_failures(), 3);
        assert!(pipeline.is_circuit_broken());

        // Attempt 4 — circuit breaker is tripped, should skip entirely
        let r4 = pipeline
            .compact(
                &mut messages,
                &mut state,
                token_count,
                context_window,
                max_output_tokens,
                4,
                None,
            )
            .await;
        assert!(r4.is_none());
        // Failures don't increment further once broken
        assert_eq!(pipeline.consecutive_failures(), 3);
    }

    #[tokio::test]
    async fn test_circuit_breaker_resets_on_success() {
        let config = CompactionLayerConfig::default();
        let mut pipeline = CompactionPipeline::new(config);
        let mut state = CompactionState::default();

        // First: drive 2 failures with messages that can't be compacted
        let big = "q".repeat(8000); // 2000 tokens each
        let token_count = 4000usize; // 2 messages * 2000 tokens
                                     // threshold = 36000 - 20000 - 13000 = 3000 < 4000 => fires
        let context_window = 36_000;
        let max_output_tokens = 8192;

        let mut messages_fail = vec![system_msg(&big), user_msg(&big)];
        pipeline
            .compact(
                &mut messages_fail,
                &mut state,
                token_count,
                context_window,
                max_output_tokens,
                1,
                None,
            )
            .await;
        let mut messages_fail2 = vec![system_msg(&big), user_msg(&big)];
        pipeline
            .compact(
                &mut messages_fail2,
                &mut state,
                token_count,
                context_window,
                max_output_tokens,
                2,
                None,
            )
            .await;
        assert_eq!(pipeline.consecutive_failures(), 2);

        // Now succeed: messages with clearable tool results
        let mut messages_success = messages_with_clearable_tool_results(10, 2000);
        let token_count_success = tokens::estimate_tokens(&messages_success);
        // Set threshold that clearing will satisfy
        // We need threshold < token_count_success, but clearing tool results gets us below.
        // threshold = success_context_window - 20000 - 13000
        // Pick threshold at ~70% of current tokens (clearing should drop below that)
        let target_threshold = token_count_success * 7 / 10;
        let success_context_window = target_threshold + 20_000 + 13_000;

        let result = pipeline
            .compact(
                &mut messages_success,
                &mut state,
                token_count_success,
                success_context_window,
                max_output_tokens,
                3,
                None,
            )
            .await;
        assert!(result.is_some(), "Compaction should succeed");
        // Consecutive failures should be reset
        assert_eq!(pipeline.consecutive_failures(), 0);
        assert!(!pipeline.is_circuit_broken());
    }

    proptest! {
        /// Property test: For any sequence of N failures (0..=10) followed by a success,
        /// the circuit breaker state is predictable. If N < 3, success resets counter.
        /// If N >= 3, the circuit breaker trips and subsequent attempts are skipped.
        #[test]
        fn prop_circuit_breaker_behavior(
            num_failures in 0u32..=5,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let config = CompactionLayerConfig::default();
                let mut pipeline = CompactionPipeline::new(config);
                let mut state = CompactionState::default();

                let big = "q".repeat(8000);
                let token_count = 4000usize; // estimated from big: 8000/4 * 2 = 4000
                // threshold = 36000 - 20000 - 13000 = 3000 < 4000 => pipeline fires
                let context_window = 36_000;
                let max_output_tokens = 8192;

                // Drive num_failures consecutive failures
                for i in 0..num_failures {
                    let mut messages_fail = vec![
                        system_msg(&big),
                        user_msg(&big),
                    ];
                    pipeline.compact(
                        &mut messages_fail, &mut state, token_count,
                        context_window, max_output_tokens, i + 1, None
                    ).await;
                }

                if num_failures >= 3 {
                    // Circuit breaker should be tripped
                    prop_assert!(pipeline.is_circuit_broken(),
                        "Circuit breaker should trip after {} failures", num_failures);
                    prop_assert_eq!(pipeline.consecutive_failures(), 3u32);

                    // Further attempts should be skipped
                    let mut msgs = vec![system_msg(&big), user_msg(&big)];
                    let r = pipeline.compact(
                        &mut msgs, &mut state, token_count,
                        context_window, max_output_tokens, 99, None
                    ).await;
                    prop_assert!(r.is_none());
                } else {
                    // Not yet tripped
                    prop_assert!(!pipeline.is_circuit_broken());
                    prop_assert_eq!(pipeline.consecutive_failures(), num_failures);

                    // A success should reset the counter
                    let mut messages_success = messages_with_clearable_tool_results(10, 2000);
                    let tc = tokens::estimate_tokens(&messages_success);
                    // threshold needs to be below tc but achievable after clearing
                    let target_threshold = tc * 7 / 10;
                    let cw = target_threshold + 20_000 + 13_000;
                    let result = pipeline.compact(
                        &mut messages_success, &mut state, tc,
                        cw, max_output_tokens, 20, None
                    ).await;
                    prop_assert!(result.is_some(),
                        "Compaction should succeed with clearable tool results");
                    prop_assert_eq!(pipeline.consecutive_failures(), 0u32,
                        "Consecutive failures should reset to 0 after success");
                }

                Ok(())
            })?;
        }
    }
}
