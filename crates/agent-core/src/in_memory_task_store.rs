//! In-memory implementation of the `TaskStore` trait.
//!
//! Uses `tokio::sync::RwLock` for async-safe concurrent access with separate
//! locks for tasks and todos to minimize contention.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::task_store::{
    CreateTaskParams, TaskEntry, TaskId, TaskStatus, TaskStatusCounts, TaskStoreError, TaskStore,
    TaskUsage, TodoItem, TodoStatus,
};

/// An in-memory task store backed by `RwLock<HashMap>` for tasks and `RwLock<Vec>` for todos.
///
/// This store is suitable for single-process usage where persistence across restarts
/// is not required. Both the task registry and todo list are protected by independent
/// `tokio::sync::RwLock`s so that reads/writes to one collection do not block the other.
pub struct InMemoryTaskStore {
    tasks: RwLock<HashMap<TaskId, TaskEntry>>,
    todos: RwLock<Vec<TodoItem>>,
}

impl InMemoryTaskStore {
    /// Create a new, empty `InMemoryTaskStore`.
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            todos: RwLock::new(Vec::new()),
        }
    }
}

impl Default for InMemoryTaskStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskStore for InMemoryTaskStore {
    // ── TaskEntry CRUD ──────────────────────────────────────────────

    async fn create_task(&self, params: CreateTaskParams) -> Result<TaskId, TaskStoreError> {
        let id = Uuid::new_v4().to_string();
        let entry = TaskEntry {
            id: id.clone(),
            status: TaskStatus::Pending,
            description: params.description,
            task_type: params.task_type,
            created_at: SystemTime::now(),
            completed_at: None,
            output: None,
            usage: None,
            dependencies: params.dependencies,
            max_retries: params.max_retries,
            retry_count: 0,
            last_error: None,
            acknowledged: false,
        };
        let mut tasks = self.tasks.write().await;
        tasks.insert(id.clone(), entry);
        Ok(id)
    }

    async fn get_task(&self, id: &str) -> Result<TaskEntry, TaskStoreError> {
        let tasks = self.tasks.read().await;
        tasks
            .get(id)
            .cloned()
            .ok_or_else(|| TaskStoreError::NotFound { id: id.to_string() })
    }

    async fn transition_task(
        &self,
        id: &str,
        to: TaskStatus,
        output: Option<String>,
    ) -> Result<(), TaskStoreError> {
        let mut tasks = self.tasks.write().await;

        // Look up the task, return NotFound if missing.
        let entry = tasks
            .get(id)
            .ok_or_else(|| TaskStoreError::NotFound { id: id.to_string() })?;
        let from = entry.status;

        // Reject any transition from a terminal state.
        if from.is_terminal() {
            return Err(TaskStoreError::InvalidTransition { from, to });
        }

        // Validate the transition is in the allowed set.
        let valid = matches!(
            (from, to),
            (TaskStatus::Pending, TaskStatus::Running)
                | (TaskStatus::Running, TaskStatus::Completed)
                | (TaskStatus::Running, TaskStatus::Failed)
                | (TaskStatus::Pending, TaskStatus::Killed)
                | (TaskStatus::Running, TaskStatus::Killed)
        );
        if !valid {
            return Err(TaskStoreError::InvalidTransition { from, to });
        }

        // Dependency validation for Pending → Running.
        if from == TaskStatus::Pending && to == TaskStatus::Running {
            // Clone deps to avoid borrow conflicts while looking up other entries.
            let deps = entry.dependencies.clone();
            for dep_id in &deps {
                match tasks.get(dep_id) {
                    Some(dep_entry) => match dep_entry.status {
                        TaskStatus::Completed => { /* ok */ }
                        TaskStatus::Failed | TaskStatus::Killed => {
                            return Err(TaskStoreError::DependencyFailed {
                                dependency_id: dep_id.clone(),
                                status: dep_entry.status,
                            });
                        }
                        _ => {
                            // Dependency exists but is not Completed (Pending or Running).
                            return Err(TaskStoreError::InvalidTransition { from, to });
                        }
                    },
                    None => {
                        // Dependency not found — treat as unmet.
                        return Err(TaskStoreError::InvalidTransition { from, to });
                    }
                }
            }
        }

        // Now perform the actual state mutation.
        let entry = tasks
            .get_mut(id)
            .expect("entry must exist; we checked above");

        match to {
            TaskStatus::Failed => {
                if entry.retry_count < entry.max_retries {
                    // Retry: reset to Pending, increment retry_count, store error.
                    entry.status = TaskStatus::Pending;
                    entry.retry_count += 1;
                    entry.last_error = output;
                } else {
                    // Retries exhausted: move to Failed terminal state.
                    entry.status = TaskStatus::Failed;
                    entry.completed_at = Some(SystemTime::now());
                    entry.last_error = output;
                }
            }
            TaskStatus::Completed => {
                entry.status = TaskStatus::Completed;
                entry.completed_at = Some(SystemTime::now());
                entry.output = output;
            }
            TaskStatus::Killed => {
                entry.status = TaskStatus::Killed;
                entry.completed_at = Some(SystemTime::now());
                entry.output = output;
            }
            TaskStatus::Running => {
                entry.status = TaskStatus::Running;
                // No completed_at or output changes for Running.
            }
            TaskStatus::Pending => {
                // This branch is unreachable given our valid-transition check,
                // but included for exhaustiveness.
                entry.status = TaskStatus::Pending;
            }
        }

        Ok(())
    }

    async fn update_task_usage(&self, id: &str, usage: TaskUsage) -> Result<(), TaskStoreError> {
        let mut tasks = self.tasks.write().await;
        let entry = tasks
            .get_mut(id)
            .ok_or_else(|| TaskStoreError::NotFound { id: id.to_string() })?;
        entry.usage = Some(usage);
        Ok(())
    }

    async fn delete_task(&self, id: &str) -> Result<(), TaskStoreError> {
        let mut tasks = self.tasks.write().await;
        tasks
            .remove(id)
            .ok_or_else(|| TaskStoreError::NotFound { id: id.to_string() })?;
        Ok(())
    }

    // ── Task Querying ───────────────────────────────────────────────

    async fn list_tasks(
        &self,
        status: Option<TaskStatus>,
    ) -> Result<Vec<TaskEntry>, TaskStoreError> {
        let tasks = self.tasks.read().await;
        let result = match status {
            Some(s) => tasks.values().filter(|t| t.status == s).cloned().collect(),
            None => tasks.values().cloned().collect(),
        };
        Ok(result)
    }

    async fn list_unacknowledged_terminal(&self) -> Result<Vec<TaskEntry>, TaskStoreError> {
        let tasks = self.tasks.read().await;
        let result = tasks
            .values()
            .filter(|t| t.status.is_terminal() && !t.acknowledged)
            .cloned()
            .collect();
        Ok(result)
    }

    async fn acknowledge_task(&self, id: &str) -> Result<(), TaskStoreError> {
        let mut tasks = self.tasks.write().await;
        let entry = tasks
            .get_mut(id)
            .ok_or_else(|| TaskStoreError::NotFound { id: id.to_string() })?;
        entry.acknowledged = true;
        Ok(())
    }

    async fn count_by_status(&self) -> Result<TaskStatusCounts, TaskStoreError> {
        let tasks = self.tasks.read().await;
        let mut counts = TaskStatusCounts::default();
        for entry in tasks.values() {
            match entry.status {
                TaskStatus::Pending => counts.pending += 1,
                TaskStatus::Running => counts.running += 1,
                TaskStatus::Completed => counts.completed += 1,
                TaskStatus::Failed => counts.failed += 1,
                TaskStatus::Killed => counts.killed += 1,
            }
        }
        Ok(counts)
    }

    async fn list_ready_tasks(&self) -> Result<Vec<TaskEntry>, TaskStoreError> {
        let tasks = self.tasks.read().await;
        let result = tasks
            .values()
            .filter(|t| {
                t.status == TaskStatus::Pending
                    && t.dependencies.iter().all(|dep_id| {
                        tasks
                            .get(dep_id)
                            .map(|dep| dep.status == TaskStatus::Completed)
                            .unwrap_or(false)
                    })
            })
            .cloned()
            .collect();
        Ok(result)
    }

    async fn list_blocked_tasks(&self) -> Result<Vec<TaskEntry>, TaskStoreError> {
        let tasks = self.tasks.read().await;
        let result = tasks
            .values()
            .filter(|t| {
                t.status == TaskStatus::Pending
                    && !t.dependencies.is_empty()
                    && t.dependencies.iter().any(|dep_id| {
                        tasks
                            .get(dep_id)
                            .map(|dep| dep.status != TaskStatus::Completed)
                            .unwrap_or(true)
                    })
            })
            .cloned()
            .collect();
        Ok(result)
    }

    // ── Garbage Collection ──────────────────────────────────────────

    async fn evict_acknowledged(&self) -> Result<usize, TaskStoreError> {
        let mut tasks = self.tasks.write().await;
        let before = tasks.len();
        tasks.retain(|_, entry| !(entry.status.is_terminal() && entry.acknowledged));
        let removed = before - tasks.len();
        Ok(removed)
    }

    async fn evict_older_than(&self, age: Duration) -> Result<usize, TaskStoreError> {
        let cutoff = SystemTime::now()
            .checked_sub(age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut tasks = self.tasks.write().await;
        let before = tasks.len();
        tasks.retain(|_, entry| {
            if !entry.status.is_terminal() {
                return true;
            }
            match entry.completed_at {
                Some(completed) => completed >= cutoff,
                None => true, // No completed_at timestamp — keep it
            }
        });
        let removed = before - tasks.len();
        Ok(removed)
    }

    // ── TodoItem CRUD ───────────────────────────────────────────────

    async fn add_todo(
        &self,
        content: String,
        active_form: Option<String>,
    ) -> Result<String, TaskStoreError> {
        let id = Uuid::new_v4().to_string();
        let item = TodoItem {
            id: id.clone(),
            content,
            status: TodoStatus::Pending,
            active_form,
        };
        let mut todos = self.todos.write().await;
        todos.push(item);
        Ok(id)
    }

    async fn get_todo(&self, id: &str) -> Result<TodoItem, TaskStoreError> {
        let todos = self.todos.read().await;
        todos
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or_else(|| TaskStoreError::NotFound { id: id.to_string() })
    }

    async fn update_todo_status(
        &self,
        id: &str,
        status: TodoStatus,
    ) -> Result<(), TaskStoreError> {
        let mut todos = self.todos.write().await;
        let item = todos
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| TaskStoreError::NotFound { id: id.to_string() })?;
        item.status = status;
        Ok(())
    }

    async fn remove_todo(&self, id: &str) -> Result<(), TaskStoreError> {
        let mut todos = self.todos.write().await;
        let pos = todos
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| TaskStoreError::NotFound { id: id.to_string() })?;
        todos.remove(pos);
        Ok(())
    }

    async fn clear_completed_todos(&self) -> Result<usize, TaskStoreError> {
        let mut todos = self.todos.write().await;
        let before = todos.len();
        todos.retain(|t| t.status != TodoStatus::Completed);
        let removed = before - todos.len();
        Ok(removed)
    }

    async fn list_todos(&self) -> Result<Vec<TodoItem>, TaskStoreError> {
        let todos = self.todos.read().await;
        Ok(todos.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_store::{CreateTaskParams, TaskStore, TaskType};
    use std::collections::HashSet;
    use proptest::prelude::*;
    use proptest::collection::vec as prop_vec;

    // ── Strategies ──────────────────────────────────────────────────────────

    fn task_type_strategy() -> impl Strategy<Value = TaskType> {
        prop_oneof![Just(TaskType::SubAgent), Just(TaskType::Background)]
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 1: TaskEntry CRUD Round-Trip
    // **Validates: Requirements 1.1, 2.3, 3.2, 10.1**
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any valid CreateTaskParams, creating a task via `create_task` and then
        /// retrieving it via `get_task` with the returned TaskId produces a TaskEntry
        /// whose `description`, `task_type`, `dependencies`, and `max_retries` fields
        /// match the input parameters, with `status == Pending` and `retry_count == 0`.
        ///
        /// **Validates: Requirements 1.1, 2.3, 3.2, 10.1**
        #[test]
        fn task_entry_crud_round_trip(
            description in "\\PC*",
            task_type in task_type_strategy(),
            max_retries in 0u32..10,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let params = CreateTaskParams {
                    description: description.clone(),
                    task_type: task_type.clone(),
                    dependencies: vec![],
                    max_retries,
                };

                let id = store
                    .create_task(params)
                    .await
                    .expect("create_task should succeed");

                let entry = store
                    .get_task(&id)
                    .await
                    .expect("get_task should find the task");

                // Verify input fields match
                prop_assert_eq!(&entry.description, &description);
                prop_assert_eq!(&entry.task_type, &task_type);
                prop_assert_eq!(&entry.dependencies, &Vec::<String>::new());
                prop_assert_eq!(entry.max_retries, max_retries);

                // Verify initial state invariants
                prop_assert_eq!(entry.status, TaskStatus::Pending);
                prop_assert_eq!(entry.retry_count, 0);
                prop_assert_eq!(entry.acknowledged, false);
                prop_assert_eq!(entry.completed_at, None);
                prop_assert_eq!(entry.output, None);
                prop_assert_eq!(entry.usage, None);
                prop_assert_eq!(entry.last_error, None);

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 2: TodoItem CRUD Round-Trip
    // **Validates: Requirements 1.2, 4.2, 8.1**
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any valid content string and optional active_form, adding a TodoItem
        /// via `add_todo` and then retrieving it via `get_todo` with the returned ID
        /// produces a TodoItem whose content and active_form fields match the inputs,
        /// with status == Pending.
        ///
        /// **Validates: Requirements 1.2, 4.2, 8.1**
        #[test]
        fn todo_item_crud_round_trip(
            content in "\\PC*",
            active_form in proptest::option::of("\\PC*"),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let id = store
                    .add_todo(content.clone(), active_form.clone())
                    .await
                    .expect("add_todo should succeed");

                let item = store
                    .get_todo(&id)
                    .await
                    .expect("get_todo should find the item");

                prop_assert_eq!(&item.content, &content);
                prop_assert_eq!(&item.active_form, &active_form);
                prop_assert_eq!(item.status, TodoStatus::Pending);
                prop_assert_eq!(&item.id, &id);

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 3: TaskId Uniqueness
    // **Validates: Requirements 2.1, 5.3**
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any sequence of N task creations (where N > 1), all returned TaskId
        /// values are distinct from each other.
        ///
        /// **Validates: Requirements 2.1, 5.3**
        #[test]
        fn task_id_uniqueness(n in 2usize..50) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();
                let mut ids = Vec::with_capacity(n);

                for i in 0..n {
                    let params = CreateTaskParams {
                        description: format!("task-{}", i),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    };
                    let id = store
                        .create_task(params)
                        .await
                        .expect("create_task should succeed");
                    ids.push(id);
                }

                let unique: HashSet<&String> = ids.iter().collect();
                prop_assert_eq!(
                    unique.len(),
                    ids.len(),
                    "All {} TaskIds should be unique, but only {} are distinct",
                    ids.len(),
                    unique.len()
                );

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 6: TodoItem Insertion Order Preserved
    // **Validates: Requirements 4.6, 8.5**
    // ═══════════════════════════════════════════════════════════════════════

    /// Strategy to generate a Vec of (content, active_form) tuples for todo items.
    fn todo_inputs_strategy() -> impl Strategy<Value = Vec<(String, Option<String>)>> {
        prop_vec(
            ("\\PC{1,50}", proptest::option::of("\\PC{1,30}")),
            1..20,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any sequence of N TodoItems added to the store, `list_todos` returns
        /// them in exactly the same order they were inserted (matched by content).
        ///
        /// **Validates: Requirements 4.6, 8.5**
        #[test]
        fn todo_item_insertion_order_preserved(inputs in todo_inputs_strategy()) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Insert all items in sequence
                for (content, active_form) in &inputs {
                    store
                        .add_todo(content.clone(), active_form.clone())
                        .await
                        .expect("add_todo should succeed");
                }

                // Retrieve all items
                let listed = store.list_todos().await.expect("list_todos should succeed");

                // Verify count matches
                prop_assert_eq!(listed.len(), inputs.len());

                // Verify order matches insertion sequence by content
                for (i, (expected_content, expected_active_form)) in inputs.iter().enumerate() {
                    prop_assert_eq!(
                        &listed[i].content,
                        expected_content,
                        "Item at position {} has wrong content",
                        i
                    );
                    prop_assert_eq!(
                        &listed[i].active_form,
                        expected_active_form,
                        "Item at position {} has wrong active_form",
                        i
                    );
                    prop_assert_eq!(
                        listed[i].status,
                        TodoStatus::Pending,
                        "Item at position {} should be Pending",
                        i
                    );
                }

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 4: Valid State Transitions Succeed
    // **Validates: Requirements 3.3, 3.4, 3.5, 3.6, 6.2**
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Pending→Running (no deps): Create a task with no dependencies,
        /// transition to Running should succeed.
        ///
        /// **Validates: Requirements 3.3, 6.2**
        #[test]
        fn valid_transition_pending_to_running_no_deps(
            desc in "\\PC{1,50}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let params = CreateTaskParams {
                    description: desc,
                    task_type: TaskType::Background,
                    dependencies: vec![],
                    max_retries: 0,
                };
                let id = store.create_task(params).await.expect("create_task should succeed");

                // Pending → Running should succeed with no deps
                store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await
                    .expect("Pending→Running with no deps should succeed");

                let task = store.get_task(&id).await.expect("get_task should succeed");
                prop_assert_eq!(task.status, TaskStatus::Running);

                Ok(())
            })?;
        }

        /// Pending→Running (deps completed): Create a dependency task, complete it,
        /// then create a task with that dep. Transition to Running should succeed.
        ///
        /// **Validates: Requirements 3.3, 6.2**
        #[test]
        fn valid_transition_pending_to_running_deps_completed(
            desc in "\\PC{1,50}",
            dep_desc in "\\PC{1,50}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create dependency task and complete it
                let dep_params = CreateTaskParams {
                    description: dep_desc,
                    task_type: TaskType::Background,
                    dependencies: vec![],
                    max_retries: 0,
                };
                let dep_id = store.create_task(dep_params).await.expect("create dep task");
                store
                    .transition_task(&dep_id, TaskStatus::Running, None)
                    .await
                    .expect("dep Pending→Running");
                store
                    .transition_task(&dep_id, TaskStatus::Completed, Some("done".to_string()))
                    .await
                    .expect("dep Running→Completed");

                // Create dependent task
                let params = CreateTaskParams {
                    description: desc,
                    task_type: TaskType::Background,
                    dependencies: vec![dep_id.clone()],
                    max_retries: 0,
                };
                let id = store.create_task(params).await.expect("create dependent task");

                // Pending → Running should succeed with dep completed
                store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await
                    .expect("Pending→Running with completed deps should succeed");

                let task = store.get_task(&id).await.expect("get_task should succeed");
                prop_assert_eq!(task.status, TaskStatus::Running);

                Ok(())
            })?;
        }

        /// Running→Completed: Create a task, transition to Running, then Completed
        /// with arbitrary output.
        ///
        /// **Validates: Requirements 3.4**
        #[test]
        fn valid_transition_running_to_completed(
            desc in "\\PC{1,50}",
            output in proptest::option::of("\\PC{1,100}"),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let params = CreateTaskParams {
                    description: desc,
                    task_type: TaskType::SubAgent,
                    dependencies: vec![],
                    max_retries: 0,
                };
                let id = store.create_task(params).await.expect("create_task");

                store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await
                    .expect("Pending→Running");
                store
                    .transition_task(&id, TaskStatus::Completed, output.clone())
                    .await
                    .expect("Running→Completed should succeed");

                let task = store.get_task(&id).await.expect("get_task");
                prop_assert_eq!(task.status, TaskStatus::Completed);
                prop_assert_eq!(&task.output, &output);
                prop_assert!(task.completed_at.is_some());

                Ok(())
            })?;
        }

        /// Running→Failed (retries exhausted, max_retries=0): Create a task with
        /// max_retries=0, transition to Running, then Failed — should be terminal Failed.
        ///
        /// **Validates: Requirements 3.5, 3.6**
        #[test]
        fn valid_transition_running_to_failed_no_retries(
            desc in "\\PC{1,50}",
            error_msg in proptest::option::of("\\PC{1,100}"),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let params = CreateTaskParams {
                    description: desc,
                    task_type: TaskType::Background,
                    dependencies: vec![],
                    max_retries: 0,
                };
                let id = store.create_task(params).await.expect("create_task");

                store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await
                    .expect("Pending→Running");
                store
                    .transition_task(&id, TaskStatus::Failed, error_msg.clone())
                    .await
                    .expect("Running→Failed should succeed");

                let task = store.get_task(&id).await.expect("get_task");
                prop_assert_eq!(task.status, TaskStatus::Failed);
                prop_assert_eq!(&task.last_error, &error_msg);
                prop_assert!(task.completed_at.is_some());
                prop_assert!(task.status.is_terminal());

                Ok(())
            })?;
        }

        /// Pending→Killed: Create a task, transition directly to Killed.
        ///
        /// **Validates: Requirements 3.6**
        #[test]
        fn valid_transition_pending_to_killed(
            desc in "\\PC{1,50}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let params = CreateTaskParams {
                    description: desc,
                    task_type: TaskType::Background,
                    dependencies: vec![],
                    max_retries: 0,
                };
                let id = store.create_task(params).await.expect("create_task");

                store
                    .transition_task(&id, TaskStatus::Killed, None)
                    .await
                    .expect("Pending→Killed should succeed");

                let task = store.get_task(&id).await.expect("get_task");
                prop_assert_eq!(task.status, TaskStatus::Killed);
                prop_assert!(task.completed_at.is_some());
                prop_assert!(task.status.is_terminal());

                Ok(())
            })?;
        }

        /// Running→Killed: Create a task, transition to Running, then Killed.
        ///
        /// **Validates: Requirements 3.6**
        #[test]
        fn valid_transition_running_to_killed(
            desc in "\\PC{1,50}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let params = CreateTaskParams {
                    description: desc,
                    task_type: TaskType::SubAgent,
                    dependencies: vec![],
                    max_retries: 0,
                };
                let id = store.create_task(params).await.expect("create_task");

                store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await
                    .expect("Pending→Running");
                store
                    .transition_task(&id, TaskStatus::Killed, None)
                    .await
                    .expect("Running→Killed should succeed");

                let task = store.get_task(&id).await.expect("get_task");
                prop_assert_eq!(task.status, TaskStatus::Killed);
                prop_assert!(task.completed_at.is_some());
                prop_assert!(task.status.is_terminal());

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 5: Invalid State Transitions Are Rejected
    // **Validates: Requirements 3.7, 11.2**
    // ═══════════════════════════════════════════════════════════════════════

    /// Strategy to generate an arbitrary TaskStatus for target transition attempts.
    fn any_task_status_strategy() -> impl Strategy<Value = TaskStatus> {
        prop_oneof![
            Just(TaskStatus::Pending),
            Just(TaskStatus::Running),
            Just(TaskStatus::Completed),
            Just(TaskStatus::Failed),
            Just(TaskStatus::Killed),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any task in Completed state and any target state, attempting
        /// transition_task returns InvalidTransition{from: Completed, to: target}.
        ///
        /// **Validates: Requirements 3.7, 11.2**
        #[test]
        fn invalid_transition_from_completed(target in any_task_status_strategy()) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create task, move to Running, then Completed
                let id = store
                    .create_task(CreateTaskParams {
                        description: "test task".to_string(),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create_task should succeed");

                store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await
                    .expect("Pending→Running should succeed");

                store
                    .transition_task(&id, TaskStatus::Completed, Some("done".to_string()))
                    .await
                    .expect("Running→Completed should succeed");

                // Now attempt any transition from Completed — should fail
                let result = store.transition_task(&id, target, None).await;
                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::InvalidTransition {
                        from: TaskStatus::Completed,
                        to: target,
                    })
                );

                Ok(())
            })?;
        }

        /// For any task in Failed state (with retries exhausted) and any target state,
        /// attempting transition_task returns InvalidTransition{from: Failed, to: target}.
        ///
        /// **Validates: Requirements 3.7, 11.2**
        #[test]
        fn invalid_transition_from_failed(target in any_task_status_strategy()) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create task with max_retries=0, move to Running, then Failed
                let id = store
                    .create_task(CreateTaskParams {
                        description: "test task".to_string(),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create_task should succeed");

                store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await
                    .expect("Pending→Running should succeed");

                store
                    .transition_task(&id, TaskStatus::Failed, Some("error".to_string()))
                    .await
                    .expect("Running→Failed should succeed (retries exhausted)");

                // Now attempt any transition from Failed — should fail
                let result = store.transition_task(&id, target, None).await;
                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::InvalidTransition {
                        from: TaskStatus::Failed,
                        to: target,
                    })
                );

                Ok(())
            })?;
        }

        /// For any task in Killed state and any target state, attempting
        /// transition_task returns InvalidTransition{from: Killed, to: target}.
        ///
        /// **Validates: Requirements 3.7, 11.2**
        #[test]
        fn invalid_transition_from_killed(target in any_task_status_strategy()) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create task, then kill it directly from Pending
                let id = store
                    .create_task(CreateTaskParams {
                        description: "test task".to_string(),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create_task should succeed");

                store
                    .transition_task(&id, TaskStatus::Killed, None)
                    .await
                    .expect("Pending→Killed should succeed");

                // Now attempt any transition from Killed — should fail
                let result = store.transition_task(&id, target, None).await;
                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::InvalidTransition {
                        from: TaskStatus::Killed,
                        to: target,
                    })
                );

                Ok(())
            })?;
        }

        /// Attempting an invalid non-terminal transition (Pending→Completed)
        /// returns InvalidTransition{from: Pending, to: Completed}.
        ///
        /// **Validates: Requirements 3.7, 11.2**
        #[test]
        fn invalid_transition_pending_to_completed(_dummy in 0u8..1) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let id = store
                    .create_task(CreateTaskParams {
                        description: "test task".to_string(),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create_task should succeed");

                // Attempt Pending→Completed directly — should fail
                let result = store
                    .transition_task(&id, TaskStatus::Completed, None)
                    .await;
                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::InvalidTransition {
                        from: TaskStatus::Pending,
                        to: TaskStatus::Completed,
                    })
                );

                Ok(())
            })?;
        }

        /// Attempting an invalid non-terminal transition (Pending→Failed)
        /// returns InvalidTransition{from: Pending, to: Failed}.
        ///
        /// **Validates: Requirements 3.7, 11.2**
        #[test]
        fn invalid_transition_pending_to_failed(_dummy in 0u8..1) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let id = store
                    .create_task(CreateTaskParams {
                        description: "test task".to_string(),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create_task should succeed");

                // Attempt Pending→Failed directly — should fail
                let result = store
                    .transition_task(&id, TaskStatus::Failed, None)
                    .await;
                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::InvalidTransition {
                        from: TaskStatus::Pending,
                        to: TaskStatus::Failed,
                    })
                );

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 7: Status Filtering Correctness
    // **Validates: Requirements 5.5, 10.2, 10.5**
    // ═══════════════════════════════════════════════════════════════════════

    /// Strategy to generate a target final status for a task (0..5 maps to each variant).
    fn final_status_strategy() -> impl Strategy<Value = TaskStatus> {
        prop_oneof![
            Just(TaskStatus::Pending),
            Just(TaskStatus::Running),
            Just(TaskStatus::Completed),
            Just(TaskStatus::Failed),
            Just(TaskStatus::Killed),
        ]
    }

    /// Strategy to generate a vec of target statuses representing the desired
    /// final status for each task in the test.
    fn status_distribution_strategy() -> impl Strategy<Value = Vec<TaskStatus>> {
        prop_vec(final_status_strategy(), 5..20)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any set of tasks with a known distribution of statuses, calling
        /// `list_tasks(Some(status))` returns exactly and only the tasks with that
        /// status, and `count_by_status` returns counts matching the cardinality
        /// of each status group.
        ///
        /// **Validates: Requirements 5.5, 10.2, 10.5**
        #[test]
        fn status_filtering_correctness(target_statuses in status_distribution_strategy()) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Track expected counts per status
                let mut expected_pending: usize = 0;
                let mut expected_running: usize = 0;
                let mut expected_completed: usize = 0;
                let mut expected_failed: usize = 0;
                let mut expected_killed: usize = 0;

                for (i, target) in target_statuses.iter().enumerate() {
                    let params = CreateTaskParams {
                        description: format!("task-{}", i),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0, // 0 retries so Failed is terminal immediately
                    };
                    let id = store.create_task(params).await.expect("create_task should succeed");

                    // Transition the task to its designated final status
                    match target {
                        TaskStatus::Pending => {
                            // Already Pending after creation, nothing to do
                            expected_pending += 1;
                        }
                        TaskStatus::Running => {
                            // Pending → Running
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running should succeed");
                            expected_running += 1;
                        }
                        TaskStatus::Completed => {
                            // Pending → Running → Completed
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running should succeed");
                            store
                                .transition_task(&id, TaskStatus::Completed, Some(format!("output-{}", i)))
                                .await
                                .expect("Running→Completed should succeed");
                            expected_completed += 1;
                        }
                        TaskStatus::Failed => {
                            // Pending → Running → Failed (max_retries=0, so terminal)
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running should succeed");
                            store
                                .transition_task(&id, TaskStatus::Failed, Some(format!("error-{}", i)))
                                .await
                                .expect("Running→Failed should succeed");
                            expected_failed += 1;
                        }
                        TaskStatus::Killed => {
                            // Pending → Killed
                            store
                                .transition_task(&id, TaskStatus::Killed, None)
                                .await
                                .expect("Pending→Killed should succeed");
                            expected_killed += 1;
                        }
                    }
                }

                // Verify list_tasks filtering for each status
                let pending_tasks = store.list_tasks(Some(TaskStatus::Pending)).await.expect("list_tasks(Pending)");
                let running_tasks = store.list_tasks(Some(TaskStatus::Running)).await.expect("list_tasks(Running)");
                let completed_tasks = store.list_tasks(Some(TaskStatus::Completed)).await.expect("list_tasks(Completed)");
                let failed_tasks = store.list_tasks(Some(TaskStatus::Failed)).await.expect("list_tasks(Failed)");
                let killed_tasks = store.list_tasks(Some(TaskStatus::Killed)).await.expect("list_tasks(Killed)");

                prop_assert_eq!(pending_tasks.len(), expected_pending, "Pending count mismatch");
                prop_assert_eq!(running_tasks.len(), expected_running, "Running count mismatch");
                prop_assert_eq!(completed_tasks.len(), expected_completed, "Completed count mismatch");
                prop_assert_eq!(failed_tasks.len(), expected_failed, "Failed count mismatch");
                prop_assert_eq!(killed_tasks.len(), expected_killed, "Killed count mismatch");

                // Verify all returned tasks actually have the correct status
                for t in &pending_tasks {
                    prop_assert_eq!(t.status, TaskStatus::Pending);
                }
                for t in &running_tasks {
                    prop_assert_eq!(t.status, TaskStatus::Running);
                }
                for t in &completed_tasks {
                    prop_assert_eq!(t.status, TaskStatus::Completed);
                }
                for t in &failed_tasks {
                    prop_assert_eq!(t.status, TaskStatus::Failed);
                }
                for t in &killed_tasks {
                    prop_assert_eq!(t.status, TaskStatus::Killed);
                }

                // Verify list_tasks(None) returns all tasks
                let all_tasks = store.list_tasks(None).await.expect("list_tasks(None)");
                prop_assert_eq!(
                    all_tasks.len(),
                    target_statuses.len(),
                    "list_tasks(None) should return all tasks"
                );

                // Verify count_by_status matches
                let counts = store.count_by_status().await.expect("count_by_status");
                prop_assert_eq!(counts.pending, expected_pending, "count_by_status.pending mismatch");
                prop_assert_eq!(counts.running, expected_running, "count_by_status.running mismatch");
                prop_assert_eq!(counts.completed, expected_completed, "count_by_status.completed mismatch");
                prop_assert_eq!(counts.failed, expected_failed, "count_by_status.failed mismatch");
                prop_assert_eq!(counts.killed, expected_killed, "count_by_status.killed mismatch");

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 8: Acknowledge Excludes from Terminal Queries
    // **Validates: Requirements 10.3, 10.4, 5.4**
    // ═══════════════════════════════════════════════════════════════════════

    /// Strategy for choosing a terminal status for each task.
    fn terminal_status_strategy() -> impl Strategy<Value = TaskStatus> {
        prop_oneof![
            Just(TaskStatus::Completed),
            Just(TaskStatus::Failed),
            Just(TaskStatus::Killed),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any set of N tasks (3..10) moved to terminal states (mix of Completed,
        /// Failed, Killed), acknowledging a random subset causes
        /// `list_unacknowledged_terminal` to return only the non-acknowledged ones,
        /// while `get_task` still returns the acknowledged entries.
        ///
        /// **Validates: Requirements 10.3, 10.4, 5.4**
        #[test]
        fn acknowledge_excludes_from_terminal_queries(
            n in 3usize..10,
            terminal_statuses in prop_vec(terminal_status_strategy(), 3..10),
            acknowledge_flags in prop_vec(any::<bool>(), 3..10),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Use the minimum of n, terminal_statuses.len(), and acknowledge_flags.len()
                let count = n.min(terminal_statuses.len()).min(acknowledge_flags.len());

                // Create tasks and move them to terminal states
                let mut task_ids = Vec::with_capacity(count);
                for i in 0..count {
                    let params = CreateTaskParams {
                        description: format!("task-{}", i),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    };
                    let id = store.create_task(params).await.expect("create_task should succeed");

                    // Move to terminal state based on the generated strategy
                    match terminal_statuses[i] {
                        TaskStatus::Completed => {
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending->Running");
                            store
                                .transition_task(&id, TaskStatus::Completed, Some(format!("output-{}", i)))
                                .await
                                .expect("Running->Completed");
                        }
                        TaskStatus::Failed => {
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending->Running");
                            store
                                .transition_task(&id, TaskStatus::Failed, Some(format!("error-{}", i)))
                                .await
                                .expect("Running->Failed");
                        }
                        TaskStatus::Killed => {
                            store
                                .transition_task(&id, TaskStatus::Killed, None)
                                .await
                                .expect("Pending->Killed");
                        }
                        _ => unreachable!("terminal_status_strategy only produces terminal statuses"),
                    }

                    task_ids.push(id);
                }

                // Acknowledge a subset based on the random bool flags
                let mut acknowledged_ids: HashSet<String> = HashSet::new();
                for i in 0..count {
                    if acknowledge_flags[i] {
                        store
                            .acknowledge_task(&task_ids[i])
                            .await
                            .expect("acknowledge_task should succeed");
                        acknowledged_ids.insert(task_ids[i].clone());
                    }
                }

                // Verify list_unacknowledged_terminal excludes acknowledged tasks
                let unacked = store
                    .list_unacknowledged_terminal()
                    .await
                    .expect("list_unacknowledged_terminal should succeed");

                let unacked_ids: HashSet<String> = unacked.iter().map(|t| t.id.clone()).collect();

                // None of the acknowledged IDs should appear in unacknowledged list
                for acked_id in &acknowledged_ids {
                    prop_assert!(
                        !unacked_ids.contains(acked_id),
                        "Acknowledged task {} should NOT appear in list_unacknowledged_terminal",
                        acked_id
                    );
                }

                // All non-acknowledged task IDs should appear in unacknowledged list
                for i in 0..count {
                    if !acknowledged_ids.contains(&task_ids[i]) {
                        prop_assert!(
                            unacked_ids.contains(&task_ids[i]),
                            "Non-acknowledged task {} should appear in list_unacknowledged_terminal",
                            task_ids[i]
                        );
                    }
                }

                // Verify acknowledged tasks still exist via get_task
                for acked_id in &acknowledged_ids {
                    let entry = store
                        .get_task(acked_id)
                        .await
                        .expect("get_task should still return acknowledged task");
                    prop_assert_eq!(
                        entry.acknowledged,
                        true,
                        "Acknowledged task should have acknowledged=true"
                    );
                    prop_assert!(
                        entry.status.is_terminal(),
                        "Acknowledged task should still be in terminal state"
                    );
                }

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 9: Dependency Resolution Correctness
    // **Validates: Requirements 6.2, 6.3, 6.4, 6.5, 11.3**
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        /// Scenario 1: If a dependency is in Failed state, transitioning the
        /// dependent task to Running returns DependencyFailed error.
        ///
        /// **Validates: Requirements 6.2, 6.3, 11.3**
        #[test]
        fn dep_failed_returns_dependency_failed_error(
            dep_desc in "\\PC{1,30}",
            task_desc in "\\PC{1,30}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create dep task with max_retries=0, transition to Running then Failed
                let dep_id = store
                    .create_task(CreateTaskParams {
                        description: dep_desc,
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create dep task");

                store
                    .transition_task(&dep_id, TaskStatus::Running, None)
                    .await
                    .expect("dep Pending→Running");
                store
                    .transition_task(&dep_id, TaskStatus::Failed, Some("error".to_string()))
                    .await
                    .expect("dep Running→Failed");

                // Create task with that dep
                let task_id = store
                    .create_task(CreateTaskParams {
                        description: task_desc,
                        task_type: TaskType::Background,
                        dependencies: vec![dep_id.clone()],
                        max_retries: 0,
                    })
                    .await
                    .expect("create dependent task");

                // Attempt Pending→Running on dependent task
                let result = store
                    .transition_task(&task_id, TaskStatus::Running, None)
                    .await;

                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::DependencyFailed {
                        dependency_id: dep_id,
                        status: TaskStatus::Failed,
                    })
                );

                Ok(())
            })?;
        }

        /// Scenario 2: If a dependency is in Killed state, transitioning the
        /// dependent task to Running returns DependencyFailed error.
        ///
        /// **Validates: Requirements 6.2, 6.3, 11.3**
        #[test]
        fn dep_killed_returns_dependency_failed_error(
            dep_desc in "\\PC{1,30}",
            task_desc in "\\PC{1,30}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create dep task, kill it (Pending→Killed)
                let dep_id = store
                    .create_task(CreateTaskParams {
                        description: dep_desc,
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create dep task");

                store
                    .transition_task(&dep_id, TaskStatus::Killed, None)
                    .await
                    .expect("dep Pending→Killed");

                // Create task with that dep
                let task_id = store
                    .create_task(CreateTaskParams {
                        description: task_desc,
                        task_type: TaskType::Background,
                        dependencies: vec![dep_id.clone()],
                        max_retries: 0,
                    })
                    .await
                    .expect("create dependent task");

                // Attempt Pending→Running on dependent task
                let result = store
                    .transition_task(&task_id, TaskStatus::Running, None)
                    .await;

                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::DependencyFailed {
                        dependency_id: dep_id,
                        status: TaskStatus::Killed,
                    })
                );

                Ok(())
            })?;
        }

        /// Scenario 3: If a dependency is in Pending state (not completed),
        /// transitioning the dependent task to Running returns InvalidTransition.
        ///
        /// **Validates: Requirements 6.4, 6.5**
        #[test]
        fn dep_pending_returns_invalid_transition(
            dep_desc in "\\PC{1,30}",
            task_desc in "\\PC{1,30}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create dep task (stays Pending)
                let dep_id = store
                    .create_task(CreateTaskParams {
                        description: dep_desc,
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create dep task");

                // Create task with that dep
                let task_id = store
                    .create_task(CreateTaskParams {
                        description: task_desc,
                        task_type: TaskType::Background,
                        dependencies: vec![dep_id.clone()],
                        max_retries: 0,
                    })
                    .await
                    .expect("create dependent task");

                // Attempt Pending→Running on dependent task
                let result = store
                    .transition_task(&task_id, TaskStatus::Running, None)
                    .await;

                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::InvalidTransition {
                        from: TaskStatus::Pending,
                        to: TaskStatus::Running,
                    })
                );

                Ok(())
            })?;
        }

        /// Scenario 4: If a dependency is in Running state (not completed),
        /// transitioning the dependent task to Running returns InvalidTransition.
        ///
        /// **Validates: Requirements 6.4, 6.5**
        #[test]
        fn dep_running_returns_invalid_transition(
            dep_desc in "\\PC{1,30}",
            task_desc in "\\PC{1,30}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create dep task, transition to Running
                let dep_id = store
                    .create_task(CreateTaskParams {
                        description: dep_desc,
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    })
                    .await
                    .expect("create dep task");

                store
                    .transition_task(&dep_id, TaskStatus::Running, None)
                    .await
                    .expect("dep Pending→Running");

                // Create task with that dep
                let task_id = store
                    .create_task(CreateTaskParams {
                        description: task_desc,
                        task_type: TaskType::Background,
                        dependencies: vec![dep_id.clone()],
                        max_retries: 0,
                    })
                    .await
                    .expect("create dependent task");

                // Attempt Pending→Running on dependent task
                let result = store
                    .transition_task(&task_id, TaskStatus::Running, None)
                    .await;

                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::InvalidTransition {
                        from: TaskStatus::Pending,
                        to: TaskStatus::Running,
                    })
                );

                Ok(())
            })?;
        }

        /// Scenario 5: If all dependencies are Completed, transitioning the
        /// dependent task to Running succeeds.
        ///
        /// **Validates: Requirements 6.2, 6.5**
        #[test]
        fn all_deps_completed_transition_succeeds(
            num_deps in 1usize..5,
            task_desc in "\\PC{1,30}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                // Create multiple dep tasks and complete them all
                let mut dep_ids = Vec::new();
                for i in 0..num_deps {
                    let dep_id = store
                        .create_task(CreateTaskParams {
                            description: format!("dep-{}", i),
                            task_type: TaskType::Background,
                            dependencies: vec![],
                            max_retries: 0,
                        })
                        .await
                        .expect("create dep task");

                    store
                        .transition_task(&dep_id, TaskStatus::Running, None)
                        .await
                        .expect("dep Pending→Running");
                    store
                        .transition_task(&dep_id, TaskStatus::Completed, Some("done".to_string()))
                        .await
                        .expect("dep Running→Completed");

                    dep_ids.push(dep_id);
                }

                // Create task with those deps
                let task_id = store
                    .create_task(CreateTaskParams {
                        description: task_desc,
                        task_type: TaskType::Background,
                        dependencies: dep_ids,
                        max_retries: 0,
                    })
                    .await
                    .expect("create dependent task");

                // Attempt Pending→Running on dependent task — should succeed
                store
                    .transition_task(&task_id, TaskStatus::Running, None)
                    .await
                    .expect("Pending→Running with all deps completed should succeed");

                let task = store.get_task(&task_id).await.expect("get_task");
                prop_assert_eq!(task.status, TaskStatus::Running);

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 10: Retry Logic Semantics
    // **Validates: Requirements 7.3, 7.4, 7.5, 7.6**
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// For any task with max_retries = M > 0, repeatedly failing the task
        /// (via Pending→Running then Running→Failed cycles) increments retry_count
        /// and resets status to Pending until retries are exhausted, at which point
        /// the task reaches terminal Failed state with completed_at set. Any further
        /// transition from Failed returns InvalidTransition.
        ///
        /// **Validates: Requirements 7.3, 7.4, 7.5, 7.6**
        #[test]
        fn retry_logic_semantics(
            max_retries in 1u32..5,
            error_msg in "\\PC{1,50}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let params = CreateTaskParams {
                    description: "retryable task".to_string(),
                    task_type: TaskType::Background,
                    dependencies: vec![],
                    max_retries,
                };
                let id = store.create_task(params).await.expect("create_task should succeed");

                // Perform retry cycles: each cycle is Pending→Running, Running→Failed
                for retry in 0..max_retries {
                    // Task should be Pending at the start of each cycle
                    let task = store.get_task(&id).await.expect("get_task should succeed");
                    prop_assert_eq!(
                        task.status,
                        TaskStatus::Pending,
                        "Before retry cycle {}, task should be Pending",
                        retry
                    );

                    // Transition Pending → Running
                    store
                        .transition_task(&id, TaskStatus::Running, None)
                        .await
                        .expect("Pending→Running should succeed");

                    // Transition Running → Failed with error message
                    store
                        .transition_task(&id, TaskStatus::Failed, Some(error_msg.clone()))
                        .await
                        .expect("Running→Failed should succeed");

                    // After failure where retry_count < max_retries:
                    // status should be back to Pending, retry_count incremented
                    let task = store.get_task(&id).await.expect("get_task should succeed");
                    prop_assert_eq!(
                        task.retry_count,
                        retry + 1,
                        "After retry cycle {}, retry_count should be {}",
                        retry,
                        retry + 1
                    );
                    prop_assert_eq!(
                        task.last_error.as_deref(),
                        Some(error_msg.as_str()),
                        "After retry cycle {}, last_error should be set",
                        retry
                    );
                    prop_assert_eq!(
                        task.completed_at,
                        None,
                        "After retry cycle {}, completed_at should still be None",
                        retry
                    );
                    prop_assert_eq!(
                        task.status,
                        TaskStatus::Pending,
                        "After retry cycle {}, status should be Pending (retry reset)",
                        retry
                    );
                }

                // Final failure cycle: retries are now exhausted (retry_count == max_retries)
                // Pending → Running
                store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await
                    .expect("Final Pending→Running should succeed");

                // Running → Failed (this time retries exhausted)
                store
                    .transition_task(&id, TaskStatus::Failed, Some(error_msg.clone()))
                    .await
                    .expect("Final Running→Failed should succeed");

                // Verify terminal Failed state
                let task = store.get_task(&id).await.expect("get_task should succeed");
                prop_assert_eq!(task.status, TaskStatus::Failed, "Final status should be Failed (terminal)");
                prop_assert_eq!(task.retry_count, max_retries, "retry_count should equal max_retries");
                prop_assert_eq!(
                    task.last_error.as_deref(),
                    Some(error_msg.as_str()),
                    "last_error should be set on terminal failure"
                );
                prop_assert!(
                    task.completed_at.is_some(),
                    "completed_at should be Some on terminal failure"
                );
                prop_assert!(task.status.is_terminal(), "Failed should be a terminal state");

                // Verify any further transition from Failed returns InvalidTransition
                let result = store
                    .transition_task(&id, TaskStatus::Running, None)
                    .await;
                prop_assert_eq!(
                    result,
                    Err(TaskStoreError::InvalidTransition {
                        from: TaskStatus::Failed,
                        to: TaskStatus::Running,
                    }),
                    "Transition from terminal Failed should return InvalidTransition"
                );

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 12: Eviction Safety
    // **Validates: Requirements 13.1, 13.2, 13.3**
    // ═══════════════════════════════════════════════════════════════════════

    /// Strategy to generate a target status for each task in eviction tests.
    /// Includes all possible final states (Pending, Running, Completed, Failed, Killed).
    fn eviction_status_strategy() -> impl Strategy<Value = TaskStatus> {
        prop_oneof![
            Just(TaskStatus::Pending),
            Just(TaskStatus::Running),
            Just(TaskStatus::Completed),
            Just(TaskStatus::Failed),
            Just(TaskStatus::Killed),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        /// Scenario A: evict_acknowledged safety
        ///
        /// Create N tasks (5..15) with various states (some Pending, some Running,
        /// some terminal). Acknowledge a subset of the terminal tasks. Call
        /// `evict_acknowledged()`. Verify: only the acknowledged terminal tasks were
        /// removed. Non-terminal tasks (Pending, Running) are untouched.
        /// Unacknowledged terminal tasks are untouched. The returned count matches
        /// expected.
        ///
        /// **Validates: Requirements 13.1, 13.2, 13.3**
        #[test]
        fn evict_acknowledged_safety(
            statuses in prop_vec(eviction_status_strategy(), 5..15),
            ack_flags in prop_vec(any::<bool>(), 5..15),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let count = statuses.len().min(ack_flags.len());

                let mut task_ids: Vec<String> = Vec::with_capacity(count);
                let mut task_final_statuses: Vec<TaskStatus> = Vec::with_capacity(count);

                // Create tasks and move them to their designated final statuses
                for i in 0..count {
                    let params = CreateTaskParams {
                        description: format!("evict-test-{}", i),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    };
                    let id = store.create_task(params).await.expect("create_task should succeed");

                    match statuses[i] {
                        TaskStatus::Pending => {
                            // Already Pending
                        }
                        TaskStatus::Running => {
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running");
                        }
                        TaskStatus::Completed => {
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running");
                            store
                                .transition_task(&id, TaskStatus::Completed, Some("done".to_string()))
                                .await
                                .expect("Running→Completed");
                        }
                        TaskStatus::Failed => {
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running");
                            store
                                .transition_task(&id, TaskStatus::Failed, Some("error".to_string()))
                                .await
                                .expect("Running→Failed");
                        }
                        TaskStatus::Killed => {
                            store
                                .transition_task(&id, TaskStatus::Killed, None)
                                .await
                                .expect("Pending→Killed");
                        }
                    }

                    task_ids.push(id);
                    task_final_statuses.push(statuses[i]);
                }

                // Acknowledge a subset of terminal tasks (only acknowledge if terminal)
                let mut acknowledged_terminal_ids: HashSet<String> = HashSet::new();
                for i in 0..count {
                    if task_final_statuses[i].is_terminal() && ack_flags[i] {
                        store
                            .acknowledge_task(&task_ids[i])
                            .await
                            .expect("acknowledge_task should succeed");
                        acknowledged_terminal_ids.insert(task_ids[i].clone());
                    }
                }

                let expected_evict_count = acknowledged_terminal_ids.len();

                // Call evict_acknowledged
                let evicted = store
                    .evict_acknowledged()
                    .await
                    .expect("evict_acknowledged should succeed");

                // Verify returned count matches expected
                prop_assert_eq!(
                    evicted,
                    expected_evict_count,
                    "evict_acknowledged should return count of evicted tasks"
                );

                // Verify acknowledged terminal tasks are gone
                for acked_id in &acknowledged_terminal_ids {
                    let result = store.get_task(acked_id).await;
                    prop_assert_eq!(
                        result,
                        Err(TaskStoreError::NotFound { id: acked_id.clone() }),
                        "Acknowledged terminal task {} should have been evicted",
                        acked_id
                    );
                }

                // Verify non-terminal tasks still exist
                for i in 0..count {
                    if !task_final_statuses[i].is_terminal() {
                        let task = store
                            .get_task(&task_ids[i])
                            .await
                            .expect("Non-terminal task should not be evicted");
                        prop_assert_eq!(
                            task.status,
                            task_final_statuses[i],
                            "Non-terminal task {} should still have its original status",
                            task_ids[i]
                        );
                    }
                }

                // Verify unacknowledged terminal tasks still exist
                for i in 0..count {
                    if task_final_statuses[i].is_terminal()
                        && !acknowledged_terminal_ids.contains(&task_ids[i])
                    {
                        let task = store
                            .get_task(&task_ids[i])
                            .await
                            .expect("Unacknowledged terminal task should not be evicted");
                        prop_assert!(
                            task.status.is_terminal(),
                            "Unacknowledged terminal task {} should still be in terminal state",
                            task_ids[i]
                        );
                        prop_assert_eq!(
                            task.acknowledged, false,
                            "Unacknowledged terminal task {} should still have acknowledged=false",
                            task_ids[i]
                        );
                    }
                }

                Ok(())
            })?;
        }

        /// Scenario B: evict_older_than safety
        ///
        /// Create several tasks, immediately complete them, then call
        /// `evict_older_than(Duration::from_nanos(0))` — this should evict all
        /// terminal tasks since they were completed "more than 0 nanoseconds ago".
        /// Verify non-terminal tasks remain. Also test with a very large duration
        /// to verify nothing gets evicted.
        ///
        /// **Validates: Requirements 13.1, 13.2, 13.3**
        #[test]
        fn evict_older_than_safety(
            statuses in prop_vec(eviction_status_strategy(), 5..15),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = InMemoryTaskStore::new();

                let count = statuses.len();

                let mut task_ids: Vec<String> = Vec::with_capacity(count);
                let mut task_final_statuses: Vec<TaskStatus> = Vec::with_capacity(count);

                // Create tasks and move them to their designated final statuses
                for i in 0..count {
                    let params = CreateTaskParams {
                        description: format!("evict-older-test-{}", i),
                        task_type: TaskType::Background,
                        dependencies: vec![],
                        max_retries: 0,
                    };
                    let id = store.create_task(params).await.expect("create_task should succeed");

                    match statuses[i] {
                        TaskStatus::Pending => {
                            // Already Pending
                        }
                        TaskStatus::Running => {
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running");
                        }
                        TaskStatus::Completed => {
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running");
                            store
                                .transition_task(&id, TaskStatus::Completed, Some("done".to_string()))
                                .await
                                .expect("Running→Completed");
                        }
                        TaskStatus::Failed => {
                            store
                                .transition_task(&id, TaskStatus::Running, None)
                                .await
                                .expect("Pending→Running");
                            store
                                .transition_task(&id, TaskStatus::Failed, Some("error".to_string()))
                                .await
                                .expect("Running→Failed");
                        }
                        TaskStatus::Killed => {
                            store
                                .transition_task(&id, TaskStatus::Killed, None)
                                .await
                                .expect("Pending→Killed");
                        }
                    }

                    task_ids.push(id);
                    task_final_statuses.push(statuses[i]);
                }

                // First, test with a very large duration — nothing should be evicted
                let evicted_none = store
                    .evict_older_than(Duration::from_secs(3600))
                    .await
                    .expect("evict_older_than should succeed");
                prop_assert_eq!(
                    evicted_none, 0,
                    "With a very large duration, no recently-created tasks should be evicted"
                );

                // Verify all tasks still exist
                for i in 0..count {
                    store
                        .get_task(&task_ids[i])
                        .await
                        .expect("All tasks should still exist after large-duration eviction");
                }

                // Now sleep a tiny bit and evict with Duration::from_nanos(0)
                // This should evict all terminal tasks since their completed_at
                // is at least 1 nanosecond old.
                tokio::time::sleep(Duration::from_millis(1)).await;

                let terminal_count = task_final_statuses.iter().filter(|s| s.is_terminal()).count();
                let non_terminal_count = count - terminal_count;

                let evicted_all = store
                    .evict_older_than(Duration::from_nanos(0))
                    .await
                    .expect("evict_older_than should succeed");
                prop_assert_eq!(
                    evicted_all,
                    terminal_count,
                    "With Duration::from_nanos(0), all terminal tasks should be evicted"
                );

                // Verify non-terminal tasks still exist
                for i in 0..count {
                    if !task_final_statuses[i].is_terminal() {
                        let task = store
                            .get_task(&task_ids[i])
                            .await
                            .expect("Non-terminal task should not be evicted");
                        prop_assert_eq!(
                            task.status,
                            task_final_statuses[i],
                            "Non-terminal task {} should still have its original status",
                            task_ids[i]
                        );
                    }
                }

                // Verify terminal tasks were evicted
                for i in 0..count {
                    if task_final_statuses[i].is_terminal() {
                        let result = store.get_task(&task_ids[i]).await;
                        prop_assert_eq!(
                            result,
                            Err(TaskStoreError::NotFound { id: task_ids[i].clone() }),
                            "Terminal task {} should have been evicted",
                            task_ids[i]
                        );
                    }
                }

                // Verify remaining task count
                let remaining = store.list_tasks(None).await.expect("list_tasks should succeed");
                prop_assert_eq!(
                    remaining.len(),
                    non_terminal_count,
                    "Only non-terminal tasks should remain after eviction"
                );

                Ok(())
            })?;
        }
    }
}
