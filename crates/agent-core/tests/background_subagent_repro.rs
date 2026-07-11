//! Repro: background sub-agent spawned via SubAgentTool with a TaskStore
//! should transition Pending → Running → Completed and store its output.

use std::sync::Arc;

use agent_core::{
    run, Agent, InMemoryTaskStore, Input, Instructions, RunConfig, SubAgentDef, SubAgentTool,
    TaskStatus, TaskStore,
};
use agent_core::{Message, Usage};
use agent_core::{Model, ModelProvider, ModelRequest, ModelResponse, ModelStream};
use agent_core::{StopReason, StreamChunk};
use async_trait::async_trait;
use futures::stream;
use serde_json::json;

/// Parent model: calls the sub_agent tool once, then ends the turn.
struct ParentModel;

#[async_trait]
impl Model for ParentModel {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, agent_core::ModelError> {
        let has_tool_result = request
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult { .. }));

        let chunks = if has_tool_result {
            vec![
                Ok(StreamChunk::TextDelta {
                    text: "Delegated to background sub-agent.".to_string(),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                }),
            ]
        } else {
            vec![
                Ok(StreamChunk::ToolUseStart {
                    id: "tu_1".to_string(),
                    name: "sub_agent".to_string(),
                }),
                Ok(StreamChunk::ToolUseEnd {
                    id: "tu_1".to_string(),
                    input: json!({"task": "count the files"}),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                }),
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }

    async fn complete(
        &self,
        _request: ModelRequest,
    ) -> Result<ModelResponse, agent_core::ModelError> {
        unimplemented!()
    }

    fn name(&self) -> &str {
        "parent-model"
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
        0.0
    }
    fn output_cost_per_million(&self) -> f64 {
        0.0
    }
}

/// Sub-agent model: completes immediately with a text answer.
struct SubModel;

#[async_trait]
impl Model for SubModel {
    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream, agent_core::ModelError> {
        let chunks = vec![
            Ok(StreamChunk::TextDelta {
                text: "42 files".to_string(),
            }),
            Ok(StreamChunk::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            }),
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }

    async fn complete(
        &self,
        _request: ModelRequest,
    ) -> Result<ModelResponse, agent_core::ModelError> {
        unimplemented!()
    }

    fn name(&self) -> &str {
        "sub-model"
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
        0.0
    }
    fn output_cost_per_million(&self) -> f64 {
        0.0
    }
}

struct RoutingProvider;

#[async_trait]
impl ModelProvider for RoutingProvider {
    async fn resolve(&self, model_name: &str) -> Result<Arc<dyn Model>, agent_core::ModelError> {
        match model_name {
            "parent" => Ok(Arc::new(ParentModel)),
            _ => Ok(Arc::new(SubModel)),
        }
    }
    fn available_models(&self) -> Vec<String> {
        vec!["parent".to_string(), "sub".to_string()]
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn background_sub_agent_completes_in_store() {
    let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let provider: Arc<dyn ModelProvider> = Arc::new(RoutingProvider);

    // Sub-agent runs on the "sub" model
    let sub_agent = Agent::builder("sub-agent")
        .instructions(Instructions::Static("helper".to_string()))
        .build();
    let sub_config = RunConfig::builder(provider.clone(), "sub")
        .max_turns(5)
        .build();

    let def = SubAgentDef {
        agent: Arc::new(sub_agent),
        tool_name: Some("sub_agent".to_string()),
        tool_description: Some("bg helper".to_string()),
        input_schema: None,
        max_turns: Some(5),
        background: true,
        allowed_tools: None,
    };
    let sub_tool = SubAgentTool::with_task_store(def, sub_config, store.clone());

    // Parent runs on the "parent" model
    let parent = Agent::builder("parent").tool(Arc::new(sub_tool)).build();
    let parent_config = RunConfig::builder(provider.clone(), "parent")
        .task_store(store.clone())
        .max_turns(5)
        .build();

    let result = run(
        &parent,
        Input::Fresh {
            prompt: "delegate".to_string(),
        },
        &parent_config,
    )
    .await
    .expect("parent run should succeed");
    assert!(result.output.contains("Delegated"));

    // Wait for the background task to reach a terminal state (up to 5 s).
    let mut completed = None;
    for _ in 0..100 {
        let tasks = store.list_tasks(None).await.unwrap();
        if let Some(t) = tasks.iter().find(|t| t.status.is_terminal()) {
            completed = Some(t.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let task = completed.expect("background sub-agent task never reached a terminal state");
    assert_eq!(task.status, TaskStatus::Completed);
    assert_eq!(task.output.as_deref(), Some("42 files"));
}

/// Parent model that reacts to background-task notifications:
/// 1. No tool result yet → delegate to sub_agent
/// 2. Tool result present → end turn ("waiting") — the run loop must NOT
///    finish here; it should wait for the sub-agent and inject its result
/// 3. Notification seen → final answer echoing the sub-agent's output
struct AwaitingParentModel;

#[async_trait]
impl Model for AwaitingParentModel {
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, agent_core::ModelError> {
        let saw_notification = request.messages.iter().any(|m| match m {
            Message::User { content } => content.iter().any(|b| match b {
                agent_core::ContentBlock::Text { text } => {
                    text.contains("[background task completed]")
                }
                _ => false,
            }),
            _ => false,
        });
        let has_tool_result = request
            .messages
            .iter()
            .any(|m| matches!(m, Message::ToolResult { .. }));

        let chunks = if saw_notification {
            vec![
                Ok(StreamChunk::TextDelta {
                    text: "FINAL: sub-agent reported 42 files".to_string(),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                }),
            ]
        } else if has_tool_result {
            vec![
                Ok(StreamChunk::TextDelta {
                    text: "Waiting for the sub-agent.".to_string(),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                }),
            ]
        } else {
            vec![
                Ok(StreamChunk::ToolUseStart {
                    id: "tu_1".to_string(),
                    name: "sub_agent".to_string(),
                }),
                Ok(StreamChunk::ToolUseEnd {
                    id: "tu_1".to_string(),
                    input: json!({"task": "count the files"}),
                }),
                Ok(StreamChunk::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                }),
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }

    async fn complete(
        &self,
        _request: ModelRequest,
    ) -> Result<ModelResponse, agent_core::ModelError> {
        unimplemented!()
    }

    fn name(&self) -> &str {
        "awaiting-parent-model"
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
        0.0
    }
    fn output_cost_per_million(&self) -> f64 {
        0.0
    }
}

struct AwaitingRoutingProvider;

#[async_trait]
impl ModelProvider for AwaitingRoutingProvider {
    async fn resolve(&self, model_name: &str) -> Result<Arc<dyn Model>, agent_core::ModelError> {
        match model_name {
            "parent" => Ok(Arc::new(AwaitingParentModel)),
            _ => Ok(Arc::new(SubModel)),
        }
    }
    fn available_models(&self) -> Vec<String> {
        vec!["parent".to_string(), "sub".to_string()]
    }
}

/// The parent run must not terminate while a background sub-agent is running:
/// the run loop waits, injects the result as a notification message, and the
/// model's final output reflects the sub-agent's result.
#[tokio::test(flavor = "multi_thread")]
async fn parent_run_receives_background_sub_agent_result() {
    let store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    let provider: Arc<dyn ModelProvider> = Arc::new(AwaitingRoutingProvider);

    let sub_agent = Agent::builder("sub-agent")
        .instructions(Instructions::Static("helper".to_string()))
        .build();
    let sub_config = RunConfig::builder(provider.clone(), "sub")
        .max_turns(5)
        .build();

    let def = SubAgentDef {
        agent: Arc::new(sub_agent),
        tool_name: Some("sub_agent".to_string()),
        tool_description: Some("bg helper".to_string()),
        input_schema: None,
        max_turns: Some(5),
        background: true,
        allowed_tools: None,
    };
    let sub_tool = SubAgentTool::with_task_store(def, sub_config, store.clone());

    let parent = Agent::builder("parent").tool(Arc::new(sub_tool)).build();
    let parent_config = RunConfig::builder(provider.clone(), "parent")
        .task_store(store.clone())
        .max_turns(10)
        .build();

    let result = run(
        &parent,
        Input::Fresh {
            prompt: "delegate and report".to_string(),
        },
        &parent_config,
    )
    .await
    .expect("parent run should succeed");

    // The final output must be the notification-aware answer, not "Waiting".
    assert!(
        result.output.contains("FINAL"),
        "parent never saw the sub-agent result; final output: {:?}",
        result.output
    );

    // The delivered task must be acknowledged (delivered exactly once).
    let unacked = store.list_unacknowledged_terminal().await.unwrap();
    assert!(
        unacked.is_empty(),
        "terminal tasks left unacknowledged: {:?}",
        unacked
    );
}
