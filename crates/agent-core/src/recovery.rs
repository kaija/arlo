//! Recovery system for the RunLoop.
//!
//! Provides error-to-strategy mapping and attempt tracking. When the model
//! returns an error during streaming, the recovery system determines the
//! appropriate strategy and tracks attempts per error variant, escalating
//! to GiveUp after MAX_RECOVERY_ATTEMPTS (3) failed attempts.

use std::collections::HashMap;

use crate::error::ModelError;
use crate::next_step::RecoveryStrategy;

/// Maximum number of recovery attempts per error variant before escalating to GiveUp.
pub const MAX_RECOVERY_ATTEMPTS: u32 = 3;

/// Tracks recovery attempts per error variant within a single run.
///
/// Each error variant is keyed by a discriminant string (e.g. "PromptTooLong",
/// "MaxOutputTokens"). When attempts for a variant exceed MAX_RECOVERY_ATTEMPTS,
/// the tracker returns a GiveUp strategy.
#[derive(Debug, Clone, Default)]
pub struct RecoveryTracker {
    attempts: HashMap<&'static str, u32>,
}

impl RecoveryTracker {
    /// Create a new empty recovery tracker.
    pub fn new() -> Self {
        Self {
            attempts: HashMap::new(),
        }
    }

    /// Determine the recovery strategy for a given ModelError.
    ///
    /// Increments the attempt count for the error's variant. If attempts exceed
    /// MAX_RECOVERY_ATTEMPTS, returns GiveUp. Otherwise, returns the appropriate
    /// strategy for the error type.
    pub fn resolve_strategy(&mut self, error: &ModelError) -> RecoveryStrategy {
        let variant_key = error_variant_key(error);
        let count = self.attempts.entry(variant_key).or_insert(0);
        *count += 1;

        if *count > MAX_RECOVERY_ATTEMPTS {
            return RecoveryStrategy::GiveUp {
                error: format!(
                    "Recovery exhausted after {} attempts for {}",
                    MAX_RECOVERY_ATTEMPTS, variant_key
                ),
            };
        }

        map_error_to_strategy(error, *count)
    }

    /// Get the current attempt count for a given error variant.
    pub fn attempts_for(&self, error: &ModelError) -> u32 {
        self.attempts
            .get(error_variant_key(error))
            .copied()
            .unwrap_or(0)
    }

    /// Get the current attempt count for a given variant key string.
    pub fn attempts_for_key(&self, key: &str) -> u32 {
        self.attempts.get(key).copied().unwrap_or(0)
    }

    /// Increment the attempt counter for a given variant key string.
    pub fn increment_key(&mut self, key: &'static str) {
        let count = self.attempts.entry(key).or_insert(0);
        *count += 1;
    }

    /// Reset all tracked attempts.
    pub fn reset(&mut self) {
        self.attempts.clear();
    }
}

/// Map a ModelError to its initial recovery strategy.
///
/// - PromptTooLong → CompactAndRetry
/// - MaxOutputTokens → ContinueMessage (first 2 attempts), then EscalateOutputTokens
/// - StreamInterrupted → ContinueMessage
/// - All others → GiveUp (no configured recovery strategy)
fn map_error_to_strategy(error: &ModelError, attempt: u32) -> RecoveryStrategy {
    match error {
        ModelError::PromptTooLong { .. } => RecoveryStrategy::CompactAndRetry,

        ModelError::MaxOutputTokens => {
            // First 2 attempts: continue message. 3rd attempt: escalate output tokens.
            if attempt <= 2 {
                RecoveryStrategy::ContinueMessage { attempt }
            } else {
                RecoveryStrategy::EscalateOutputTokens { max: 0 } // max will be filled by the caller
            }
        }

        ModelError::StreamInterrupted(_) => RecoveryStrategy::ContinueMessage { attempt },

        // No configured recovery for these error types
        _ => RecoveryStrategy::GiveUp {
            error: format!("No recovery strategy for: {}", error),
        },
    }
}

/// Extract a string key from the error variant for tracking purposes.
fn error_variant_key(error: &ModelError) -> &'static str {
    match error {
        ModelError::Api { .. } => "Api",
        ModelError::RateLimited { .. } => "RateLimited",
        ModelError::PromptTooLong { .. } => "PromptTooLong",
        ModelError::MaxOutputTokens => "MaxOutputTokens",
        ModelError::Connection(_) => "Connection",
        ModelError::StreamInterrupted(_) => "StreamInterrupted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_prompt_too_long_maps_to_compact_and_retry() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::PromptTooLong { tokens: 200000 };
        let strategy = tracker.resolve_strategy(&error);
        assert_eq!(strategy, RecoveryStrategy::CompactAndRetry);
    }

    #[test]
    fn test_max_output_tokens_maps_to_continue_message() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::MaxOutputTokens;
        let strategy = tracker.resolve_strategy(&error);
        assert_eq!(strategy, RecoveryStrategy::ContinueMessage { attempt: 1 });
    }

    #[test]
    fn test_max_output_tokens_escalates_on_third_attempt() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::MaxOutputTokens;

        // First attempt
        let s1 = tracker.resolve_strategy(&error);
        assert_eq!(s1, RecoveryStrategy::ContinueMessage { attempt: 1 });

        // Second attempt
        let s2 = tracker.resolve_strategy(&error);
        assert_eq!(s2, RecoveryStrategy::ContinueMessage { attempt: 2 });

        // Third attempt escalates to EscalateOutputTokens
        let s3 = tracker.resolve_strategy(&error);
        assert_eq!(s3, RecoveryStrategy::EscalateOutputTokens { max: 0 });
    }

    #[test]
    fn test_escalation_to_give_up_after_max_attempts() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::PromptTooLong { tokens: 200000 };

        // Attempt 1-3: CompactAndRetry
        for _ in 0..3 {
            let strategy = tracker.resolve_strategy(&error);
            assert_eq!(strategy, RecoveryStrategy::CompactAndRetry);
        }

        // Attempt 4: GiveUp
        let strategy = tracker.resolve_strategy(&error);
        assert!(matches!(strategy, RecoveryStrategy::GiveUp { .. }));
    }

    #[test]
    fn test_different_error_variants_tracked_independently() {
        let mut tracker = RecoveryTracker::new();
        let prompt_error = ModelError::PromptTooLong { tokens: 200000 };
        let output_error = ModelError::MaxOutputTokens;

        // Exhaust prompt recovery
        for _ in 0..3 {
            tracker.resolve_strategy(&prompt_error);
        }
        // 4th attempt for prompt should give up
        let s = tracker.resolve_strategy(&prompt_error);
        assert!(matches!(s, RecoveryStrategy::GiveUp { .. }));

        // But MaxOutputTokens should still be recoverable
        let s = tracker.resolve_strategy(&output_error);
        assert_eq!(s, RecoveryStrategy::ContinueMessage { attempt: 1 });
    }

    #[test]
    fn test_connection_error_gives_up_immediately() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::Connection("timeout".to_string());
        let strategy = tracker.resolve_strategy(&error);
        assert!(matches!(strategy, RecoveryStrategy::GiveUp { .. }));
    }

    #[test]
    fn test_api_error_gives_up_immediately() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::Api {
            status: 500,
            body: "server error".to_string(),
        };
        let strategy = tracker.resolve_strategy(&error);
        assert!(matches!(strategy, RecoveryStrategy::GiveUp { .. }));
    }

    #[test]
    fn test_stream_interrupted_maps_to_continue_message() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::StreamInterrupted("connection reset".to_string());
        let strategy = tracker.resolve_strategy(&error);
        assert_eq!(strategy, RecoveryStrategy::ContinueMessage { attempt: 1 });
    }

    #[test]
    fn test_attempts_for_returns_current_count() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::MaxOutputTokens;

        assert_eq!(tracker.attempts_for(&error), 0);
        tracker.resolve_strategy(&error);
        assert_eq!(tracker.attempts_for(&error), 1);
        tracker.resolve_strategy(&error);
        assert_eq!(tracker.attempts_for(&error), 2);
    }

    #[test]
    fn test_reset_clears_all_attempts() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::MaxOutputTokens;
        tracker.resolve_strategy(&error);
        tracker.resolve_strategy(&error);
        assert_eq!(tracker.attempts_for(&error), 2);

        tracker.reset();
        assert_eq!(tracker.attempts_for(&error), 0);
    }

    #[test]
    fn test_rate_limited_gives_up() {
        let mut tracker = RecoveryTracker::new();
        let error = ModelError::RateLimited {
            retry_after_ms: 5000,
        };
        let strategy = tracker.resolve_strategy(&error);
        assert!(matches!(strategy, RecoveryStrategy::GiveUp { .. }));
    }

    // Feature: rust-agent-framework, Property 17: Recovery escalation
    // **Validates: Requirements 18.6**
    //
    // For any recoverable ModelError variant (PromptTooLong, MaxOutputTokens,
    // StreamInterrupted), after MAX_RECOVERY_ATTEMPTS (3) calls to resolve_strategy(),
    // the next call must return GiveUp. Different error variants must be tracked
    // independently — exhausting one does not affect others.

    /// Enum representing recoverable error variant choices for property testing.
    #[derive(Debug, Clone)]
    enum RecoverableErrorKind {
        PromptTooLong { tokens: usize },
        MaxOutputTokens,
        StreamInterrupted { msg: String },
    }

    impl RecoverableErrorKind {
        /// Convert to a ModelError instance.
        fn to_model_error(&self) -> ModelError {
            match self {
                RecoverableErrorKind::PromptTooLong { tokens } => {
                    ModelError::PromptTooLong { tokens: *tokens }
                }
                RecoverableErrorKind::MaxOutputTokens => ModelError::MaxOutputTokens,
                RecoverableErrorKind::StreamInterrupted { msg } => {
                    ModelError::StreamInterrupted(msg.clone())
                }
            }
        }

        /// Get the variant key string (matches error_variant_key behavior).
        fn variant_key(&self) -> &'static str {
            match self {
                RecoverableErrorKind::PromptTooLong { .. } => "PromptTooLong",
                RecoverableErrorKind::MaxOutputTokens => "MaxOutputTokens",
                RecoverableErrorKind::StreamInterrupted { .. } => "StreamInterrupted",
            }
        }
    }

    /// Strategy to generate recoverable error variant kinds.
    fn arb_recoverable_error() -> impl Strategy<Value = RecoverableErrorKind> {
        prop_oneof![
            (1usize..1_000_000usize)
                .prop_map(|tokens| RecoverableErrorKind::PromptTooLong { tokens }),
            Just(RecoverableErrorKind::MaxOutputTokens),
            "[a-zA-Z0-9 _-]{1,30}".prop_map(|msg| RecoverableErrorKind::StreamInterrupted { msg }),
        ]
    }

    proptest! {
        #[test]
        fn prop_recovery_escalates_to_give_up_after_max_attempts(
            error_kind in arb_recoverable_error(),
        ) {
            let mut tracker = RecoveryTracker::new();

            // First MAX_RECOVERY_ATTEMPTS calls should NOT return GiveUp
            for i in 1..=MAX_RECOVERY_ATTEMPTS {
                let error = error_kind.to_model_error();
                let strategy = tracker.resolve_strategy(&error);
                prop_assert!(
                    !matches!(strategy, RecoveryStrategy::GiveUp { .. }),
                    "Attempt {} should not be GiveUp, got {:?}", i, strategy
                );
            }

            // The next call (attempt MAX_RECOVERY_ATTEMPTS + 1) MUST return GiveUp
            let error = error_kind.to_model_error();
            let strategy = tracker.resolve_strategy(&error);
            prop_assert!(
                matches!(strategy, RecoveryStrategy::GiveUp { .. }),
                "Attempt {} should be GiveUp, got {:?}",
                MAX_RECOVERY_ATTEMPTS + 1,
                strategy
            );
        }

        #[test]
        fn prop_recovery_different_variants_tracked_independently(
            error_a in arb_recoverable_error(),
            error_b in arb_recoverable_error(),
        ) {
            // Only test when the two errors have different variant keys
            prop_assume!(error_a.variant_key() != error_b.variant_key());

            let mut tracker = RecoveryTracker::new();

            // Exhaust error_a: call MAX_RECOVERY_ATTEMPTS + 1 times to get GiveUp
            for _ in 0..MAX_RECOVERY_ATTEMPTS {
                let err = error_a.to_model_error();
                tracker.resolve_strategy(&err);
            }
            let err_a = error_a.to_model_error();
            let strategy_a = tracker.resolve_strategy(&err_a);
            prop_assert!(
                matches!(strategy_a, RecoveryStrategy::GiveUp { .. }),
                "error_a should be GiveUp after exhaustion, got {:?}", strategy_a
            );

            // error_b should still be recoverable (first attempt)
            let err_b = error_b.to_model_error();
            let strategy_b = tracker.resolve_strategy(&err_b);
            prop_assert!(
                !matches!(strategy_b, RecoveryStrategy::GiveUp { .. }),
                "error_b should still be recoverable after error_a exhausted, got {:?}", strategy_b
            );

            // Verify the attempt count for error_b is only 1
            let err_b_check = error_b.to_model_error();
            prop_assert_eq!(tracker.attempts_for(&err_b_check), 1);
        }
    }
}
