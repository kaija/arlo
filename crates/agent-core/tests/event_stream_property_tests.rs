//! Property-based tests for event stream well-formedness.
//!
//! Feature: rust-agent-framework, Property 19: Event stream well-formedness
//! **Validates: Requirements 21.8, 21.9**
//!
//! For any run execution (regardless of outcome), the RunLoop shall emit exactly
//! one terminal event (AgentEnd, MaxTurns, Aborted, Error, Interruption, or
//! GuardrailTripped) as the final event. Additionally, for every tool execution,
//! ToolStart shall always be emitted before the corresponding ToolEnd for the same
//! tool id.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;

use agent_core::tool::Concurrency;
use agent_core::{
    run_stream, Agent, ContentBlock, Input, Instructions, Message, Model, ModelError,
    ModelProvider, ModelRequest, ModelResponse, ModelStream, RunConfig, RunEvent, StopReason,
    StreamChunk, Tool, ToolContext, ToolOutput, Usage,
};

// --- Helper: terminal event detection ---

fn is_terminal_event(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::AgentEnd { .. }
            | RunEvent::MaxTurns { .. }
            | RunEvent::Aborted { .. }
            | RunEvent::Error { .. }
            | RunEvent::Interruption { .. }
            | RunEvent::GuardrailTripped { .. }
    )
}

/// Extract tool id from ToolStart events.
fn tool_start_id(event: &RunEvent) -> Option<&str> {
    match event {
        RunEvent::ToolStart { id, .. } => Some(id.as_str()),
        _ => None,
    }
}

/// Extract tool id from ToolEnd events.
fn tool_end_id(event: &RunEvent) -> Option<&str> {
    match event {
        RunEvent::ToolEnd { id, .. } => Some(id.as_str()),
        _ => None,
    }
}

// --- Mock components ---

/// A mock model that returns a simple text response (no tools).
struct SimpleTextModel {
    response: String,
}

#[async_trait]
impl Model for SimpleTextModel {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
        let text = self.response.clone();
        let chunks = vec![
            Ok(StreamChunk::TextDelta { text }),
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

    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }

    fn name(&self) -> &str {
        "simple-text-model"
    }
    fn provider(&self) -> &str {
        "mock"
    }
    fn context_window(&self) -> usize {
        128000
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

/// A mock model that calls a tool on the first invocation, then returns text.
struct SingleToolCallModel;

#[async_trait]
impl Model for SingleToolCallModel {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ModelError> {
        // If there are tool results in messages, respond with final text
        let has_tool_result = request
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult { .. }));

        if has_tool_result {
            let chunks = vec![
                Ok(StreamChunk::TextDelta {
                    text: "Done with tool.".to_string(),
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

        // First call: invoke a tool
        let chunks = vec![
            Ok(StreamChunk::ToolUseStart {
                id: "tool_abc".to_string(),
                name: "echo".to_string(),
            }),
            Ok(StreamChunk::ToolUseInputDelta {
                id: "tool_abc".to_string(),
                delta: r#"{"text":"hello"}"#.to_string(),
            }),
            Ok(StreamChunk::ToolUseEnd {
                id: "tool_abc".to_string(),
                input: json!({"text": "hello"}),
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

    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }

    fn name(&self) -> &str {
        "single-tool-model"
    }
    fn provider(&self) -> &str {
        "mock"
    }
    fn context_window(&self) -> usize {
        128000
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

/// A model that always calls tools (never emits final text) — used to trigger max turns.
struct AlwaysToolCallModel {
    call_count: std::sync::atomic::AtomicU32,
}

impl AlwaysToolCallModel {
    fn new() -> Self {
        Self {
            call_count: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl Model for AlwaysToolCallModel {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
        let n = self
            .call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let tool_id = format!("tool_{}", n);
        let tool_id2 = tool_id.clone();
        let chunks = vec![
            Ok(StreamChunk::ToolUseStart {
                id: tool_id.clone(),
                name: "echo".to_string(),
            }),
            Ok(StreamChunk::ToolUseEnd {
                id: tool_id2,
                input: json!({"text": "loop"}),
            }),
            Ok(StreamChunk::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_tokens: None,
                },
            }),
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }

    fn name(&self) -> &str {
        "always-tool-model"
    }
    fn provider(&self) -> &str {
        "mock"
    }
    fn context_window(&self) -> usize {
        128000
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

/// A model that returns an error on stream.
struct ErrorModel;

#[async_trait]
impl Model for ErrorModel {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
        Err(ModelError::Connection(
            "simulated connection failure".to_string(),
        ))
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }

    fn name(&self) -> &str {
        "error-model"
    }
    fn provider(&self) -> &str {
        "mock"
    }
    fn context_window(&self) -> usize {
        128000
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

/// A model that triggers budget exceeded by returning enormous usage.
struct BudgetBustingModel;

#[async_trait]
impl Model for BudgetBustingModel {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, ModelError> {
        let chunks = vec![
            Ok(StreamChunk::TextDelta {
                text: "expensive response".to_string(),
            }),
            Ok(StreamChunk::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10_000_000,
                    output_tokens: 10_000_000,
                    cache_read_tokens: None,
                },
            }),
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }

    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        unimplemented!()
    }

    fn name(&self) -> &str {
        "budget-busting-model"
    }
    fn provider(&self) -> &str {
        "mock"
    }
    fn context_window(&self) -> usize {
        128000
    }
    fn max_output_tokens(&self) -> usize {
        4096
    }
    fn supports_tools(&self) -> bool {
        true
    }
    fn input_cost_per_million(&self) -> f64 {
        100.0
    }
    fn output_cost_per_million(&self) -> f64 {
        300.0
    }
}

/// A mock provider that returns a given model.
struct MockProvider {
    model: Arc<dyn Model>,
}

#[async_trait]
impl ModelProvider for MockProvider {
    async fn resolve(&self, _model_name: &str) -> Result<Arc<dyn Model>, ModelError> {
        Ok(Arc::clone(&self.model))
    }
    fn available_models(&self) -> Vec<String> {
        vec!["mock".to_string()]
    }
}

/// A simple echo tool for testing.
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echoes input text"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {"text": {"type": "string"}}})
    }
    fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
        Concurrency::Safe
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, agent_core::ToolError> {
        let text = input.get("text").and_then(|v| v.as_str()).unwrap_or("");
        Ok(ToolOutput::Text(text.to_string()))
    }
}

fn make_provider(model: Arc<dyn Model>) -> Arc<dyn ModelProvider> {
    Arc::new(MockProvider { model })
}

// --- Assertion helpers ---

/// Verify event stream well-formedness invariants:
/// 1. Exactly one terminal event exists
/// 2. The terminal event is the LAST event
/// 3. For every ToolEnd, there's a preceding ToolStart with the same id
fn assert_event_stream_well_formed(events: &[RunEvent]) {
    assert!(!events.is_empty(), "Event stream must not be empty");

    // Count terminal events
    let terminal_count = events.iter().filter(|e| is_terminal_event(e)).count();
    assert_eq!(
        terminal_count, 1,
        "Expected exactly 1 terminal event, found {}. Events: {:?}",
        terminal_count, events
    );

    // Terminal event must be the last event
    let last = events.last().unwrap();
    assert!(
        is_terminal_event(last),
        "Last event must be terminal, got: {:?}",
        last
    );

    // No events after the terminal event (implied by the above two checks, but
    // let's also verify no terminal events appear before the last position)
    for (i, event) in events.iter().enumerate() {
        if i < events.len() - 1 {
            assert!(
                !is_terminal_event(event),
                "Terminal event found at position {} (not last). Event: {:?}",
                i,
                event
            );
        }
    }

    // For every ToolEnd, there must be a preceding ToolStart with the same id
    let mut started_tool_ids: Vec<String> = Vec::new();
    for event in events {
        if let Some(id) = tool_start_id(event) {
            started_tool_ids.push(id.to_string());
        }
        if let Some(id) = tool_end_id(event) {
            assert!(
                started_tool_ids.contains(&id.to_string()),
                "ToolEnd for id '{}' without preceding ToolStart. Events: {:?}",
                id,
                events
            );
        }
    }
}

// --- Scenario tests (parametric, covering various execution paths) ---

/// Scenario 1: Simple text response (no tools)
/// The stream should emit events ending in exactly one AgentEnd terminal event.
#[tokio::test]
async fn test_event_stream_simple_text_response() {
    let model: Arc<dyn Model> = Arc::new(SimpleTextModel {
        response: "Hello world".to_string(),
    });
    let provider = make_provider(model);
    let agent = Agent::builder("test-agent")
        .instructions(Instructions::Static("Be helpful.".into()))
        .build();
    let config = RunConfig::builder(provider, "mock").build();
    let input = Input::Fresh {
        prompt: "Say hello".to_string(),
    };

    let stream = run_stream(&agent, input, &config);
    let events: Vec<RunEvent> = stream.collect().await;

    assert_event_stream_well_formed(&events);

    // Specifically, should end with AgentEnd
    let last = events.last().unwrap();
    assert!(
        matches!(last, RunEvent::AgentEnd { .. }),
        "Expected AgentEnd terminal event, got: {:?}",
        last
    );
}

/// Scenario 2: Single tool call then final response.
/// Should still produce exactly one terminal event at the end.
#[tokio::test]
async fn test_event_stream_single_tool_call() {
    let model: Arc<dyn Model> = Arc::new(SingleToolCallModel);
    let provider = make_provider(model);
    let tool: Arc<dyn Tool> = Arc::new(EchoTool);
    let agent = Agent::builder("tool-agent")
        .instructions(Instructions::Static("Use tools.".into()))
        .tool(tool)
        .build();
    let config = RunConfig::builder(provider, "mock").build();
    let input = Input::Fresh {
        prompt: "Echo hello".to_string(),
    };

    let stream = run_stream(&agent, input, &config);
    let events: Vec<RunEvent> = stream.collect().await;

    assert_event_stream_well_formed(&events);

    // Should end with AgentEnd (tool call → final response)
    let last = events.last().unwrap();
    assert!(
        matches!(last, RunEvent::AgentEnd { .. }),
        "Expected AgentEnd terminal event, got: {:?}",
        last
    );
}

/// Scenario 3: Max turns reached.
/// When the agent hits the turn limit, the terminal event should be MaxTurns.
#[tokio::test]
async fn test_event_stream_max_turns_reached() {
    let model: Arc<dyn Model> = Arc::new(AlwaysToolCallModel::new());
    let provider = make_provider(model);
    let tool: Arc<dyn Tool> = Arc::new(EchoTool);
    let agent = Agent::builder("limited-agent")
        .instructions(Instructions::Static("Keep going.".into()))
        .tool(tool)
        .max_turns(2)
        .build();
    let config = RunConfig::builder(provider, "mock").max_turns(2).build();
    let input = Input::Fresh {
        prompt: "Do work".to_string(),
    };

    let stream = run_stream(&agent, input, &config);
    let events: Vec<RunEvent> = stream.collect().await;

    assert_event_stream_well_formed(&events);

    // Should end with MaxTurns
    let last = events.last().unwrap();
    assert!(
        matches!(last, RunEvent::MaxTurns { .. }),
        "Expected MaxTurns terminal event, got: {:?}",
        last
    );
}

/// Scenario 4: Budget exceeded triggers Aborted terminal event.
#[tokio::test]
async fn test_event_stream_budget_exceeded() {
    let model: Arc<dyn Model> = Arc::new(BudgetBustingModel);
    let provider = make_provider(model);
    let agent = Agent::builder("budget-agent")
        .instructions(Instructions::Static("Be helpful.".into()))
        .build();
    let config = RunConfig::builder(provider, "mock")
        .budget_usd(0.001) // tiny budget
        .build();
    let input = Input::Fresh {
        prompt: "Hi".to_string(),
    };

    let stream = run_stream(&agent, input, &config);
    let events: Vec<RunEvent> = stream.collect().await;

    assert_event_stream_well_formed(&events);

    // Should end with Aborted (budget_exceeded)
    let last = events.last().unwrap();
    assert!(
        matches!(last, RunEvent::Aborted { reason } if reason == "budget_exceeded"),
        "Expected Aborted(budget_exceeded) terminal event, got: {:?}",
        last
    );
}

/// Scenario 5: Model error produces an Error terminal event.
#[tokio::test]
async fn test_event_stream_model_error() {
    let model: Arc<dyn Model> = Arc::new(ErrorModel);
    let provider = make_provider(model);
    let agent = Agent::builder("error-agent")
        .instructions(Instructions::Static("Be helpful.".into()))
        .build();
    let config = RunConfig::builder(provider, "mock").build();
    let input = Input::Fresh {
        prompt: "Hi".to_string(),
    };

    let stream = run_stream(&agent, input, &config);
    let events: Vec<RunEvent> = stream.collect().await;

    assert_event_stream_well_formed(&events);

    // Should end with Error
    let last = events.last().unwrap();
    assert!(
        matches!(last, RunEvent::Error { .. }),
        "Expected Error terminal event, got: {:?}",
        last
    );
}

/// Scenario 6: Multiple text responses with different content
/// Parametric test running several prompts to ensure invariant holds broadly.
#[tokio::test]
async fn test_event_stream_parametric_text_responses() {
    let prompts = vec![
        "Hello",
        "What is 2+2?",
        "",
        "A very long prompt that contains many words to test with larger inputs",
    ];

    for prompt in prompts {
        let model: Arc<dyn Model> = Arc::new(SimpleTextModel {
            response: format!("Response to: {}", prompt),
        });
        let provider = make_provider(model);
        let agent = Agent::builder("param-agent")
            .instructions(Instructions::Static("Be helpful.".into()))
            .build();
        let config = RunConfig::builder(provider, "mock").build();
        let input = Input::Fresh {
            prompt: prompt.to_string(),
        };

        let stream = run_stream(&agent, input, &config);
        let events: Vec<RunEvent> = stream.collect().await;

        assert_event_stream_well_formed(&events);
    }
}

/// Scenario 7: Input via Items (pre-existing conversation history).
#[tokio::test]
async fn test_event_stream_items_input() {
    let model: Arc<dyn Model> = Arc::new(SimpleTextModel {
        response: "Continuing conversation.".to_string(),
    });
    let provider = make_provider(model);
    let agent = Agent::builder("items-agent")
        .instructions(Instructions::Static("Be helpful.".into()))
        .build();
    let config = RunConfig::builder(provider, "mock").build();
    let input = Input::Items {
        messages: vec![
            Message::System {
                content: "You are helpful.".to_string(),
            },
            Message::User {
                content: vec![ContentBlock::Text {
                    text: "Continue".to_string(),
                }],
            },
        ],
    };

    let stream = run_stream(&agent, input, &config);
    let events: Vec<RunEvent> = stream.collect().await;

    assert_event_stream_well_formed(&events);
}

/// Scenario 8: Agent with max_turns=1 that gets a tool call (triggers MaxTurns quickly).
#[tokio::test]
async fn test_event_stream_max_turns_one() {
    let model: Arc<dyn Model> = Arc::new(SingleToolCallModel);
    let provider = make_provider(model);
    let tool: Arc<dyn Tool> = Arc::new(EchoTool);
    let agent = Agent::builder("one-turn-agent")
        .instructions(Instructions::Static("Use tools.".into()))
        .tool(tool)
        .max_turns(1)
        .build();
    let config = RunConfig::builder(provider, "mock").build();
    let input = Input::Fresh {
        prompt: "Do something".to_string(),
    };

    let stream = run_stream(&agent, input, &config);
    let events: Vec<RunEvent> = stream.collect().await;

    assert_event_stream_well_formed(&events);

    // With max_turns=1, after the first tool-calling turn, it should hit the turn limit
    let last = events.last().unwrap();
    assert!(
        matches!(last, RunEvent::MaxTurns { .. } | RunEvent::AgentEnd { .. }),
        "Expected MaxTurns or AgentEnd terminal event, got: {:?}",
        last
    );
}
