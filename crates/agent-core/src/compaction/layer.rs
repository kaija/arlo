//! Core trait and types for compaction layers.
//!
//! Defines the `CompactionLayer` trait, `LayerResult` enum, and `CompactionContext` struct.

use crate::message::Message;

use super::config::CompactionLayerConfig;
use super::CompactionEvent;

/// Result of a single layer's execution attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerResult {
    /// Layer successfully modified messages.
    Applied(CompactionEvent),
    /// Layer determined it has nothing to do (e.g., no eligible tool results).
    Noop,
    /// Layer encountered an error and could not complete.
    Failed(String),
}

/// Trait for synchronous compaction layers.
pub trait CompactionLayer: Send + Sync {
    /// Apply this layer's compaction logic to the message history.
    fn apply(&self, messages: &mut Vec<Message>, context: &CompactionContext) -> LayerResult;

    /// Human-readable name for observability.
    fn name(&self) -> &str;
}

/// Context passed to compaction layers during execution.
#[derive(Debug, Clone)]
pub struct CompactionContext {
    /// Current token count (from Usage data or heuristic).
    pub token_count: usize,
    /// The computed trigger threshold.
    pub trigger_threshold: usize,
    /// Current turn number (for determining message age).
    pub current_turn: u32,
    /// Configuration parameters.
    pub config: CompactionLayerConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_result_noop_equality() {
        assert_eq!(LayerResult::Noop, LayerResult::Noop);
    }

    #[test]
    fn layer_result_failed_equality() {
        let a = LayerResult::Failed("error A".to_string());
        let b = LayerResult::Failed("error A".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn layer_result_applied_equality() {
        let event = CompactionEvent {
            stage: "test".to_string(),
            messages_affected: 2,
            tokens_before: 100,
            tokens_after: 50,
        };
        let a = LayerResult::Applied(event.clone());
        let b = LayerResult::Applied(event);
        assert_eq!(a, b);
    }

    #[test]
    fn layer_result_variants_are_distinct() {
        let event = CompactionEvent {
            stage: "x".to_string(),
            messages_affected: 1,
            tokens_before: 10,
            tokens_after: 5,
        };
        assert_ne!(LayerResult::Noop, LayerResult::Failed("err".to_string()));
        assert_ne!(LayerResult::Noop, LayerResult::Applied(event));
    }

    #[test]
    fn compaction_context_debug() {
        let ctx = CompactionContext {
            token_count: 150_000,
            trigger_threshold: 167_000,
            current_turn: 12,
            config: CompactionLayerConfig::default(),
        };
        let debug_str = format!("{:?}", ctx);
        assert!(debug_str.contains("token_count"));
        assert!(debug_str.contains("150000"));
    }

    #[test]
    fn compaction_context_clone() {
        let ctx = CompactionContext {
            token_count: 100,
            trigger_threshold: 200,
            current_turn: 5,
            config: CompactionLayerConfig::default(),
        };
        let cloned = ctx.clone();
        assert_eq!(cloned.token_count, 100);
        assert_eq!(cloned.trigger_threshold, 200);
        assert_eq!(cloned.current_turn, 5);
    }

    /// Verify the CompactionLayer trait is object-safe by creating a trait object.
    #[test]
    fn compaction_layer_trait_is_object_safe() {
        struct DummyLayer;
        impl CompactionLayer for DummyLayer {
            fn apply(
                &self,
                _messages: &mut Vec<Message>,
                _context: &CompactionContext,
            ) -> LayerResult {
                LayerResult::Noop
            }
            fn name(&self) -> &str {
                "dummy"
            }
        }

        let layer: Box<dyn CompactionLayer> = Box::new(DummyLayer);
        assert_eq!(layer.name(), "dummy");

        let ctx = CompactionContext {
            token_count: 0,
            trigger_threshold: 0,
            current_turn: 0,
            config: CompactionLayerConfig::default(),
        };
        let mut msgs: Vec<Message> = vec![];
        assert_eq!(layer.apply(&mut msgs, &ctx), LayerResult::Noop);
    }
}
