//! Integration tests for the drive() run loop.
//!
//! These tests exercise the full loop end-to-end: model mock → tool execution →
//! permission check → apply_transition() → RunResult / RunEvent stream.
//! They cover the scenarios that unit tests on `apply_transition` and
//! `resolve_next_step` cannot reach in isolation:
//!
//! - HITL approval prompt fires for a tool with ApprovalRequirement::Always
//! - Approval handler Allow / Deny / AlwaysAllow responses continue the loop correctly
//! - PermissionMode::Bypass skips the approval prompt entirely
//! - A static-allow pattern in settings skips the approval prompt
//! - Session grant (AlwaysAllow from a prior turn) skips subsequent prompts
//! - web_fetch (newly fixed) triggers HITL in Normal mode
//! - Loop runs to completion through a tool call and produces correct turn count

use std::sync::{Arc, Mutex};

use agent_core::{
    run, run_stream, Agent, ApprovalContext, ApprovalHandler, ApprovalResponse, Input,
    Instructions, ModelError, ModelProvider, ModelRequest, ModelResponse, ModelStream,
    PermissionEngine, PermissionMode, RunConfig, RunEvent, StopReason, StreamChunk, ToolContext,
    ToolOutput, Usage,
};
use agent_core::{ApprovalRequirement, Concurrency, Message, Model, Tool, ToolError};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;

// ── Shared mock infrastructure ────────────────────────────────────────────────

/// Text-only model: always returns a single text response.
struct TextModel {
    text: &'static str,
}

#[async_trait]
impl Model for TextModel {
    async fn stream(&self, _req: ModelRequest) -> Result<ModelStream, ModelError> {
        let chunks = vec![
            Ok(StreamChunk::TextDelta {
                text: self.text.to_string(),
            }),
            Ok(StreamChunk::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_tokens: None,
                },
            }),
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }
    async fn complete(&self, _: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }
    fn name(&self) -> &str {
        "text-model"
    }
    fn provider(&self) -> &str {
        "mock"
    }
    fn context_window(&self) -> usize {
        128_000
    }
    fn max_output_tokens(&self) -> usize {
        4096
    }
    fn supports_tools(&self) -> bool {
        false
    }
    fn input_cost_per_million(&self) -> f64 {
        3.0
    }
    fn output_cost_per_million(&self) -> f64 {
        15.0
    }
}

/// Model that calls a named tool once then returns final text.
/// On the second call (when tool results are present) it returns text.
struct ToolThenTextModel {
    tool_name: &'static str,
    tool_id: &'static str,
    final_text: &'static str,
}

#[async_trait]
impl Model for ToolThenTextModel {
    async fn stream(&self, req: ModelRequest) -> Result<ModelStream, ModelError> {
        let has_results = req
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult { .. }));
        if has_results {
            let chunks = vec![
                Ok(StreamChunk::TextDelta {
                    text: self.final_text.to_string(),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 20,
                        output_tokens: 10,
                        cache_read_tokens: None,
                    },
                }),
            ];
            return Ok(Box::pin(futures::stream::iter(chunks)));
        }
        let chunks = vec![
            Ok(StreamChunk::ToolUseStart {
                id: self.tool_id.to_string(),
                name: self.tool_name.to_string(),
            }),
            Ok(StreamChunk::ToolUseInputDelta {
                id: self.tool_id.to_string(),
                delta: "{}".to_string(),
            }),
            Ok(StreamChunk::ToolUseEnd {
                id: self.tool_id.to_string(),
                input: json!({}),
            }),
            Ok(StreamChunk::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 15,
                    output_tokens: 8,
                    cache_read_tokens: None,
                },
            }),
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }
    async fn complete(&self, _: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }
    fn name(&self) -> &str {
        "tool-then-text"
    }
    fn provider(&self) -> &str {
        "mock"
    }
    fn context_window(&self) -> usize {
        128_000
    }
    fn max_output_tokens(&self) -> usize {
        4096
    }
    fn supports_tools(&self) -> bool {
        true
    }
    fn input_cost_per_million(&self) -> f64 {
        3.0
    }
    fn output_cost_per_million(&self) -> f64 {
        15.0
    }
}

fn provider(model: impl Model + 'static) -> Arc<dyn ModelProvider> {
    struct P(Arc<dyn Model>);
    #[async_trait]
    impl ModelProvider for P {
        async fn resolve(&self, _: &str) -> Result<Arc<dyn Model>, ModelError> {
            Ok(self.0.clone())
        }
        fn available_models(&self) -> Vec<String> {
            vec![]
        }
    }
    Arc::new(P(Arc::new(model)))
}

/// A no-op tool that requires approval.
struct NeedsApprovalTool;

#[async_trait]
impl Tool for NeedsApprovalTool {
    fn name(&self) -> &str {
        "needs_approval"
    }
    fn description(&self) -> &str {
        "requires approval"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    fn concurrency(&self, _: &serde_json::Value) -> Concurrency {
        Concurrency::Safe
    }
    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Always
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Text("executed".to_string()))
    }
}

/// An approval handler that returns a fixed response for every pending item.
struct FixedApprovalHandler {
    response: ApprovalResponse,
}

#[async_trait]
impl ApprovalHandler for FixedApprovalHandler {
    async fn request_approval(&self, ctx: &ApprovalContext) -> Vec<ApprovalResponse> {
        ctx.pending.iter().map(|_| self.response.clone()).collect()
    }
}

/// An approval handler that records each call and returns Allow.
#[derive(Clone)]
struct RecordingApprovalHandler {
    calls: Arc<Mutex<Vec<ApprovalContext>>>,
}

impl RecordingApprovalHandler {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(vec![])),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait]
impl ApprovalHandler for RecordingApprovalHandler {
    async fn request_approval(&self, ctx: &ApprovalContext) -> Vec<ApprovalResponse> {
        self.calls.lock().unwrap().push(ctx.clone());
        ctx.pending
            .iter()
            .map(|_| ApprovalResponse::Allow)
            .collect()
    }
}

// ── Basic loop tests ──────────────────────────────────────────────────────────

/// Simplest possible run: one text response, no tools.
#[tokio::test]
async fn loop_simple_text_response() {
    let agent = Agent::builder("a")
        .instructions(Instructions::Static("hi".into()))
        .build();
    let config = RunConfig::builder(provider(TextModel { text: "the answer" }), "mock").build();
    let result = run(&agent, Input::Fresh { prompt: "q".into() }, &config)
        .await
        .unwrap();
    assert_eq!(result.output, "the answer");
    assert_eq!(result.turns, 1);
}

/// Tool call followed by final text: turn count should be 2.
#[tokio::test]
async fn loop_tool_call_increments_turns() {
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    // Bypass so approval is skipped; we just want to count turns.
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "done",
        }),
        "mock",
    )
    .build(); // default Bypass mode
    let result = run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    )
    .await
    .unwrap();
    assert_eq!(result.output, "done");
    assert_eq!(result.turns, 2);
}

/// Agent-level max_turns cap returns successfully without error.
#[tokio::test]
async fn loop_max_turns_terminates_cleanly() {
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .max_turns(1)
        .build();
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "done",
        }),
        "mock",
    )
    .build();
    // With max_turns=1 the loop should hit MaxTurns and return Ok (not Err).
    let result = run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    )
    .await
    .unwrap();
    assert_eq!(result.turns, 0);
}

// ── HITL approval tests ───────────────────────────────────────────────────────

/// Normal mode + ApprovalRequirement::Always: run is interrupted and the
/// approval handler is called.
#[tokio::test]
async fn hitl_interruption_fires_in_normal_mode() {
    let handler = RecordingApprovalHandler::new();
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "done",
        }),
        "mock",
    )
    .permissions(PermissionEngine::new(PermissionMode::Normal))
    .approval_handler(Arc::new(handler.clone()))
    .build();

    let result = run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    )
    .await
    .unwrap();

    assert_eq!(
        handler.call_count(),
        1,
        "approval handler must be called exactly once"
    );
    assert_eq!(result.output, "done");
    assert_eq!(result.turns, 2);
}

/// Approval handler returns Deny: tool result is injected as is_error, run continues.
#[tokio::test]
async fn hitl_deny_injects_error_result_and_continues() {
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "denied-response",
        }),
        "mock",
    )
    .permissions(PermissionEngine::new(PermissionMode::Normal))
    .approval_handler(Arc::new(FixedApprovalHandler {
        response: ApprovalResponse::Deny,
    }))
    .build();

    let result = run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    )
    .await
    .unwrap();

    // Run must complete (not error out), and the model must have seen a denial message.
    assert_eq!(result.output, "denied-response");
    let has_denial = result.state.messages.iter().any(|m| match m {
        agent_core::Message::ToolResult {
            content, is_error, ..
        } => *is_error && content.contains("not approved"),
        _ => false,
    });
    assert!(
        has_denial,
        "a denied tool must produce an is_error ToolResult in history"
    );
}

/// Approval handler returns AlwaysAllow: session grant is registered, and on a
/// second run using the same permissions engine the tool is auto-approved.
#[tokio::test]
async fn hitl_always_allow_grants_session_and_skips_second_prompt() {
    let handler = RecordingApprovalHandler::new();

    // Wrap in a shared session grants store so the grant persists in the engine clone.
    let shared = Arc::new(tokio::sync::RwLock::new(vec![]));
    let permissions =
        PermissionEngine::new(PermissionMode::Normal).with_shared_session_grants(shared.clone());

    // First run: handler returns AlwaysAllow
    struct AlwaysAllowHandler;
    #[async_trait]
    impl ApprovalHandler for AlwaysAllowHandler {
        async fn request_approval(&self, ctx: &ApprovalContext) -> Vec<ApprovalResponse> {
            ctx.pending
                .iter()
                .map(|_| ApprovalResponse::AlwaysAllow {
                    pattern: "needs_approval".into(),
                })
                .collect()
        }
    }

    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    let config1 = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "ok",
        }),
        "mock",
    )
    .permissions(permissions.clone())
    .approval_handler(Arc::new(AlwaysAllowHandler))
    .build();

    run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config1,
    )
    .await
    .unwrap();

    // Second run with the same shared grants + RecordingHandler
    let config2 = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t2",
            final_text: "ok2",
        }),
        "mock",
    )
    .permissions(permissions.with_shared_session_grants(shared))
    .approval_handler(Arc::new(handler.clone()))
    .build();

    run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config2,
    )
    .await
    .unwrap();

    assert_eq!(
        handler.call_count(),
        0,
        "second run should not prompt: session grant from AlwaysAllow must bypass Layer 4"
    );
}

// ── Permission layer bypass tests ─────────────────────────────────────────────

/// PermissionMode::Bypass (yolo): approval handler is never called, tool runs freely.
#[tokio::test]
async fn hitl_bypass_mode_skips_approval() {
    let handler = RecordingApprovalHandler::new();
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    // Default RunConfig uses Bypass mode.
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "done",
        }),
        "mock",
    )
    .approval_handler(Arc::new(handler.clone()))
    .build();

    let result = run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    )
    .await
    .unwrap();

    assert_eq!(
        handler.call_count(),
        0,
        "Bypass mode must never call the approval handler"
    );
    assert_eq!(result.output, "done");
}

/// Static-allow in settings (Layer 2): tool is pre-approved, handler not called.
#[tokio::test]
async fn hitl_static_allow_skips_approval() {
    let handler = RecordingApprovalHandler::new();
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    let permissions = PermissionEngine::new(PermissionMode::Normal)
        .with_static_allow(vec!["needs_approval".to_string()]);
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "done",
        }),
        "mock",
    )
    .permissions(permissions)
    .approval_handler(Arc::new(handler.clone()))
    .build();

    let result = run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    )
    .await
    .unwrap();

    assert_eq!(handler.call_count(), 0, "static_allow must bypass Layer 4");
    assert_eq!(result.output, "done");
}

/// Static-deny (Layer 2): tool is blocked, run is aborted immediately.
#[tokio::test]
async fn hitl_static_deny_aborts_run() {
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    let permissions = PermissionEngine::new(PermissionMode::Normal)
        .with_static_deny(vec!["needs_approval".to_string()]);
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "done",
        }),
        "mock",
    )
    .permissions(permissions)
    .build();

    let result = run(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    )
    .await;
    assert!(result.is_err(), "static_deny must abort the run");
}

// ── No approval handler (surface without TUI) ────────────────────────────────

/// When no approval handler is set in Normal mode, the run returns an
/// Interruption RunEvent and a RunResult (not an error) with pending_approvals set.
#[tokio::test]
async fn hitl_no_handler_returns_interruption_event_in_stream() {
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "done",
        }),
        "mock",
    )
    .permissions(PermissionEngine::new(PermissionMode::Normal))
    // no approval_handler — surfaces the Interruption RunEvent
    .build();

    let stream = run_stream(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    );
    let events: Vec<RunEvent> = stream.collect().await;

    let has_interruption = events
        .iter()
        .any(|e| matches!(e, RunEvent::Interruption { .. }));
    assert!(
        has_interruption,
        "stream must emit an Interruption event when no handler is set"
    );

    // Must be exactly one terminal event
    let terminal_count = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                RunEvent::AgentEnd { .. }
                    | RunEvent::MaxTurns { .. }
                    | RunEvent::Aborted { .. }
                    | RunEvent::Error { .. }
                    | RunEvent::Interruption { .. }
                    | RunEvent::GuardrailTripped { .. }
            )
        })
        .count();
    assert_eq!(terminal_count, 1, "exactly one terminal event per run");
}

// ── web_fetch / web_search HITL regression tests ─────────────────────────────

/// Regression: web_fetch previously returned ApprovalRequirement::Never.
/// Verify the tool now reports Always (unit-level check — no HTTP call needed).
#[test]
fn web_fetch_reports_approval_requirement_always() {
    let tool = agent_tools::WebFetchTool::new();
    assert_eq!(
        tool.approval_requirement(),
        ApprovalRequirement::Always,
        "web_fetch must require approval so HITL fires in Normal mode"
    );
}

/// Regression: web_search previously returned ApprovalRequirement::Never.
#[test]
fn web_search_reports_approval_requirement_always() {
    struct NullProvider;
    #[async_trait::async_trait]
    impl agent_tools::SearchProvider for NullProvider {
        async fn search(
            &self,
            _: &str,
            _: usize,
        ) -> Result<Vec<agent_tools::SearchResult>, agent_core::ToolError> {
            Ok(vec![])
        }
    }
    let tool = agent_tools::WebSearchTool::new(Box::new(NullProvider));
    assert_eq!(
        tool.approval_requirement(),
        ApprovalRequirement::Always,
        "web_search must require approval so HITL fires in Normal mode"
    );
}

/// Full loop: web_fetch triggers HITL in Normal mode.
/// Uses a mock model that requests web_fetch, and verifies the approval
/// handler is called (no actual HTTP request is made).
#[tokio::test]
async fn loop_web_fetch_triggers_hitl_in_normal_mode() {
    let handler = RecordingApprovalHandler::new();

    // Model that calls web_fetch once then returns text.
    let model = ToolThenTextModel {
        tool_name: "web_fetch",
        tool_id: "wf1",
        final_text: "gold price fetched",
    };

    let web_fetch: Arc<dyn Tool> = Arc::new(agent_tools::WebFetchTool::new());
    let agent = Agent::builder("gold-agent").tool(web_fetch).build();

    let config = RunConfig::builder(provider(model), "mock")
        .permissions(PermissionEngine::new(PermissionMode::Normal))
        .approval_handler(Arc::new(handler.clone()))
        .build();

    let result = run(
        &agent,
        Input::Fresh {
            prompt: "get the gold price today".into(),
        },
        &config,
    )
    .await
    .unwrap();

    assert_eq!(
        handler.call_count(),
        1,
        "web_fetch must trigger the approval handler exactly once in Normal mode"
    );
    assert_eq!(result.output, "gold price fetched");
}

/// web_fetch in Bypass mode: no prompt, tool runs, result returned.
#[tokio::test]
async fn loop_web_fetch_no_prompt_in_bypass_mode() {
    let handler = RecordingApprovalHandler::new();

    let model = ToolThenTextModel {
        tool_name: "web_fetch",
        tool_id: "wf1",
        final_text: "gold: $3000",
    };

    let web_fetch: Arc<dyn Tool> = Arc::new(agent_tools::WebFetchTool::new());
    let agent = Agent::builder("gold-agent").tool(web_fetch).build();

    // Default config = Bypass mode
    let config = RunConfig::builder(provider(model), "mock")
        .approval_handler(Arc::new(handler.clone()))
        .build();

    let result = run(
        &agent,
        Input::Fresh {
            prompt: "gold price".into(),
        },
        &config,
    )
    .await
    .unwrap();

    assert_eq!(
        handler.call_count(),
        0,
        "Bypass mode must not prompt for web_fetch"
    );
    assert_eq!(result.output, "gold: $3000");
}

// ── Stream event invariants ───────────────────────────────────────────────────

/// Every run emits exactly one terminal RunEvent, regardless of how many turns
/// it takes to complete.
#[tokio::test]
async fn loop_stream_emits_exactly_one_terminal_event() {
    let agent = Agent::builder("a")
        .tool(Arc::new(NeedsApprovalTool) as Arc<dyn Tool>)
        .build();
    let config = RunConfig::builder(
        provider(ToolThenTextModel {
            tool_name: "needs_approval",
            tool_id: "t1",
            final_text: "done",
        }),
        "mock",
    )
    // Bypass: tool runs freely, 2-turn run
    .build();

    let events: Vec<RunEvent> = run_stream(
        &agent,
        Input::Fresh {
            prompt: "go".into(),
        },
        &config,
    )
    .collect()
    .await;

    let terminal_count = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                RunEvent::AgentEnd { .. }
                    | RunEvent::MaxTurns { .. }
                    | RunEvent::Aborted { .. }
                    | RunEvent::Error { .. }
                    | RunEvent::Interruption { .. }
                    | RunEvent::GuardrailTripped { .. }
            )
        })
        .count();
    assert_eq!(
        terminal_count, 1,
        "exactly one terminal event per streamed run"
    );
}

/// TurnStart events are emitted once per turn, numbered from 1.
#[tokio::test]
async fn loop_stream_turn_start_events_numbered_from_one() {
    let agent = Agent::builder("a").build();
    let config = RunConfig::builder(provider(TextModel { text: "hi" }), "mock").build();

    let events: Vec<RunEvent> = run_stream(&agent, Input::Fresh { prompt: "q".into() }, &config)
        .collect()
        .await;

    let turn_starts: Vec<u32> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::TurnStart { turn, .. } => Some(*turn),
            _ => None,
        })
        .collect();

    assert_eq!(
        turn_starts,
        vec![1],
        "single-turn run must emit TurnStart {{ turn: 1 }}"
    );
}
