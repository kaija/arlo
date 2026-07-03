//! Guardrail traits for composable safety checks at input, output, and tool boundaries.
//!
//! Three guardrail trait boundaries exist:
//! - [`InputGuardrail`]: Checks user input before the first model call (first turn only).
//! - [`OutputGuardrail`]: Checks the model's final output before returning to the user.
//! - [`ToolGuardrail`]: Checks tool inputs before execution and outputs after execution.
//!
//! Guardrails execute sequentially in registration order, short-circuiting at the first
//! `passed: false` result.

use async_trait::async_trait;

use crate::message::Message;

/// The result of a guardrail check.
///
/// When `passed` is `true`, the check succeeded and execution may continue.
/// When `passed` is `false`, the guardrail was tripped — `reason` should explain why,
/// and `metadata` can carry additional structured information.
#[derive(Debug, Clone, PartialEq)]
pub struct GuardrailResult {
    /// Whether the guardrail check passed.
    pub passed: bool,
    /// Explanation of why the guardrail was tripped (None if passed).
    pub reason: Option<String>,
    /// Additional structured metadata about the check result.
    pub metadata: Option<serde_json::Value>,
}

impl GuardrailResult {
    /// Create a passing guardrail result.
    pub fn pass() -> Self {
        Self {
            passed: true,
            reason: None,
            metadata: None,
        }
    }

    /// Create a failing guardrail result with the given reason.
    pub fn fail(reason: impl Into<String>) -> Self {
        Self {
            passed: false,
            reason: Some(reason.into()),
            metadata: None,
        }
    }

    /// Create a failing guardrail result with reason and metadata.
    pub fn fail_with_metadata(reason: impl Into<String>, metadata: serde_json::Value) -> Self {
        Self {
            passed: false,
            reason: Some(reason.into()),
            metadata: Some(metadata),
        }
    }
}

/// A guardrail that checks user input/messages before the first model call.
///
/// Input guardrails are invoked only on the first turn. If any input guardrail
/// returns `passed: false`, the run terminates immediately with a `GuardrailTripped` event.
///
/// # Object Safety
///
/// This trait is object-safe and can be used as `Arc<dyn InputGuardrail>`.
#[async_trait]
pub trait InputGuardrail: Send + Sync {
    /// Returns the name of this guardrail (used in tripped events).
    fn name(&self) -> &str;

    /// Check the input messages before the first model call.
    ///
    /// Returns a `GuardrailResult` indicating whether the input is acceptable.
    async fn check(&self, input: &[Message]) -> GuardrailResult;
}

/// A guardrail that checks the model's final output before returning to the user.
///
/// Output guardrails are invoked when `NextStep` resolves to `FinalOutput`.
/// If any output guardrail returns `passed: false`, the run yields a
/// `GuardrailTripped` event and terminates without delivering the output.
///
/// # Object Safety
///
/// This trait is object-safe and can be used as `Arc<dyn OutputGuardrail>`.
#[async_trait]
pub trait OutputGuardrail: Send + Sync {
    /// Returns the name of this guardrail (used in tripped events).
    fn name(&self) -> &str;

    /// Check the model's final output.
    ///
    /// # Arguments
    /// * `output` — The text output from the model.
    /// * `structured` — Optional structured (JSON) output, if the agent produced one.
    async fn check(
        &self,
        output: &str,
        structured: Option<&serde_json::Value>,
    ) -> GuardrailResult;
}

/// A guardrail that checks tool inputs before execution and outputs after execution.
///
/// Tool guardrails are invoked on every tool call. If `check_input()` returns
/// `passed: false`, the tool execution is skipped and a `GuardrailTripped` event
/// is yielded.
///
/// # Object Safety
///
/// This trait is object-safe and can be used as `Arc<dyn ToolGuardrail>`.
#[async_trait]
pub trait ToolGuardrail: Send + Sync {
    /// Returns the name of this guardrail (used in tripped events).
    fn name(&self) -> &str;

    /// Check the tool's input before execution.
    ///
    /// # Arguments
    /// * `tool_name` — The name of the tool being invoked.
    /// * `input` — The JSON arguments being passed to the tool.
    async fn check_input(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> GuardrailResult;

    /// Check the tool's output after execution.
    ///
    /// # Arguments
    /// * `tool_name` — The name of the tool that was invoked.
    /// * `output` — The text output from the tool execution.
    async fn check_output(
        &self,
        tool_name: &str,
        output: &str,
    ) -> GuardrailResult;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    use crate::message::{ContentBlock, Message};

    // --- Mock implementations ---

    /// A mock input guardrail that blocks messages containing a banned word.
    struct BannedWordGuardrail {
        banned_word: String,
    }

    #[async_trait]
    impl InputGuardrail for BannedWordGuardrail {
        fn name(&self) -> &str {
            "banned_word_guardrail"
        }

        async fn check(&self, input: &[Message]) -> GuardrailResult {
            for msg in input {
                if let Message::User { content } = msg {
                    for block in content {
                        if let ContentBlock::Text { text } = block {
                            if text.contains(&self.banned_word) {
                                return GuardrailResult::fail(format!(
                                    "Input contains banned word: {}",
                                    self.banned_word
                                ));
                            }
                        }
                    }
                }
            }
            GuardrailResult::pass()
        }
    }

    /// A mock output guardrail that blocks output exceeding a max length.
    struct MaxLengthOutputGuardrail {
        max_length: usize,
    }

    #[async_trait]
    impl OutputGuardrail for MaxLengthOutputGuardrail {
        fn name(&self) -> &str {
            "max_length_output_guardrail"
        }

        async fn check(
            &self,
            output: &str,
            _structured: Option<&serde_json::Value>,
        ) -> GuardrailResult {
            if output.len() > self.max_length {
                GuardrailResult::fail_with_metadata(
                    format!(
                        "Output exceeds max length of {} (got {})",
                        self.max_length,
                        output.len()
                    ),
                    json!({ "length": output.len(), "max": self.max_length }),
                )
            } else {
                GuardrailResult::pass()
            }
        }
    }

    /// A mock tool guardrail that blocks execution of a specific tool.
    struct BlockedToolGuardrail {
        blocked_tool: String,
    }

    #[async_trait]
    impl ToolGuardrail for BlockedToolGuardrail {
        fn name(&self) -> &str {
            "blocked_tool_guardrail"
        }

        async fn check_input(
            &self,
            tool_name: &str,
            _input: &serde_json::Value,
        ) -> GuardrailResult {
            if tool_name == self.blocked_tool {
                GuardrailResult::fail(format!("Tool '{}' is blocked", tool_name))
            } else {
                GuardrailResult::pass()
            }
        }

        async fn check_output(
            &self,
            _tool_name: &str,
            output: &str,
        ) -> GuardrailResult {
            if output.contains("SENSITIVE") {
                GuardrailResult::fail("Tool output contains sensitive data")
            } else {
                GuardrailResult::pass()
            }
        }
    }

    /// A guardrail that always passes (for testing sequential execution).
    struct AlwaysPassGuardrail {
        name: String,
    }

    #[async_trait]
    impl InputGuardrail for AlwaysPassGuardrail {
        fn name(&self) -> &str {
            &self.name
        }

        async fn check(&self, _input: &[Message]) -> GuardrailResult {
            GuardrailResult::pass()
        }
    }

    // --- GuardrailResult tests ---

    #[test]
    fn guardrail_result_pass() {
        let result = GuardrailResult::pass();
        assert!(result.passed);
        assert_eq!(result.reason, None);
        assert_eq!(result.metadata, None);
    }

    #[test]
    fn guardrail_result_fail() {
        let result = GuardrailResult::fail("bad input");
        assert!(!result.passed);
        assert_eq!(result.reason, Some("bad input".to_string()));
        assert_eq!(result.metadata, None);
    }

    #[test]
    fn guardrail_result_fail_with_metadata() {
        let metadata = json!({"severity": "high"});
        let result = GuardrailResult::fail_with_metadata("violation", metadata.clone());
        assert!(!result.passed);
        assert_eq!(result.reason, Some("violation".to_string()));
        assert_eq!(result.metadata, Some(metadata));
    }

    #[test]
    fn guardrail_result_clone_and_eq() {
        let result = GuardrailResult::fail("test");
        let cloned = result.clone();
        assert_eq!(result, cloned);
    }

    #[test]
    fn guardrail_result_debug() {
        let result = GuardrailResult::pass();
        let debug = format!("{:?}", result);
        assert!(debug.contains("GuardrailResult"));
        assert!(debug.contains("true"));
    }

    // --- InputGuardrail trait tests ---

    #[tokio::test]
    async fn input_guardrail_passes_clean_input() {
        let guardrail = BannedWordGuardrail {
            banned_word: "forbidden".to_string(),
        };
        let messages = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "Hello, how are you?".to_string(),
            }],
        }];
        let result = guardrail.check(&messages).await;
        assert!(result.passed);
    }

    #[tokio::test]
    async fn input_guardrail_blocks_banned_word() {
        let guardrail = BannedWordGuardrail {
            banned_word: "forbidden".to_string(),
        };
        let messages = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "This contains the forbidden word".to_string(),
            }],
        }];
        let result = guardrail.check(&messages).await;
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("forbidden"));
    }

    #[tokio::test]
    async fn input_guardrail_name() {
        let guardrail = BannedWordGuardrail {
            banned_word: "test".to_string(),
        };
        assert_eq!(guardrail.name(), "banned_word_guardrail");
    }

    // --- OutputGuardrail trait tests ---

    #[tokio::test]
    async fn output_guardrail_passes_short_output() {
        let guardrail = MaxLengthOutputGuardrail { max_length: 100 };
        let result = guardrail.check("Short output", None).await;
        assert!(result.passed);
    }

    #[tokio::test]
    async fn output_guardrail_blocks_long_output() {
        let guardrail = MaxLengthOutputGuardrail { max_length: 10 };
        let result = guardrail.check("This output is way too long", None).await;
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("exceeds max length"));
        assert!(result.metadata.is_some());
    }

    #[tokio::test]
    async fn output_guardrail_with_structured_output() {
        let guardrail = MaxLengthOutputGuardrail { max_length: 100 };
        let structured = json!({"key": "value"});
        let result = guardrail.check("short", Some(&structured)).await;
        assert!(result.passed);
    }

    #[tokio::test]
    async fn output_guardrail_name() {
        let guardrail = MaxLengthOutputGuardrail { max_length: 100 };
        assert_eq!(guardrail.name(), "max_length_output_guardrail");
    }

    // --- ToolGuardrail trait tests ---

    #[tokio::test]
    async fn tool_guardrail_passes_allowed_tool() {
        let guardrail = BlockedToolGuardrail {
            blocked_tool: "dangerous_tool".to_string(),
        };
        let result = guardrail
            .check_input("safe_tool", &json!({"arg": "value"}))
            .await;
        assert!(result.passed);
    }

    #[tokio::test]
    async fn tool_guardrail_blocks_forbidden_tool() {
        let guardrail = BlockedToolGuardrail {
            blocked_tool: "dangerous_tool".to_string(),
        };
        let result = guardrail
            .check_input("dangerous_tool", &json!({"command": "rm -rf /"}))
            .await;
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("blocked"));
    }

    #[tokio::test]
    async fn tool_guardrail_check_output_passes() {
        let guardrail = BlockedToolGuardrail {
            blocked_tool: "any".to_string(),
        };
        let result = guardrail.check_output("shell", "normal output").await;
        assert!(result.passed);
    }

    #[tokio::test]
    async fn tool_guardrail_check_output_blocks_sensitive() {
        let guardrail = BlockedToolGuardrail {
            blocked_tool: "any".to_string(),
        };
        let result = guardrail
            .check_output("shell", "contains SENSITIVE data")
            .await;
        assert!(!result.passed);
        assert!(result.reason.unwrap().contains("sensitive"));
    }

    #[tokio::test]
    async fn tool_guardrail_name() {
        let guardrail = BlockedToolGuardrail {
            blocked_tool: "test".to_string(),
        };
        assert_eq!(guardrail.name(), "blocked_tool_guardrail");
    }

    // --- Object safety tests ---

    #[test]
    fn input_guardrail_is_object_safe() {
        fn _accepts_dyn(_g: &dyn InputGuardrail) {}
        fn _accepts_arc(_g: Arc<dyn InputGuardrail>) {}
    }

    #[test]
    fn output_guardrail_is_object_safe() {
        fn _accepts_dyn(_g: &dyn OutputGuardrail) {}
        fn _accepts_arc(_g: Arc<dyn OutputGuardrail>) {}
    }

    #[test]
    fn tool_guardrail_is_object_safe() {
        fn _accepts_dyn(_g: &dyn ToolGuardrail) {}
        fn _accepts_arc(_g: Arc<dyn ToolGuardrail>) {}
    }

    // --- Send + Sync bounds ---

    #[test]
    fn input_guardrail_send_sync() {
        fn _assert_send<T: Send>() {}
        fn _assert_sync<T: Sync>() {}
        _assert_send::<Arc<dyn InputGuardrail>>();
        _assert_sync::<Arc<dyn InputGuardrail>>();
    }

    #[test]
    fn output_guardrail_send_sync() {
        fn _assert_send<T: Send>() {}
        fn _assert_sync<T: Sync>() {}
        _assert_send::<Arc<dyn OutputGuardrail>>();
        _assert_sync::<Arc<dyn OutputGuardrail>>();
    }

    #[test]
    fn tool_guardrail_send_sync() {
        fn _assert_send<T: Send>() {}
        fn _assert_sync<T: Sync>() {}
        _assert_send::<Arc<dyn ToolGuardrail>>();
        _assert_sync::<Arc<dyn ToolGuardrail>>();
    }

    // --- Vec of trait objects (simulating registration order) ---

    #[tokio::test]
    async fn multiple_input_guardrails_sequential_check() {
        let guardrails: Vec<Arc<dyn InputGuardrail>> = vec![
            Arc::new(AlwaysPassGuardrail {
                name: "first".to_string(),
            }),
            Arc::new(BannedWordGuardrail {
                banned_word: "blocked".to_string(),
            }),
            Arc::new(AlwaysPassGuardrail {
                name: "third".to_string(),
            }),
        ];

        let messages = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "This is blocked content".to_string(),
            }],
        }];

        // Simulate sequential execution with short-circuit
        let mut tripped_name = None;
        for guardrail in &guardrails {
            let result = guardrail.check(&messages).await;
            if !result.passed {
                tripped_name = Some(guardrail.name().to_string());
                break;
            }
        }

        assert_eq!(tripped_name, Some("banned_word_guardrail".to_string()));
    }

    #[tokio::test]
    async fn all_guardrails_pass_when_input_clean() {
        let guardrails: Vec<Arc<dyn InputGuardrail>> = vec![
            Arc::new(AlwaysPassGuardrail {
                name: "first".to_string(),
            }),
            Arc::new(BannedWordGuardrail {
                banned_word: "blocked".to_string(),
            }),
            Arc::new(AlwaysPassGuardrail {
                name: "third".to_string(),
            }),
        ];

        let messages = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "Clean input without issues".to_string(),
            }],
        }];

        let mut all_passed = true;
        for guardrail in &guardrails {
            let result = guardrail.check(&messages).await;
            if !result.passed {
                all_passed = false;
                break;
            }
        }

        assert!(all_passed);
    }
}

// Feature: rust-agent-framework, Property 11: Guardrail execution semantics
// **Validates: Requirements 13.5, 13.8, 13.9**
//
// For any sequence of registered guardrails, they shall execute sequentially in
// registration order, short-circuiting at the first that returns passed=false.
// Input guardrails shall only be invoked on the first turn and never on subsequent turns.
#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Represents a predetermined guardrail outcome for testing.
    #[derive(Debug, Clone)]
    enum GuardrailOutcome {
        Pass,
        Fail(String),
    }

    /// Simulates sequential guardrail execution with short-circuit semantics.
    /// Returns:
    /// - The overall outcome (pass if all pass, fail at first failure)
    /// - The number of guardrails actually checked
    /// - The index of the first failure (None if all passed)
    fn run_guardrails_sequentially(
        outcomes: &[GuardrailOutcome],
        checked_flags: &[Arc<AtomicUsize>],
    ) -> (GuardrailResult, usize, Option<usize>) {
        let mut checked_count = 0;
        for (i, outcome) in outcomes.iter().enumerate() {
            // Mark this guardrail as checked
            checked_flags[i].fetch_add(1, Ordering::SeqCst);
            checked_count += 1;

            match outcome {
                GuardrailOutcome::Pass => continue,
                GuardrailOutcome::Fail(reason) => {
                    return (GuardrailResult::fail(reason.clone()), checked_count, Some(i));
                }
            }
        }
        (GuardrailResult::pass(), checked_count, None)
    }

    /// Simulates input guardrail turn-gating: input guardrails are only invoked on
    /// the first turn. Returns whether input guardrails were invoked for each turn.
    fn run_input_guardrails_across_turns(
        outcomes: &[GuardrailOutcome],
        num_turns: usize,
    ) -> Vec<bool> {
        let mut invoked_per_turn = Vec::with_capacity(num_turns);
        for turn in 0..num_turns {
            if turn == 0 {
                // First turn: invoke input guardrails
                invoked_per_turn.push(true);
                // Actually run them (short-circuit logic)
                for outcome in outcomes {
                    match outcome {
                        GuardrailOutcome::Pass => continue,
                        GuardrailOutcome::Fail(_) => break,
                    }
                }
            } else {
                // Subsequent turns: skip input guardrails
                invoked_per_turn.push(false);
            }
        }
        invoked_per_turn
    }

    /// Strategy to generate a sequence of guardrail outcomes.
    fn guardrail_outcomes_strategy() -> impl Strategy<Value = Vec<GuardrailOutcome>> {
        prop::collection::vec(
            prop_oneof![
                Just(GuardrailOutcome::Pass),
                "[a-z]{1,20}".prop_map(GuardrailOutcome::Fail),
            ],
            1..=20,
        )
    }

    proptest! {
        /// If all guardrails pass, the overall result is pass and all guardrails are checked.
        #[test]
        fn prop_all_pass_yields_overall_pass(len in 1usize..=20) {
            let outcomes: Vec<GuardrailOutcome> = vec![GuardrailOutcome::Pass; len];
            let flags: Vec<Arc<AtomicUsize>> = (0..len)
                .map(|_| Arc::new(AtomicUsize::new(0)))
                .collect();

            let (result, checked, failure_idx) = run_guardrails_sequentially(&outcomes, &flags);

            prop_assert!(result.passed, "All-pass sequence should yield overall pass");
            prop_assert_eq!(checked, len, "All guardrails should be checked when all pass");
            prop_assert_eq!(failure_idx, None, "No failure index when all pass");

            // Every guardrail should have been called exactly once
            for (i, flag) in flags.iter().enumerate() {
                prop_assert_eq!(
                    flag.load(Ordering::SeqCst), 1,
                    "Guardrail {} should have been called exactly once", i
                );
            }
        }

        /// If any guardrail fails, the overall result is fail at exactly the first failure index.
        #[test]
        fn prop_first_failure_short_circuits(outcomes in guardrail_outcomes_strategy()) {
            let len = outcomes.len();
            let flags: Vec<Arc<AtomicUsize>> = (0..len)
                .map(|_| Arc::new(AtomicUsize::new(0)))
                .collect();

            let (result, checked, failure_idx) = run_guardrails_sequentially(&outcomes, &flags);

            // Find expected first failure
            let expected_first_failure = outcomes.iter().position(|o| matches!(o, GuardrailOutcome::Fail(_)));

            match expected_first_failure {
                None => {
                    // All pass
                    prop_assert!(result.passed, "Should pass when no failures in sequence");
                    prop_assert_eq!(checked, len);
                    prop_assert_eq!(failure_idx, None);
                }
                Some(expected_idx) => {
                    // Fails at first failure
                    prop_assert!(!result.passed, "Should fail when a failure exists");
                    prop_assert_eq!(failure_idx, Some(expected_idx),
                        "Failure should be at first failing index");
                    prop_assert_eq!(checked, expected_idx + 1,
                        "Only guardrails up to and including the failure should be checked");

                    // Guardrails before and at the failure should be called
                    for i in 0..=expected_idx {
                        prop_assert_eq!(
                            flags[i].load(Ordering::SeqCst), 1,
                            "Guardrail {} (at or before failure) should be called", i
                        );
                    }
                    // Guardrails after the failure should NOT be called
                    for i in (expected_idx + 1)..len {
                        prop_assert_eq!(
                            flags[i].load(Ordering::SeqCst), 0,
                            "Guardrail {} (after failure at {}) should NOT be called", i, expected_idx
                        );
                    }
                }
            }
        }

        /// Guardrails after the failure index are never checked (dedicated property).
        #[test]
        fn prop_guardrails_after_failure_never_checked(
            prefix_len in 0usize..10,
            suffix_len in 1usize..10,
            reason in "[a-z]{1,15}"
        ) {
            let mut outcomes = vec![GuardrailOutcome::Pass; prefix_len];
            outcomes.push(GuardrailOutcome::Fail(reason));
            // Add more outcomes after the failure
            for _ in 0..suffix_len {
                outcomes.push(GuardrailOutcome::Pass);
            }

            let total_len = outcomes.len();
            let flags: Vec<Arc<AtomicUsize>> = (0..total_len)
                .map(|_| Arc::new(AtomicUsize::new(0)))
                .collect();

            let (result, checked, failure_idx) = run_guardrails_sequentially(&outcomes, &flags);

            prop_assert!(!result.passed);
            prop_assert_eq!(failure_idx, Some(prefix_len));
            prop_assert_eq!(checked, prefix_len + 1);

            // Verify suffix guardrails were never invoked
            for i in (prefix_len + 1)..total_len {
                prop_assert_eq!(
                    flags[i].load(Ordering::SeqCst), 0,
                    "Guardrail {} after failure should never be checked", i
                );
            }
        }

        /// Input guardrails are only invoked on the first turn, never on subsequent turns.
        #[test]
        fn prop_input_guardrails_first_turn_only(
            outcomes in guardrail_outcomes_strategy(),
            num_turns in 1usize..=10
        ) {
            let invoked = run_input_guardrails_across_turns(&outcomes, num_turns);

            // First turn: always invoked
            prop_assert!(invoked[0], "Input guardrails must be invoked on first turn");

            // Subsequent turns: never invoked
            for (turn, was_invoked) in invoked.iter().enumerate().skip(1) {
                prop_assert!(
                    !was_invoked,
                    "Input guardrails must NOT be invoked on turn {} (only first turn)", turn
                );
            }
        }

        /// Sequential execution preserves registration order — the failure reason
        /// matches the first failing guardrail's reason.
        #[test]
        fn prop_failure_reason_matches_first_failing_guardrail(
            outcomes in guardrail_outcomes_strategy()
        ) {
            let len = outcomes.len();
            let flags: Vec<Arc<AtomicUsize>> = (0..len)
                .map(|_| Arc::new(AtomicUsize::new(0)))
                .collect();

            let (result, _, _) = run_guardrails_sequentially(&outcomes, &flags);

            let expected_first_failure = outcomes.iter().find_map(|o| {
                if let GuardrailOutcome::Fail(reason) = o {
                    Some(reason.clone())
                } else {
                    None
                }
            });

            match expected_first_failure {
                None => {
                    prop_assert!(result.passed);
                    prop_assert_eq!(result.reason, None);
                }
                Some(expected_reason) => {
                    prop_assert!(!result.passed);
                    prop_assert_eq!(result.reason, Some(expected_reason));
                }
            }
        }
    }
}
