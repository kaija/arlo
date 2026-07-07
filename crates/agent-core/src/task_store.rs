//! Task store abstraction layer: types, enums, and the `TaskStore` trait.
//!
//! This module defines the storage-agnostic interface for managing background task
//! lifecycle and LLM planning items within agent-core.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use thiserror::Error;

// ── Type Aliases ────────────────────────────────────────────────────────────

/// A unique identifier for a task entry. Wraps a UUID v4 string.
pub type TaskId = String;

// ── Enums ───────────────────────────────────────────────────────────────────

/// Lifecycle states for a background/sub-agent task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Task registered but not yet started.
    Pending,
    /// Task is currently executing.
    Running,
    /// Task finished successfully.
    Completed,
    /// Task encountered an unrecoverable error (or retries exhausted).
    Failed,
    /// Task was explicitly cancelled.
    Killed,
}

impl TaskStatus {
    /// Returns true if this is a terminal state (no further transitions allowed).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Killed)
    }
}

/// Distinguishes between different kinds of background tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskType {
    /// A sub-agent spawned via SubAgentTool.
    SubAgent,
    /// A generic background task.
    Background,
}

/// Progress states for a planning/todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

// ── Structs ─────────────────────────────────────────────────────────────────

/// Token and cost usage metadata for a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TaskUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// A single record in the TaskRegistry representing one background or sub-agent task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEntry {
    /// Unique identifier (UUID v4).
    pub id: TaskId,
    /// Current lifecycle state.
    pub status: TaskStatus,
    /// Human-readable description of the task.
    pub description: String,
    /// Type of task (SubAgent, Background).
    pub task_type: TaskType,
    /// When the task was registered.
    pub created_at: SystemTime,
    /// When the task reached a terminal state.
    pub completed_at: Option<SystemTime>,
    /// Task result upon completion, or error description on failure.
    pub output: Option<String>,
    /// Token/cost usage accumulated by this task.
    pub usage: Option<TaskUsage>,
    /// TaskIds that must complete before this task can run.
    pub dependencies: Vec<TaskId>,
    /// Maximum retry attempts (0 = no retries).
    pub max_retries: u32,
    /// Number of retries attempted so far.
    pub retry_count: u32,
    /// Error message from the most recent failure.
    pub last_error: Option<String>,
    /// Whether this task has been acknowledged after reaching terminal state.
    pub acknowledged: bool,
}

/// A single planning/progress item in the TodoList.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Unique identifier (UUID v4).
    pub id: String,
    /// Description of the planned work.
    pub content: String,
    /// Current progress state.
    pub status: TodoStatus,
    /// Optional contextual display variant.
    pub active_form: Option<String>,
}

/// Parameters for creating a new TaskEntry.
#[derive(Debug, Clone)]
pub struct CreateTaskParams {
    pub description: String,
    pub task_type: TaskType,
    pub dependencies: Vec<TaskId>,
    pub max_retries: u32,
}

/// Summary of task counts by status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskStatusCounts {
    pub pending: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub killed: usize,
}

// ── Errors ──────────────────────────────────────────────────────────────────

/// Errors returned by TaskStore operations.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum TaskStoreError {
    /// The specified TaskId or TodoItem ID does not exist.
    #[error("Not found: {id}")]
    NotFound { id: String },

    /// An illegal state transition was attempted.
    #[error("Invalid transition: cannot move from {from:?} to {to:?}")]
    InvalidTransition { from: TaskStatus, to: TaskStatus },

    /// A dependency task failed or was killed.
    #[error("Dependency failed: task {dependency_id} is in state {status:?}")]
    DependencyFailed {
        dependency_id: TaskId,
        status: TaskStatus,
    },

    /// An unexpected internal error.
    #[error("Storage error: {message}")]
    StorageError { message: String },
}

// ── Trait Definition ────────────────────────────────────────────────────────

/// The async trait defining the storage abstraction for both task registry entries
/// and todo list items.
#[async_trait]
pub trait TaskStore: Send + Sync {
    // ── TaskEntry CRUD ──────────────────────────────────────────────

    /// Create a new TaskEntry with Pending status. Returns the generated TaskId.
    async fn create_task(&self, params: CreateTaskParams) -> Result<TaskId, TaskStoreError>;

    /// Retrieve a TaskEntry by its ID.
    async fn get_task(&self, id: &str) -> Result<TaskEntry, TaskStoreError>;

    /// Transition a task's status. Validates the transition is legal.
    /// On transition to a terminal state, sets completed_at.
    /// On fail with retries remaining, resets to Pending and increments retry_count.
    async fn transition_task(
        &self,
        id: &str,
        to: TaskStatus,
        output: Option<String>,
    ) -> Result<(), TaskStoreError>;

    /// Update usage metadata for a task.
    async fn update_task_usage(&self, id: &str, usage: TaskUsage) -> Result<(), TaskStoreError>;

    /// Delete a task by ID. Returns NotFound if it doesn't exist.
    async fn delete_task(&self, id: &str) -> Result<(), TaskStoreError>;

    // ── Task Querying ───────────────────────────────────────────────

    /// List all tasks, optionally filtered by status.
    async fn list_tasks(
        &self,
        status: Option<TaskStatus>,
    ) -> Result<Vec<TaskEntry>, TaskStoreError>;

    /// List all unacknowledged tasks in terminal states.
    async fn list_unacknowledged_terminal(&self) -> Result<Vec<TaskEntry>, TaskStoreError>;

    /// Mark a terminal task as acknowledged.
    async fn acknowledge_task(&self, id: &str) -> Result<(), TaskStoreError>;

    /// Count tasks grouped by status.
    async fn count_by_status(&self) -> Result<TaskStatusCounts, TaskStoreError>;

    /// List tasks that are Pending with all dependencies met (ready to run).
    async fn list_ready_tasks(&self) -> Result<Vec<TaskEntry>, TaskStoreError>;

    /// List tasks that are Pending with unmet dependencies (blocked).
    async fn list_blocked_tasks(&self) -> Result<Vec<TaskEntry>, TaskStoreError>;

    // ── Garbage Collection ──────────────────────────────────────────

    /// Evict all acknowledged terminal tasks. Returns count of evicted entries.
    async fn evict_acknowledged(&self) -> Result<usize, TaskStoreError>;

    /// Evict terminal tasks older than the given duration (regardless of acknowledged status).
    async fn evict_older_than(&self, age: Duration) -> Result<usize, TaskStoreError>;

    // ── TodoItem CRUD ───────────────────────────────────────────────

    /// Add a new TodoItem with Pending status. Returns the generated ID.
    async fn add_todo(
        &self,
        content: String,
        active_form: Option<String>,
    ) -> Result<String, TaskStoreError>;

    /// Retrieve a TodoItem by ID.
    async fn get_todo(&self, id: &str) -> Result<TodoItem, TaskStoreError>;

    /// Update a TodoItem's status.
    async fn update_todo_status(
        &self,
        id: &str,
        status: TodoStatus,
    ) -> Result<(), TaskStoreError>;

    /// Remove a TodoItem by ID.
    async fn remove_todo(&self, id: &str) -> Result<(), TaskStoreError>;

    /// Remove all completed TodoItems. Returns count removed.
    async fn clear_completed_todos(&self) -> Result<usize, TaskStoreError>;

    /// List all TodoItems in insertion order.
    async fn list_todos(&self) -> Result<Vec<TodoItem>, TaskStoreError>;
}


#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── Strategies ──────────────────────────────────────────────────────────

    fn task_status_strategy() -> impl Strategy<Value = TaskStatus> {
        prop_oneof![
            Just(TaskStatus::Pending),
            Just(TaskStatus::Running),
            Just(TaskStatus::Completed),
            Just(TaskStatus::Failed),
            Just(TaskStatus::Killed),
        ]
    }

    fn todo_status_strategy() -> impl Strategy<Value = TodoStatus> {
        prop_oneof![
            Just(TodoStatus::Pending),
            Just(TodoStatus::InProgress),
            Just(TodoStatus::Completed),
        ]
    }

    fn task_type_strategy() -> impl Strategy<Value = TaskType> {
        prop_oneof![Just(TaskType::SubAgent), Just(TaskType::Background),]
    }

    /// Generate a SystemTime within a reasonable range (year 2000 to 2100).
    fn system_time_strategy() -> impl Strategy<Value = SystemTime> {
        // Seconds from UNIX_EPOCH: year 2000 (~946684800) to year 2100 (~4102444800)
        (946_684_800u64..4_102_444_800u64).prop_map(|secs| {
            std::time::UNIX_EPOCH + Duration::from_secs(secs)
        })
    }

    /// Generate a finite f64 suitable for JSON round-trip (no NaN, no Infinity).
    /// We limit to integer cents (divide by 100) to avoid floating-point precision
    /// issues that arise when large f64 values don't round-trip through decimal JSON.
    fn finite_f64_strategy() -> impl Strategy<Value = f64> {
        (-1_000_000_00i64..1_000_000_00i64).prop_map(|cents| cents as f64 / 100.0)
    }

    fn task_usage_strategy() -> impl Strategy<Value = TaskUsage> {
        (any::<u64>(), any::<u64>(), finite_f64_strategy()).prop_map(
            |(input_tokens, output_tokens, cost_usd)| TaskUsage {
                input_tokens,
                output_tokens,
                cost_usd,
            },
        )
    }

    fn task_entry_strategy() -> impl Strategy<Value = TaskEntry> {
        // Split into two tuples to stay within proptest's 12-element tuple limit.
        let part1 = (
            "[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}",
            task_status_strategy(),
            ".*",
            task_type_strategy(),
            system_time_strategy(),
            proptest::option::of(system_time_strategy()),
            proptest::option::of(".*"),
            proptest::option::of(task_usage_strategy()),
            proptest::collection::vec(
                "[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}",
                0..4,
            ),
        );
        let part2 = (0u32..10, 0u32..10, proptest::option::of(".*"), any::<bool>());

        (part1, part2).prop_map(
            |(
                (id, status, description, task_type, created_at, completed_at, output, usage, dependencies),
                (max_retries, retry_count, last_error, acknowledged),
            )| {
                TaskEntry {
                    id,
                    status,
                    description,
                    task_type,
                    created_at,
                    completed_at,
                    output,
                    usage,
                    dependencies,
                    max_retries,
                    retry_count,
                    last_error,
                    acknowledged,
                }
            },
        )
    }

    fn todo_item_strategy() -> impl Strategy<Value = TodoItem> {
        (
            "[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}",
            ".*",
            todo_status_strategy(),
            proptest::option::of(".*"),
        )
            .prop_map(|(id, content, status, active_form)| TodoItem {
                id,
                content,
                status,
                active_form,
            })
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 11: Serialization Round-Trip
    // **Validates: Requirements 2.8, 4.5**
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any valid TaskEntry, serializing via serde_json and deserializing
        /// produces a value equal to the original.
        ///
        /// **Validates: Requirements 2.8, 4.5**
        #[test]
        fn task_entry_serialization_round_trip(entry in task_entry_strategy()) {
            let serialized = serde_json::to_vec(&entry).expect("TaskEntry should serialize");
            let deserialized: TaskEntry =
                serde_json::from_slice(&serialized).expect("TaskEntry should deserialize");
            prop_assert_eq!(&entry, &deserialized);
        }

        /// For any valid TodoItem, serializing via serde_json and deserializing
        /// produces a value equal to the original.
        ///
        /// **Validates: Requirements 2.8, 4.5**
        #[test]
        fn todo_item_serialization_round_trip(item in todo_item_strategy()) {
            let serialized = serde_json::to_vec(&item).expect("TodoItem should serialize");
            let deserialized: TodoItem =
                serde_json::from_slice(&serialized).expect("TodoItem should deserialize");
            prop_assert_eq!(&item, &deserialized);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 13: Error Display Contains Context
    // **Validates: Requirements 11.5**
    // ═══════════════════════════════════════════════════════════════════════

    /// Strategy to generate arbitrary TaskStoreError variants with meaningful field values.
    fn task_store_error_strategy() -> impl Strategy<Value = TaskStoreError> {
        prop_oneof![
            "[a-zA-Z0-9_-]{1,50}".prop_map(|id| TaskStoreError::NotFound { id }),
            (task_status_strategy(), task_status_strategy()).prop_map(|(from, to)| {
                TaskStoreError::InvalidTransition { from, to }
            }),
            ("[a-zA-Z0-9_-]{1,50}", task_status_strategy()).prop_map(
                |(dependency_id, status)| TaskStoreError::DependencyFailed {
                    dependency_id,
                    status,
                }
            ),
            "[a-zA-Z0-9 _.,!-]{1,100}".prop_map(|message| TaskStoreError::StorageError {
                message,
            }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any TaskStoreError variant, the Display output contains the
        /// variant-specific context values.
        ///
        /// **Validates: Requirements 11.5**
        #[test]
        fn error_display_contains_context(error in task_store_error_strategy()) {
            let display = format!("{}", error);
            match &error {
                TaskStoreError::NotFound { id } => {
                    prop_assert!(
                        display.contains(id),
                        "NotFound display '{}' should contain id '{}'", display, id
                    );
                }
                TaskStoreError::InvalidTransition { from, to } => {
                    let from_str = format!("{:?}", from);
                    let to_str = format!("{:?}", to);
                    prop_assert!(
                        display.contains(&from_str),
                        "InvalidTransition display '{}' should contain from status '{}'",
                        display, from_str
                    );
                    prop_assert!(
                        display.contains(&to_str),
                        "InvalidTransition display '{}' should contain to status '{}'",
                        display, to_str
                    );
                }
                TaskStoreError::DependencyFailed { dependency_id, status } => {
                    let status_str = format!("{:?}", status);
                    prop_assert!(
                        display.contains(dependency_id),
                        "DependencyFailed display '{}' should contain dependency_id '{}'",
                        display, dependency_id
                    );
                    prop_assert!(
                        display.contains(&status_str),
                        "DependencyFailed display '{}' should contain status '{}'",
                        display, status_str
                    );
                }
                TaskStoreError::StorageError { message } => {
                    prop_assert!(
                        display.contains(message),
                        "StorageError display '{}' should contain message '{}'",
                        display, message
                    );
                }
            }
        }
    }
}
