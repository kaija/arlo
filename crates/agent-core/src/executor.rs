//! StreamingToolExecutor: concurrent tool execution during model streaming.
//!
//! Tools start executing during model streaming, before the stream completes.
//! The executor respects concurrency classifications (Safe vs Exclusive) and
//! supports error cascading via cancellation tokens.

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::ToolError;
use crate::message::ToolUseBlock;
use crate::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// The result of a single tool execution, including its identity.
#[derive(Debug)]
pub struct ToolResult {
    /// The tool_use_id from the original ToolUseBlock.
    pub tool_use_id: String,
    /// The tool name.
    pub tool_name: String,
    /// The result of execution.
    pub result: Result<ToolOutput, ToolError>,
}

/// Internal representation of an enqueued tool awaiting execution.
struct EnqueuedTool {
    index: usize,
    tool_use: ToolUseBlock,
    tool: Arc<dyn Tool>,
    ctx: ToolContext,
    concurrency: Concurrency,
}

/// Indexed result from a completed tool execution.
struct IndexedResult {
    index: usize,
    tool_use_id: String,
    tool_name: String,
    result: Result<ToolOutput, ToolError>,
}

/// Concurrent tool executor that starts tools during model streaming.
///
/// Respects concurrency classification:
/// - `Safe` tools run in parallel up to `max_concurrency`
/// - `Exclusive` tools wait for all executing tools, then run alone
///
/// Results are returned in enqueue order via `drain_completed()`.
pub struct StreamingToolExecutor {
    queue: Vec<EnqueuedTool>,
    completed: Vec<IndexedResult>,
    pending: Vec<JoinHandle<IndexedResult>>,
    max_concurrency: usize,
    cancel_token: CancellationToken,
}

impl StreamingToolExecutor {
    /// Create a new executor with the given max concurrency.
    ///
    /// `max_concurrency` is clamped to a minimum of 1. Default is 8.
    pub fn new(max_concurrency: usize) -> Self {
        Self {
            queue: Vec::new(),
            completed: Vec::new(),
            pending: Vec::new(),
            max_concurrency: max_concurrency.max(1),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Enqueue a tool for execution.
    ///
    /// The tool's concurrency classification is determined from the input.
    pub fn enqueue(
        &mut self,
        tool_use: ToolUseBlock,
        tool: Arc<dyn Tool>,
        ctx: ToolContext,
    ) {
        let concurrency = tool.concurrency(&tool_use.input);
        let index = self.queue.len();
        self.queue.push(EnqueuedTool {
            index,
            tool_use,
            tool,
            ctx,
            concurrency,
        });
    }

    /// Execute all enqueued tools respecting concurrency rules.
    ///
    /// - Safe tools run in parallel up to `max_concurrency`
    /// - Exclusive tools wait for all pending tools, then run alone
    /// - If a tool with `error_cascades()` fails, remaining tools are cancelled
    ///
    /// After this method returns, all results are available via `drain_completed()`.
    pub async fn execute_all(&mut self) {
        let queue = std::mem::take(&mut self.queue);
        let semaphore = Arc::new(Semaphore::new(self.max_concurrency));
        let mut safe_batch: Vec<EnqueuedTool> = Vec::new();

        for enqueued in queue {
            match enqueued.concurrency {
                Concurrency::Safe => {
                    safe_batch.push(enqueued);
                }
                Concurrency::Exclusive => {
                    // Flush all pending safe tools first
                    self.flush_safe_batch(&mut safe_batch, &semaphore)
                        .await;
                    self.join_all_pending().await;

                    // Check cancellation before running exclusive tool
                    if self.cancel_token.is_cancelled() {
                        self.completed.push(IndexedResult {
                            index: enqueued.index,
                            tool_use_id: enqueued.tool_use.id.clone(),
                            tool_name: enqueued.tool_use.name.clone(),
                            result: Err(ToolError::ExecutionFailed(
                                "Cancelled due to sibling error".to_string(),
                            )),
                        });
                        continue;
                    }

                    // Run exclusive tool alone
                    self.run_exclusive(enqueued).await;
                }
            }
        }

        // Flush remaining safe tools
        self.flush_safe_batch(&mut safe_batch, &semaphore).await;
        self.join_all_pending().await;
    }

    /// Drain completed results in enqueue order.
    ///
    /// Returns all completed tool results sorted by their original enqueue index.
    pub fn drain_completed(&mut self) -> Vec<ToolResult> {
        self.completed.sort_by_key(|r| r.index);
        self.completed
            .drain(..)
            .map(|r| ToolResult {
                tool_use_id: r.tool_use_id,
                tool_name: r.tool_name,
                result: r.result,
            })
            .collect()
    }

    /// Spawn safe tools from the batch up to max_concurrency.
    async fn flush_safe_batch(
        &mut self,
        batch: &mut Vec<EnqueuedTool>,
        semaphore: &Arc<Semaphore>,
    ) {
        for enqueued in batch.drain(..) {
            if self.cancel_token.is_cancelled() {
                self.completed.push(IndexedResult {
                    index: enqueued.index,
                    tool_use_id: enqueued.tool_use.id.clone(),
                    tool_name: enqueued.tool_use.name.clone(),
                    result: Err(ToolError::ExecutionFailed(
                        "Cancelled due to sibling error".to_string(),
                    )),
                });
                continue;
            }

            let sem = Arc::clone(semaphore);
            let cancel = self.cancel_token.clone();
            let tool = enqueued.tool;
            let input = enqueued.tool_use.input.clone();
            let ctx = enqueued.ctx;
            let index = enqueued.index;
            let tool_use_id = enqueued.tool_use.id.clone();
            let tool_name = enqueued.tool_use.name.clone();
            let error_cascades = tool.error_cascades();
            let tool_timeout = tool.timeout();

            let handle = tokio::spawn(async move {
                // Acquire semaphore permit for concurrency limiting
                let _permit = sem.acquire().await.unwrap();

                // Check cancellation before executing
                if cancel.is_cancelled() {
                    return IndexedResult {
                        index,
                        tool_use_id,
                        tool_name,
                        result: Err(ToolError::ExecutionFailed(
                            "Cancelled due to sibling error".to_string(),
                        )),
                    };
                }

                let result = {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            Err(ToolError::ExecutionFailed(
                                "Cancelled due to sibling error".to_string(),
                            ))
                        }
                        res = tokio::time::timeout(tool_timeout, tool.execute(input, &ctx)) => {
                            match res {
                                Ok(inner) => inner,
                                Err(_elapsed) => Err(ToolError::Timeout),
                            }
                        }
                    }
                };

                // If this tool cascades errors and it failed, cancel siblings
                if error_cascades {
                    if let Err(_) = &result {
                        tracing::error!(tool_name = %tool_name, "tool_error_cascading");
                        cancel.cancel();
                    }
                }

                if result.is_err() {
                    tracing::error!(tool_name = %tool_name, "tool_execution_failed");
                }

                IndexedResult {
                    index,
                    tool_use_id,
                    tool_name,
                    result,
                }
            });

            self.pending.push(handle);
        }
    }

    /// Run an exclusive tool alone (no other tools executing).
    async fn run_exclusive(&mut self, enqueued: EnqueuedTool) {
        let tool = enqueued.tool;
        let input = enqueued.tool_use.input.clone();
        let ctx = enqueued.ctx;
        let error_cascades = tool.error_cascades();
        let tool_name_str = enqueued.tool_use.name.clone();
        let tool_timeout = tool.timeout();

        let result = {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    Err(ToolError::ExecutionFailed(
                        "Cancelled due to sibling error".to_string(),
                    ))
                }
                res = tokio::time::timeout(tool_timeout, tool.execute(input, &ctx)) => {
                    match res {
                        Ok(inner) => inner,
                        Err(_elapsed) => Err(ToolError::Timeout),
                    }
                }
            }
        };

        if error_cascades {
            if let Err(_) = &result {
                tracing::error!(tool_name = %tool_name_str, "tool_error_cascading");
                self.cancel_token.cancel();
            }
        }

        if result.is_err() {
            tracing::error!(tool_name = %tool_name_str, "tool_execution_failed");
        }

        self.completed.push(IndexedResult {
            index: enqueued.index,
            tool_use_id: enqueued.tool_use.id.clone(),
            tool_name: enqueued.tool_use.name.clone(),
            result,
        });
    }

    /// Join all pending tasks and collect results.
    async fn join_all_pending(&mut self) {
        let handles: Vec<_> = self.pending.drain(..).collect();
        for handle in handles {
            match handle.await {
                Ok(indexed) => self.completed.push(indexed),
                Err(_join_err) => {
                    // Task panicked; record as error
                    self.completed.push(IndexedResult {
                        index: usize::MAX,
                        tool_use_id: String::new(),
                        tool_name: String::new(),
                        result: Err(ToolError::ExecutionFailed(
                            "Task panicked".to_string(),
                        )),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Concurrency, Tool, ToolContext, ToolOutput};
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn make_ctx() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            working_dir: PathBuf::from("/tmp/test"),
        }
    }

    fn make_tool_use(id: &str, name: &str) -> ToolUseBlock {
        ToolUseBlock {
            id: id.to_string(),
            name: name.to_string(),
            input: json!({}),
        }
    }

    /// A simple safe tool that completes instantly.
    struct SafeTool {
        tool_name: String,
        output: String,
    }

    #[async_trait]
    impl Tool for SafeTool {
        fn name(&self) -> &str { &self.tool_name }
        fn description(&self) -> &str { "safe tool" }
        fn parameters_schema(&self) -> serde_json::Value { json!({"type": "object"}) }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Text(self.output.clone()))
        }
    }

    /// An exclusive tool.
    struct ExclusiveTool {
        tool_name: String,
        output: String,
    }

    #[async_trait]
    impl Tool for ExclusiveTool {
        fn name(&self) -> &str { &self.tool_name }
        fn description(&self) -> &str { "exclusive tool" }
        fn parameters_schema(&self) -> serde_json::Value { json!({"type": "object"}) }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Exclusive
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Text(self.output.clone()))
        }
    }

    /// A tool that fails and cascades errors.
    struct CascadingFailTool;

    #[async_trait]
    impl Tool for CascadingFailTool {
        fn name(&self) -> &str { "cascading_fail" }
        fn description(&self) -> &str { "fails and cascades" }
        fn parameters_schema(&self) -> serde_json::Value { json!({"type": "object"}) }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }
        fn error_cascades(&self) -> bool { true }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Err(ToolError::ExecutionFailed("intentional failure".to_string()))
        }
    }

    /// A slow tool that takes some time to execute (for concurrency testing).
    struct SlowTool {
        tool_name: String,
        delay_ms: u64,
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for SlowTool {
        fn name(&self) -> &str { &self.tool_name }
        fn description(&self) -> &str { "slow tool" }
        fn parameters_schema(&self) -> serde_json::Value { json!({"type": "object"}) }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            self.counter.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput::Text(format!("done:{}", self.tool_name)))
        }
    }

    /// A tool that sleeps and then fails (for cascading tests with delays).
    #[allow(dead_code)]
    struct SlowCascadingFailTool {
        delay_ms: u64,
    }

    #[async_trait]
    impl Tool for SlowCascadingFailTool {
        fn name(&self) -> &str { "slow_cascade_fail" }
        fn description(&self) -> &str { "slow fail cascading" }
        fn parameters_schema(&self) -> serde_json::Value { json!({"type": "object"}) }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }
        fn error_cascades(&self) -> bool { true }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            Err(ToolError::ExecutionFailed("delayed failure".to_string()))
        }
    }

    #[tokio::test]
    async fn test_new_clamps_min_concurrency() {
        let exec = StreamingToolExecutor::new(0);
        assert_eq!(exec.max_concurrency, 1);

        let exec = StreamingToolExecutor::new(1);
        assert_eq!(exec.max_concurrency, 1);

        let exec = StreamingToolExecutor::new(16);
        assert_eq!(exec.max_concurrency, 16);
    }

    #[tokio::test]
    async fn test_single_safe_tool() {
        let mut exec = StreamingToolExecutor::new(8);
        let tool: Arc<dyn Tool> = Arc::new(SafeTool {
            tool_name: "echo".to_string(),
            output: "hello".to_string(),
        });

        exec.enqueue(make_tool_use("t1", "echo"), tool, make_ctx());
        exec.execute_all().await;

        let results = exec.drain_completed();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_use_id, "t1");
        assert_eq!(results[0].tool_name, "echo");
        assert!(matches!(&results[0].result, Ok(ToolOutput::Text(s)) if s == "hello"));
    }

    #[tokio::test]
    async fn test_multiple_safe_tools_parallel() {
        let mut exec = StreamingToolExecutor::new(8);

        for i in 0..4 {
            let tool: Arc<dyn Tool> = Arc::new(SafeTool {
                tool_name: format!("tool{}", i),
                output: format!("out{}", i),
            });
            exec.enqueue(
                make_tool_use(&format!("t{}", i), &format!("tool{}", i)),
                tool,
                make_ctx(),
            );
        }

        exec.execute_all().await;
        let results = exec.drain_completed();
        assert_eq!(results.len(), 4);

        // Results should be in enqueue order
        for i in 0..4 {
            assert_eq!(results[i].tool_use_id, format!("t{}", i));
            assert_eq!(results[i].tool_name, format!("tool{}", i));
        }
    }

    #[tokio::test]
    async fn test_exclusive_tool_runs_alone() {
        let mut exec = StreamingToolExecutor::new(8);

        let tool1: Arc<dyn Tool> = Arc::new(SafeTool {
            tool_name: "safe1".to_string(),
            output: "s1".to_string(),
        });
        let tool2: Arc<dyn Tool> = Arc::new(ExclusiveTool {
            tool_name: "excl".to_string(),
            output: "e1".to_string(),
        });
        let tool3: Arc<dyn Tool> = Arc::new(SafeTool {
            tool_name: "safe2".to_string(),
            output: "s2".to_string(),
        });

        exec.enqueue(make_tool_use("t1", "safe1"), tool1, make_ctx());
        exec.enqueue(make_tool_use("t2", "excl"), tool2, make_ctx());
        exec.enqueue(make_tool_use("t3", "safe2"), tool3, make_ctx());

        exec.execute_all().await;
        let results = exec.drain_completed();
        assert_eq!(results.len(), 3);

        // Results in enqueue order
        assert_eq!(results[0].tool_use_id, "t1");
        assert_eq!(results[1].tool_use_id, "t2");
        assert_eq!(results[2].tool_use_id, "t3");

        assert!(results[0].result.is_ok());
        assert!(results[1].result.is_ok());
        assert!(results[2].result.is_ok());
    }

    #[tokio::test]
    async fn test_results_in_enqueue_order_despite_varying_durations() {
        let mut exec = StreamingToolExecutor::new(8);

        // Tool 0 is slow, tool 1 is fast - but results should still be in order
        let counter = Arc::new(AtomicUsize::new(0));
        let tool0: Arc<dyn Tool> = Arc::new(SlowTool {
            tool_name: "slow".to_string(),
            delay_ms: 50,
            counter: counter.clone(),
        });
        let tool1: Arc<dyn Tool> = Arc::new(SafeTool {
            tool_name: "fast".to_string(),
            output: "fast_result".to_string(),
        });

        exec.enqueue(make_tool_use("t0", "slow"), tool0, make_ctx());
        exec.enqueue(make_tool_use("t1", "fast"), tool1, make_ctx());

        exec.execute_all().await;
        let results = exec.drain_completed();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool_use_id, "t0");
        assert_eq!(results[1].tool_use_id, "t1");
    }

    #[tokio::test]
    async fn test_error_cascading_cancels_siblings() {
        let mut exec = StreamingToolExecutor::new(8);

        // A cascading fail tool that fails quickly
        let fail_tool: Arc<dyn Tool> = Arc::new(CascadingFailTool);
        // A slow tool that should get cancelled
        let counter = Arc::new(AtomicUsize::new(0));
        let slow_tool: Arc<dyn Tool> = Arc::new(SlowTool {
            tool_name: "slow_sibling".to_string(),
            delay_ms: 5000, // Very slow - should be cancelled
            counter: counter.clone(),
        });

        exec.enqueue(make_tool_use("t_fail", "cascading_fail"), fail_tool, make_ctx());
        exec.enqueue(
            make_tool_use("t_slow", "slow_sibling"),
            slow_tool,
            make_ctx(),
        );

        exec.execute_all().await;
        let results = exec.drain_completed();
        assert_eq!(results.len(), 2);

        // First tool should have the intentional failure
        assert_eq!(results[0].tool_use_id, "t_fail");
        assert!(results[0].result.is_err());

        // Second tool should have been cancelled
        assert_eq!(results[1].tool_use_id, "t_slow");
        assert!(results[1].result.is_err());
    }

    #[tokio::test]
    async fn test_max_concurrency_respected() {
        // With max_concurrency=2, at most 2 tools should run simultaneously
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let peak_concurrent = Arc::new(AtomicUsize::new(0));

        let mut exec = StreamingToolExecutor::new(2);

        for i in 0..4 {
            let mc = max_concurrent.clone();
            let pc = peak_concurrent.clone();
            let tool: Arc<dyn Tool> = Arc::new(ConcurrencyTrackingTool {
                tool_name: format!("t{}", i),
                delay_ms: 30,
                current_concurrent: mc,
                peak_concurrent: pc,
            });
            exec.enqueue(
                make_tool_use(&format!("id{}", i), &format!("t{}", i)),
                tool,
                make_ctx(),
            );
        }

        exec.execute_all().await;
        let results = exec.drain_completed();
        assert_eq!(results.len(), 4);

        // Peak concurrent should not exceed 2
        assert!(peak_concurrent.load(Ordering::SeqCst) <= 2);
    }

    /// Tool that tracks concurrency for testing max_concurrency limits.
    struct ConcurrencyTrackingTool {
        tool_name: String,
        delay_ms: u64,
        current_concurrent: Arc<AtomicUsize>,
        peak_concurrent: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for ConcurrencyTrackingTool {
        fn name(&self) -> &str { &self.tool_name }
        fn description(&self) -> &str { "tracks concurrency" }
        fn parameters_schema(&self) -> serde_json::Value { json!({"type": "object"}) }
        fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
            Concurrency::Safe
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            let current = self.current_concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            // Update peak
            self.peak_concurrent.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            self.current_concurrent.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput::Text(format!("done:{}", self.tool_name)))
        }
    }

    #[tokio::test]
    async fn test_empty_queue_execute_all() {
        let mut exec = StreamingToolExecutor::new(8);
        exec.execute_all().await;
        let results = exec.drain_completed();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_exclusive_between_safe_batches() {
        // S S E S S - exclusive should run between safe batches
        let mut exec = StreamingToolExecutor::new(8);

        let tools: Vec<(&str, &str, bool)> = vec![
            ("t0", "safe0", false),
            ("t1", "safe1", false),
            ("t2", "exclusive", true),
            ("t3", "safe2", false),
            ("t4", "safe3", false),
        ];

        for (id, name, is_exclusive) in tools {
            let tool: Arc<dyn Tool> = if is_exclusive {
                Arc::new(ExclusiveTool {
                    tool_name: name.to_string(),
                    output: format!("out_{}", name),
                })
            } else {
                Arc::new(SafeTool {
                    tool_name: name.to_string(),
                    output: format!("out_{}", name),
                })
            };
            exec.enqueue(make_tool_use(id, name), tool, make_ctx());
        }

        exec.execute_all().await;
        let results = exec.drain_completed();
        assert_eq!(results.len(), 5);

        // All in enqueue order
        assert_eq!(results[0].tool_use_id, "t0");
        assert_eq!(results[1].tool_use_id, "t1");
        assert_eq!(results[2].tool_use_id, "t2");
        assert_eq!(results[3].tool_use_id, "t3");
        assert_eq!(results[4].tool_use_id, "t4");

        // All succeeded
        for r in &results {
            assert!(r.result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_non_cascading_error_does_not_cancel() {
        let mut exec = StreamingToolExecutor::new(8);

        /// A tool that fails but does NOT cascade.
        struct NonCascadingFailTool;

        #[async_trait]
        impl Tool for NonCascadingFailTool {
            fn name(&self) -> &str { "non_cascade_fail" }
            fn description(&self) -> &str { "fails without cascading" }
            fn parameters_schema(&self) -> serde_json::Value { json!({"type": "object"}) }
            fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
                Concurrency::Safe
            }
            fn error_cascades(&self) -> bool { false }
            async fn execute(
                &self,
                _input: serde_json::Value,
                _ctx: &ToolContext,
            ) -> Result<ToolOutput, ToolError> {
                Err(ToolError::ExecutionFailed("non-cascading fail".to_string()))
            }
        }

        let fail_tool: Arc<dyn Tool> = Arc::new(NonCascadingFailTool);
        let safe_tool: Arc<dyn Tool> = Arc::new(SafeTool {
            tool_name: "after_fail".to_string(),
            output: "still_works".to_string(),
        });

        exec.enqueue(make_tool_use("t1", "non_cascade_fail"), fail_tool, make_ctx());
        exec.enqueue(make_tool_use("t2", "after_fail"), safe_tool, make_ctx());

        exec.execute_all().await;
        let results = exec.drain_completed();
        assert_eq!(results.len(), 2);

        // First tool failed
        assert!(results[0].result.is_err());
        // Second tool still succeeded
        assert!(results[1].result.is_ok());
    }
}
