//! Property-based tests for StreamingToolExecutor concurrency classification enforcement.
//!
//! Feature: rust-agent-framework, Property 4: Concurrency classification enforcement
//! **Validates: Requirements 10.2, 10.3, 10.8**
//!
//! Generates sequences of Safe/Exclusive tool enqueue operations, asserts Safe tools
//! can run in parallel and Exclusive tools run alone (zero overlap with any other tool).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use proptest::prelude::*;
use serde_json::json;
use tokio::runtime::Runtime;

use agent_core::{
    Concurrency, StreamingToolExecutor, Tool, ToolContext, ToolError, ToolOutput,
};

/// Recorded execution interval for a tool.
#[derive(Debug, Clone)]
struct ExecutionRecord {
    index: usize,
    concurrency: Concurrency,
    start: Instant,
    end: Instant,
}

/// A mock tool that records its execution start/end timestamps.
struct TimingTool {
    tool_name: String,
    classification: Concurrency,
    delay: Duration,
    records: Arc<Mutex<Vec<ExecutionRecord>>>,
    index: usize,
}

#[async_trait]
impl Tool for TimingTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        "timing tool for property tests"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
        self.classification
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let start = Instant::now();
        tokio::time::sleep(self.delay).await;
        let end = Instant::now();

        self.records.lock().unwrap().push(ExecutionRecord {
            index: self.index,
            concurrency: self.classification,
            start,
            end,
        });

        Ok(ToolOutput::Text(format!("done:{}", self.index)))
    }
}

fn make_ctx() -> ToolContext {
    ToolContext {
        session_id: "prop-test-session".to_string(),
        working_dir: PathBuf::from("/tmp/prop-test"),
    }
}

/// Check if two time intervals overlap.
/// Two intervals [s1, e1] and [s2, e2] overlap if s1 < e2 AND s2 < e1.
fn intervals_overlap(a: &ExecutionRecord, b: &ExecutionRecord) -> bool {
    a.start < b.end && b.start < a.end
}

/// Strategy for generating a sequence of concurrency classifications.
/// Produces vectors of 2..=8 elements, each being Safe or Exclusive.
fn arb_classification_sequence() -> impl Strategy<Value = Vec<Concurrency>> {
    prop::collection::vec(
        prop_oneof![Just(Concurrency::Safe), Just(Concurrency::Exclusive)],
        2..=8,
    )
}

proptest! {
    /// **Validates: Requirements 10.2, 10.3, 10.8**
    ///
    /// Property 4: Concurrency classification enforcement
    ///
    /// For any sequence of Safe/Exclusive tool classifications:
    /// - Exclusive tools must have ZERO overlap with any other tool's execution interval
    /// - Safe tools MAY overlap with other Safe tools (concurrent execution is allowed)
    ///
    /// This property ensures the StreamingToolExecutor correctly enforces that:
    /// - Safe tools can run in parallel (Req 10.2)
    /// - Exclusive tools wait for all executing tools before starting (Req 10.3)
    /// - No other tools start while an Exclusive tool is executing (Req 10.8)
    #[test]
    fn prop_exclusive_tools_never_overlap_with_any_other_tool(
        classifications in arb_classification_sequence()
    ) {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let records: Arc<Mutex<Vec<ExecutionRecord>>> = Arc::new(Mutex::new(Vec::new()));
            let mut executor = StreamingToolExecutor::new(8);

            // Enqueue tools with the generated classifications.
            // Use 10ms delay so timing is measurable but tests stay fast.
            for (i, &classification) in classifications.iter().enumerate() {
                let tool: Arc<dyn Tool> = Arc::new(TimingTool {
                    tool_name: format!("tool_{}", i),
                    classification,
                    delay: Duration::from_millis(10),
                    records: records.clone(),
                    index: i,
                });

                let tool_use = agent_core::ToolUseBlock {
                    id: format!("tu_{}", i),
                    name: format!("tool_{}", i),
                    input: json!({}),
                };

                executor.enqueue(tool_use, tool, make_ctx());
            }

            executor.execute_all().await;

            // Verify all tools completed
            let recorded = records.lock().unwrap();
            prop_assert_eq!(
                recorded.len(),
                classifications.len(),
                "All tools should have recorded their execution"
            );

            // Key property: for any Exclusive tool, no other tool's interval overlaps
            for exclusive_record in recorded.iter().filter(|r| r.concurrency == Concurrency::Exclusive) {
                for other_record in recorded.iter() {
                    if other_record.index == exclusive_record.index {
                        continue; // Skip self
                    }

                    prop_assert!(
                        !intervals_overlap(exclusive_record, other_record),
                        "Exclusive tool {} (start={:?}, end={:?}) overlaps with tool {} (start={:?}, end={:?}, concurrency={:?})",
                        exclusive_record.index,
                        exclusive_record.start,
                        exclusive_record.end,
                        other_record.index,
                        other_record.start,
                        other_record.end,
                        other_record.concurrency
                    );
                }
            }

            Ok(())
        })?;
    }

    /// **Validates: Requirements 10.2**
    ///
    /// Property 4 (supplement): Safe tools can run concurrently
    ///
    /// When multiple Safe tools are enqueued, at least some should have overlapping
    /// execution intervals, demonstrating parallel execution.
    #[test]
    fn prop_safe_tools_can_run_concurrently(
        count in 3usize..=6
    ) {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let records: Arc<Mutex<Vec<ExecutionRecord>>> = Arc::new(Mutex::new(Vec::new()));
            let mut executor = StreamingToolExecutor::new(8);

            // Enqueue only Safe tools with a 15ms delay
            for i in 0..count {
                let tool: Arc<dyn Tool> = Arc::new(TimingTool {
                    tool_name: format!("safe_tool_{}", i),
                    classification: Concurrency::Safe,
                    delay: Duration::from_millis(15),
                    records: records.clone(),
                    index: i,
                });

                let tool_use = agent_core::ToolUseBlock {
                    id: format!("tu_{}", i),
                    name: format!("safe_tool_{}", i),
                    input: json!({}),
                };

                executor.enqueue(tool_use, tool, make_ctx());
            }

            executor.execute_all().await;

            let recorded = records.lock().unwrap();
            prop_assert_eq!(recorded.len(), count);

            // Check that at least one pair of Safe tools has overlapping intervals.
            // With max_concurrency=8 and multiple tools with 15ms delay, they should
            // all start nearly simultaneously, so there should be significant overlap.
            let mut found_overlap = false;
            for i in 0..recorded.len() {
                for j in (i + 1)..recorded.len() {
                    if intervals_overlap(&recorded[i], &recorded[j]) {
                        found_overlap = true;
                        break;
                    }
                }
                if found_overlap {
                    break;
                }
            }

            prop_assert!(
                found_overlap,
                "With {} Safe tools and max_concurrency=8, at least some should run concurrently",
                count
            );

            Ok(())
        })?;
    }
}


// --- Property 5: Tool result ordering preservation ---

/// A tool with a configurable delay for testing ordering preservation.
struct DelayedTool {
    tool_name: String,
    delay_ms: u64,
}

#[async_trait]
impl Tool for DelayedTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        "A tool with configurable delay for ordering property tests"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
        Concurrency::Safe
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        Ok(ToolOutput::Text(format!("done:{}", self.tool_name)))
    }
}

/// Strategy: generate a Vec of delay values (2-8 tools, each with 0-50ms delay).
fn arb_tool_delays() -> impl Strategy<Value = Vec<u64>> {
    prop::collection::vec(0u64..=50, 2..=8)
}

proptest! {
    /// **Validates: Requirements 10.4**
    ///
    /// Property 5: Tool result ordering preservation
    ///
    /// For any set of tools enqueued with random delays (causing varying
    /// completion order), drain_completed() MUST return results in the
    /// original enqueue order (t0, t1, t2, ...) regardless of which tools
    /// complete first.
    #[test]
    fn prop_drain_completed_preserves_enqueue_order(delays in arb_tool_delays()) {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let tool_count = delays.len();
            let mut executor = StreamingToolExecutor::new(8);

            // Enqueue tools in order t0, t1, t2, ... with varying delays
            for (i, &delay_ms) in delays.iter().enumerate() {
                let tool: Arc<dyn Tool> = Arc::new(DelayedTool {
                    tool_name: format!("tool_{}", i),
                    delay_ms,
                });
                let tool_use = agent_core::ToolUseBlock {
                    id: format!("t{}", i),
                    name: format!("tool_{}", i),
                    input: json!({}),
                };
                executor.enqueue(tool_use, tool, make_ctx());
            }

            // Execute all tools concurrently
            executor.execute_all().await;

            // Drain results
            let results = executor.drain_completed();

            // Assert: correct count
            prop_assert_eq!(
                results.len(),
                tool_count,
                "Expected {} results, got {}",
                tool_count,
                results.len()
            );

            // Assert: results are in enqueue order (t0, t1, t2, ...)
            for (i, result) in results.iter().enumerate() {
                let expected_id = format!("t{}", i);
                prop_assert_eq!(
                    &result.tool_use_id,
                    &expected_id,
                    "Result at index {} has tool_use_id '{}', expected '{}'",
                    i,
                    result.tool_use_id,
                    expected_id
                );

                let expected_name = format!("tool_{}", i);
                prop_assert_eq!(
                    &result.tool_name,
                    &expected_name,
                    "Result at index {} has tool_name '{}', expected '{}'",
                    i,
                    result.tool_name,
                    expected_name
                );

                // All tools should succeed
                prop_assert!(
                    result.result.is_ok(),
                    "Result at index {} should be Ok, got: {:?}",
                    i,
                    result.result
                );
            }

            Ok(())
        })?;
    }

    /// **Validates: Requirements 10.4**
    ///
    /// Property 5 (variant): with max_concurrency=1 (sequential execution),
    /// ordering must still be preserved. This confirms the property holds even
    /// when tools cannot overlap.
    #[test]
    fn prop_drain_completed_preserves_order_sequential(delays in arb_tool_delays()) {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let tool_count = delays.len();
            // max_concurrency = 1 forces sequential execution
            let mut executor = StreamingToolExecutor::new(1);

            for (i, &delay_ms) in delays.iter().enumerate() {
                let tool: Arc<dyn Tool> = Arc::new(DelayedTool {
                    tool_name: format!("tool_{}", i),
                    delay_ms,
                });
                let tool_use = agent_core::ToolUseBlock {
                    id: format!("t{}", i),
                    name: format!("tool_{}", i),
                    input: json!({}),
                };
                executor.enqueue(tool_use, tool, make_ctx());
            }

            executor.execute_all().await;
            let results = executor.drain_completed();

            prop_assert_eq!(results.len(), tool_count);

            for (i, result) in results.iter().enumerate() {
                let expected_id = format!("t{}", i);
                prop_assert_eq!(
                    &result.tool_use_id,
                    &expected_id,
                    "Sequential: result at index {} has tool_use_id '{}', expected '{}'",
                    i,
                    result.tool_use_id,
                    expected_id
                );
            }

            Ok(())
        })?;
    }

    /// **Validates: Requirements 10.4**
    ///
    /// Property 5 (variant): with max_concurrency=2, test that limited parallelism
    /// still preserves ordering. This is a middle ground between fully parallel and
    /// sequential execution.
    #[test]
    fn prop_drain_completed_preserves_order_limited_concurrency(delays in arb_tool_delays()) {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let tool_count = delays.len();
            let mut executor = StreamingToolExecutor::new(2);

            for (i, &delay_ms) in delays.iter().enumerate() {
                let tool: Arc<dyn Tool> = Arc::new(DelayedTool {
                    tool_name: format!("tool_{}", i),
                    delay_ms,
                });
                let tool_use = agent_core::ToolUseBlock {
                    id: format!("t{}", i),
                    name: format!("tool_{}", i),
                    input: json!({}),
                };
                executor.enqueue(tool_use, tool, make_ctx());
            }

            executor.execute_all().await;
            let results = executor.drain_completed();

            prop_assert_eq!(results.len(), tool_count);

            for (i, result) in results.iter().enumerate() {
                let expected_id = format!("t{}", i);
                prop_assert_eq!(
                    &result.tool_use_id,
                    &expected_id,
                    "Limited concurrency: result at index {} has tool_use_id '{}', expected '{}'",
                    i,
                    result.tool_use_id,
                    expected_id
                );
            }

            Ok(())
        })?;
    }
}
