//! agent-core: Core traits, types, and loop logic for the arlo-rust agent framework.

pub mod agent;
pub mod compaction;
pub mod compactor;
pub mod config;
pub mod error;
pub mod event;
pub mod executor;
pub mod guardrail;
pub mod message;
pub mod model;
pub mod next_step;
pub mod permission;
pub mod recovery;
pub mod run_loop;
pub mod state;
pub mod stream;
pub mod skill;
pub mod sub_agent;
pub mod pattern;
pub mod settings;
pub mod task_store;
pub mod in_memory_task_store;
pub mod tool;

pub use agent::{
    Agent, AgentBuilder, AgentHooks, BoxFuture, HookCallback, Instructions, RunContext,
    SubAgentDef,
};
pub use config::{ApprovalContext, ApprovalHandler, ApprovalResponse, DenyAllApprovalHandler, Input, RunConfig, RunConfigBuilder, RunResult};
pub use error::{ModelError, RunError, ToolError};
pub use event::{RunEvent, RunStream};
pub use guardrail::{GuardrailResult, InputGuardrail, OutputGuardrail, ToolGuardrail};
pub use message::{ContentBlock, Message, ToolUseBlock, Usage};
pub use model::{Model, ModelProvider, ModelRequest, ModelResponse, ModelStream, ToolDefinition};
pub use next_step::{ContinueReason, NextStep, PendingApproval, RecoveryStrategy};
pub use permission::{PermissionDecision, PermissionEngine, PermissionMode};
pub use state::{CompactionState, RunState, SCHEMA_VERSION};
pub use stream::{StopReason, StreamChunk};
pub use tool::{ApprovalRequirement, Concurrency, Tool, ToolContext, ToolOutput};

pub use compactor::{
    CompactionConfig, CompactionEvent, CompactionFn, CompactionStage, ContextCompactor,
};

pub use compaction::{
    config::CompactionLayerConfig,
    layer::{CompactionContext, CompactionLayer, LayerResult},
    tokens::{compute_token_count, estimate_tokens},
    CompactionPipeline,
};

pub use executor::{StreamingToolExecutor, ToolResult};

pub use run_loop::{run, run_stream};

pub use recovery::{RecoveryTracker, MAX_RECOVERY_ATTEMPTS};

pub use sub_agent::{SubAgentTool, SubAgentUsage};

pub use skill::{
    render_skill_body, Skill, SkillArgument, SkillContext, SkillRegistry, SkillSource, SkillTool,
};

pub use task_store::{
    CreateTaskParams, TaskEntry, TaskId, TaskStatus, TaskStatusCounts, TaskStore, TaskStoreError,
    TaskType, TaskUsage, TodoItem, TodoStatus,
};

pub use in_memory_task_store::InMemoryTaskStore;
