//! TodoListTool: An LLM-facing tool for creating, updating, listing, and
//! removing planning items via the TaskStore.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::error::ToolError;
use crate::task_store::{TaskStore, TaskStoreError, TodoStatus};
use crate::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// An LLM-facing tool that exposes CRUD operations on `TodoItem` entries,
/// enabling agents to maintain visible planning lists.
pub struct TodoListTool {
    store: Arc<dyn TaskStore>,
}

impl TodoListTool {
    /// Create a new `TodoListTool` backed by the given `TaskStore`.
    pub fn new(store: Arc<dyn TaskStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TodoListTool {
    fn name(&self) -> &str {
        "todolist"
    }

    fn description(&self) -> &str {
        "Track multi-step plans and record progress on tasks. Use this tool to create, update, \
         list, and remove planning items visible to the user. Available actions: 'add' to create \
         a new item, 'update' to change an item's status, 'list' to see all items, 'remove' to \
         delete an item, 'clear_completed' to remove all completed items."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "update", "list", "remove", "clear_completed"],
                    "description": "The action to perform"
                },
                "content": {
                    "type": "string",
                    "maxLength": 1000,
                    "description": "Content for the todo item (required for 'add')"
                },
                "active_form": {
                    "type": "string",
                    "maxLength": 200,
                    "description": "Short active display text (optional for 'add')"
                },
                "id": {
                    "type": "string",
                    "description": "ID of the todo item (required for 'update' and 'remove')"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "New status (required for 'update')"
                }
            },
            "required": ["action"]
        })
    }

    fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
        Concurrency::Safe
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidInput(
                    "Missing required parameter 'action'. Valid actions: add, update, list, remove, clear_completed".to_string(),
                )
            })?;

        match action {
            "add" => self.handle_add(&input).await,
            "update" => self.handle_update(&input).await,
            "list" => self.handle_list().await,
            "remove" => self.handle_remove(&input).await,
            "clear_completed" => self.handle_clear_completed().await,
            _ => Err(ToolError::InvalidInput(format!(
                "Unknown action '{}'. Valid actions: add, update, list, remove, clear_completed",
                action
            ))),
        }
    }
}

impl TodoListTool {
    async fn handle_add(&self, input: &serde_json::Value) -> Result<ToolOutput, ToolError> {
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidInput(
                    "Missing required parameter 'content' for action 'add'".to_string(),
                )
            })?;

        if content.len() > 1000 {
            return Err(ToolError::InvalidInput(format!(
                "Parameter 'content' exceeds maximum length of 1000 characters (got {})",
                content.len()
            )));
        }

        let active_form = input.get("active_form").and_then(|v| v.as_str());

        if let Some(af) = active_form {
            if af.len() > 200 {
                return Err(ToolError::InvalidInput(format!(
                    "Parameter 'active_form' exceeds maximum length of 200 characters (got {})",
                    af.len()
                )));
            }
        }

        let id = self
            .store
            .add_todo(content.to_string(), active_form.map(|s| s.to_string()))
            .await
            .map_err(map_store_error)?;

        Ok(ToolOutput::Text(id))
    }

    async fn handle_update(&self, input: &serde_json::Value) -> Result<ToolOutput, ToolError> {
        let id = input.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidInput(
                "Missing required parameter 'id' for action 'update'".to_string(),
            )
        })?;

        let status_str = input
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidInput(
                    "Missing required parameter 'status' for action 'update'".to_string(),
                )
            })?;

        let status = parse_todo_status(status_str)?;

        self.store
            .update_todo_status(id, status)
            .await
            .map_err(map_store_error)?;

        Ok(ToolOutput::Text(format!(
            "Updated item '{}' to status '{}'",
            id, status_str
        )))
    }

    async fn handle_list(&self) -> Result<ToolOutput, ToolError> {
        let items = self.store.list_todos().await.map_err(map_store_error)?;

        let json_items: Vec<serde_json::Value> = items
            .iter()
            .map(|item| {
                json!({
                    "id": item.id,
                    "content": item.content,
                    "status": match item.status {
                        TodoStatus::Pending => "pending",
                        TodoStatus::InProgress => "in_progress",
                        TodoStatus::Completed => "completed",
                    },
                    "active_form": item.active_form,
                })
            })
            .collect();

        let output = serde_json::to_string(&json_items)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to serialize list: {}", e)))?;

        Ok(ToolOutput::Text(output))
    }

    async fn handle_remove(&self, input: &serde_json::Value) -> Result<ToolOutput, ToolError> {
        let id = input.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidInput(
                "Missing required parameter 'id' for action 'remove'".to_string(),
            )
        })?;

        self.store.remove_todo(id).await.map_err(map_store_error)?;

        Ok(ToolOutput::Text(format!("Removed item '{}'", id)))
    }

    async fn handle_clear_completed(&self) -> Result<ToolOutput, ToolError> {
        let count = self
            .store
            .clear_completed_todos()
            .await
            .map_err(map_store_error)?;

        Ok(ToolOutput::Text(format!(
            "Cleared {} completed item{}",
            count,
            if count == 1 { "" } else { "s" }
        )))
    }
}

/// Parse a status string into a `TodoStatus`.
fn parse_todo_status(s: &str) -> Result<TodoStatus, ToolError> {
    match s {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" => Ok(TodoStatus::InProgress),
        "completed" => Ok(TodoStatus::Completed),
        _ => Err(ToolError::InvalidInput(format!(
            "Invalid status '{}'. Valid statuses: pending, in_progress, completed",
            s
        ))),
    }
}

/// Map a `TaskStoreError` to the appropriate `ToolError`.
fn map_store_error(err: TaskStoreError) -> ToolError {
    match err {
        TaskStoreError::NotFound { id } => {
            ToolError::ExecutionFailed(format!("Item not found: '{}'", id))
        }
        TaskStoreError::StorageError { message } => {
            ToolError::ExecutionFailed(format!("Storage error: {}", message))
        }
        other => ToolError::ExecutionFailed(format!("TaskStore error: {}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryTaskStore;
    use proptest::prelude::*;
    use std::path::PathBuf;

    fn make_store() -> Arc<dyn TaskStore> {
        Arc::new(InMemoryTaskStore::new())
    }

    fn make_context() -> ToolContext {
        ToolContext {
            session_id: "test-session".to_string(),
            working_dir: PathBuf::from("/tmp/test"),
        }
    }

    // ── Strategies for property tests ───────────────────────────────────

    /// Generate valid content strings (1..=1000 chars, non-empty printable ASCII).
    fn content_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9 _.,-]{1,1000}").unwrap()
    }

    /// Generate valid active_form strings (1..=200 chars).
    fn active_form_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9 _.,-]{1,200}").unwrap()
    }

    /// Generate a valid todo status string.
    fn status_str_strategy() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("pending"), Just("in_progress"), Just("completed"),]
    }

    /// Generate an invalid action string (not one of the valid actions).
    fn invalid_action_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-z]{1,20}")
            .unwrap()
            .prop_filter("must not be a valid action", |s| {
                !matches!(
                    s.as_str(),
                    "add" | "update" | "list" | "remove" | "clear_completed"
                )
            })
    }

    /// Generate a non-existent ID string (UUID-like but guaranteed not in store).
    fn nonexistent_id_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("missing-[a-f0-9]{8}").unwrap()
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 3: TodoListTool add round-trip
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 3: TodoListTool add round-trip
        /// For any valid content/active_form, add returns an ID and get_todo matches.
        ///
        /// **Validates: Requirements 2.3**
        #[test]
        fn prop_add_round_trip(
            content in content_strategy(),
            active_form in proptest::option::of(active_form_strategy()),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = make_store();
                let tool = TodoListTool::new(store.clone());
                let ctx = make_context();

                let mut input = json!({"action": "add", "content": content});
                if let Some(ref af) = active_form {
                    input["active_form"] = json!(af);
                }

                let result = tool.execute(input, &ctx).await.unwrap();
                let id = match result {
                    ToolOutput::Text(id) => id,
                    other => panic!("Expected Text, got {:?}", other),
                };

                // ID must be non-empty
                prop_assert!(!id.is_empty());

                // get_todo must return matching item
                let item = store.get_todo(&id).await.unwrap();
                prop_assert_eq!(&item.content, &content);
                prop_assert_eq!(&item.active_form, &active_form);
                prop_assert_eq!(item.status, TodoStatus::Pending);

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 4: TodoListTool update confirmation
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 4: TodoListTool update confirmation
        /// For any existing item and valid status, update returns confirmation and store reflects new status.
        ///
        /// **Validates: Requirements 2.4**
        #[test]
        fn prop_update_confirmation(
            content in content_strategy(),
            status in status_str_strategy(),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = make_store();
                let tool = TodoListTool::new(store.clone());
                let ctx = make_context();

                // First add an item
                let id = store.add_todo(content, None).await.unwrap();

                // Now update it
                let result = tool
                    .execute(json!({"action": "update", "id": id, "status": status}), &ctx)
                    .await
                    .unwrap();

                // Result should contain the ID and status
                match result {
                    ToolOutput::Text(msg) => {
                        prop_assert!(msg.contains(&id));
                        prop_assert!(msg.contains(status));
                    }
                    other => panic!("Expected Text, got {:?}", other),
                }

                // Store should reflect the new status
                let item = store.get_todo(&id).await.unwrap();
                let expected_status = match status {
                    "pending" => TodoStatus::Pending,
                    "in_progress" => TodoStatus::InProgress,
                    "completed" => TodoStatus::Completed,
                    _ => unreachable!(),
                };
                prop_assert_eq!(item.status, expected_status);

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 5: TodoListTool list completeness
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 5: TodoListTool list completeness
        /// For N added items, list returns exactly N with matching fields.
        ///
        /// **Validates: Requirements 2.5**
        #[test]
        fn prop_list_completeness(
            contents in proptest::collection::vec(content_strategy(), 1..10),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = make_store();
                let tool = TodoListTool::new(store.clone());
                let ctx = make_context();

                // Add N items
                let n = contents.len();
                for c in &contents {
                    store.add_todo(c.clone(), None).await.unwrap();
                }

                // List via tool
                let result = tool.execute(json!({"action": "list"}), &ctx).await.unwrap();
                match result {
                    ToolOutput::Text(json_str) => {
                        let items: Vec<serde_json::Value> =
                            serde_json::from_str(&json_str).unwrap();
                        prop_assert_eq!(items.len(), n);

                        // Each item should have matching content
                        for (i, c) in contents.iter().enumerate() {
                            prop_assert_eq!(items[i]["content"].as_str().unwrap(), c.as_str());
                            prop_assert_eq!(items[i]["status"].as_str().unwrap(), "pending");
                            // id must be present and non-empty
                            prop_assert!(!items[i]["id"].as_str().unwrap().is_empty());
                        }
                    }
                    other => panic!("Expected Text, got {:?}", other),
                }

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 6: TodoListTool remove round-trip
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 6: TodoListTool remove round-trip
        /// After remove, get_todo returns NotFound.
        ///
        /// **Validates: Requirements 2.6**
        #[test]
        fn prop_remove_round_trip(
            content in content_strategy(),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = make_store();
                let tool = TodoListTool::new(store.clone());
                let ctx = make_context();

                // Add item
                let id = store.add_todo(content, None).await.unwrap();

                // Remove via tool
                let result = tool
                    .execute(json!({"action": "remove", "id": id}), &ctx)
                    .await
                    .unwrap();

                // Result should contain the ID
                match result {
                    ToolOutput::Text(msg) => {
                        prop_assert!(msg.contains(&id));
                    }
                    other => panic!("Expected Text, got {:?}", other),
                }

                // get_todo should return NotFound
                let get_result = store.get_todo(&id).await;
                prop_assert!(get_result.is_err());
                match get_result.unwrap_err() {
                    TaskStoreError::NotFound { id: err_id } => {
                        prop_assert_eq!(err_id, id);
                    }
                    other => panic!("Expected NotFound, got {:?}", other),
                }

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 7: TodoListTool clear_completed correctness
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 7: TodoListTool clear_completed correctness
        /// clear_completed removes exactly completed items.
        ///
        /// **Validates: Requirements 2.7**
        #[test]
        fn prop_clear_completed_correctness(
            statuses in proptest::collection::vec(status_str_strategy(), 1..10),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let store = make_store();
                let tool = TodoListTool::new(store.clone());
                let ctx = make_context();

                // Add items and set their statuses
                let completed_count = statuses.iter().filter(|&&s| s == "completed").count();
                let non_completed_count = statuses.len() - completed_count;

                for (i, &status_str) in statuses.iter().enumerate() {
                    let id = store.add_todo(format!("item-{}", i), None).await.unwrap();
                    let status = match status_str {
                        "pending" => TodoStatus::Pending,
                        "in_progress" => TodoStatus::InProgress,
                        "completed" => TodoStatus::Completed,
                        _ => unreachable!(),
                    };
                    if status != TodoStatus::Pending {
                        store.update_todo_status(&id, status).await.unwrap();
                    }
                }

                // clear_completed via tool
                let result = tool
                    .execute(json!({"action": "clear_completed"}), &ctx)
                    .await
                    .unwrap();

                // Result should contain the count
                match result {
                    ToolOutput::Text(msg) => {
                        prop_assert!(msg.contains(&completed_count.to_string()));
                    }
                    other => panic!("Expected Text, got {:?}", other),
                }

                // Remaining items should all be non-completed
                let remaining = store.list_todos().await.unwrap();
                prop_assert_eq!(remaining.len(), non_completed_count);
                for item in &remaining {
                    prop_assert_ne!(item.status, TodoStatus::Completed);
                }

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 8: TodoListTool invalid action error
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 8: TodoListTool invalid action error
        /// Unknown actions return ToolError listing valid actions.
        ///
        /// **Validates: Requirements 2.8**
        #[test]
        fn prop_invalid_action_error(
            action in invalid_action_strategy(),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let tool = TodoListTool::new(make_store());
                let ctx = make_context();

                let result = tool
                    .execute(json!({"action": action}), &ctx)
                    .await;

                prop_assert!(result.is_err());
                match result.unwrap_err() {
                    ToolError::InvalidInput(msg) => {
                        prop_assert!(msg.contains(&action));
                        prop_assert!(msg.contains("add"));
                        prop_assert!(msg.contains("update"));
                        prop_assert!(msg.contains("list"));
                        prop_assert!(msg.contains("remove"));
                        prop_assert!(msg.contains("clear_completed"));
                    }
                    other => panic!("Expected InvalidInput, got {:?}", other),
                }

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 9: TodoListTool NotFound propagation
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 9: TodoListTool NotFound propagation
        /// Operations on missing IDs return ToolError with the ID.
        ///
        /// **Validates: Requirements 2.9**
        #[test]
        fn prop_not_found_propagation(
            missing_id in nonexistent_id_strategy(),
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let tool = TodoListTool::new(make_store());
                let ctx = make_context();

                // update with missing ID
                let update_result = tool
                    .execute(json!({"action": "update", "id": missing_id, "status": "completed"}), &ctx)
                    .await;
                prop_assert!(update_result.is_err());
                match update_result.unwrap_err() {
                    ToolError::ExecutionFailed(msg) => {
                        prop_assert!(msg.contains(&missing_id));
                    }
                    other => panic!("Expected ExecutionFailed for update, got {:?}", other),
                }

                // remove with missing ID
                let remove_result = tool
                    .execute(json!({"action": "remove", "id": missing_id}), &ctx)
                    .await;
                prop_assert!(remove_result.is_err());
                match remove_result.unwrap_err() {
                    ToolError::ExecutionFailed(msg) => {
                        prop_assert!(msg.contains(&missing_id));
                    }
                    other => panic!("Expected ExecutionFailed for remove, got {:?}", other),
                }

                Ok(())
            })?;
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Property 10: TodoListTool missing parameter error
    // ═══════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 10: TodoListTool missing parameter error
        /// Missing required params return ToolError naming the param.
        ///
        /// **Validates: Requirements 2.10**
        #[test]
        fn prop_missing_parameter_error(
            // Use a dummy value to drive proptest iteration (we test all missing-param combos)
            scenario in 0u8..4,
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let tool = TodoListTool::new(make_store());
                let ctx = make_context();

                let (input, expected_param) = match scenario {
                    0 => {
                        // add without content
                        (json!({"action": "add"}), "content")
                    }
                    1 => {
                        // update without id
                        (json!({"action": "update", "status": "pending"}), "id")
                    }
                    2 => {
                        // update without status
                        (json!({"action": "update", "id": "some-id"}), "status")
                    }
                    3 => {
                        // remove without id
                        (json!({"action": "remove"}), "id")
                    }
                    _ => unreachable!(),
                };

                let result = tool.execute(input, &ctx).await;
                prop_assert!(result.is_err());
                match result.unwrap_err() {
                    ToolError::InvalidInput(msg) => {
                        prop_assert!(
                            msg.contains(expected_param),
                            "Error message '{}' should contain param name '{}'",
                            msg, expected_param
                        );
                    }
                    other => panic!("Expected InvalidInput, got {:?}", other),
                }

                Ok(())
            })?;
        }
    }

    #[test]
    fn name_returns_todolist() {
        let tool = TodoListTool::new(make_store());
        assert_eq!(tool.name(), "todolist");
    }

    #[test]
    fn concurrency_returns_safe() {
        let tool = TodoListTool::new(make_store());
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Safe);
    }

    #[test]
    fn description_mentions_tracking_plans() {
        let tool = TodoListTool::new(make_store());
        let desc = tool.description();
        assert!(desc.contains("Track multi-step plans"));
        assert!(desc.contains("add"));
        assert!(desc.contains("update"));
        assert!(desc.contains("list"));
        assert!(desc.contains("remove"));
        assert!(desc.contains("clear_completed"));
    }

    #[test]
    fn parameters_schema_has_action_enum() {
        let tool = TodoListTool::new(make_store());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["action"]));

        let action_enum = &schema["properties"]["action"]["enum"];
        assert_eq!(
            action_enum,
            &json!(["add", "update", "list", "remove", "clear_completed"])
        );
    }

    #[tokio::test]
    async fn add_returns_id() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool
            .execute(json!({"action": "add", "content": "Test item"}), &ctx)
            .await;
        assert!(result.is_ok());
        match result.unwrap() {
            ToolOutput::Text(id) => assert!(!id.is_empty()),
            other => panic!("Expected Text, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn add_with_active_form() {
        let store = make_store();
        let tool = TodoListTool::new(store.clone());
        let ctx = make_context();
        let result = tool
            .execute(
                json!({"action": "add", "content": "Full description", "active_form": "Short"}),
                &ctx,
            )
            .await
            .unwrap();
        match result {
            ToolOutput::Text(id) => {
                let item = store.get_todo(&id).await.unwrap();
                assert_eq!(item.content, "Full description");
                assert_eq!(item.active_form, Some("Short".to_string()));
            }
            other => panic!("Expected Text, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn add_missing_content_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool.execute(json!({"action": "add"}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("content")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn add_content_too_long_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let long_content = "x".repeat(1001);
        let result = tool
            .execute(json!({"action": "add", "content": long_content}), &ctx)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("1000")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn add_active_form_too_long_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let long_af = "x".repeat(201);
        let result = tool
            .execute(
                json!({"action": "add", "content": "ok", "active_form": long_af}),
                &ctx,
            )
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("200")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn update_changes_status() {
        let store = make_store();
        let tool = TodoListTool::new(store.clone());
        let ctx = make_context();

        let id = store.add_todo("item".to_string(), None).await.unwrap();

        let result = tool
            .execute(
                json!({"action": "update", "id": id, "status": "in_progress"}),
                &ctx,
            )
            .await
            .unwrap();

        match result {
            ToolOutput::Text(msg) => {
                assert!(msg.contains(&id));
                assert!(msg.contains("in_progress"));
            }
            other => panic!("Expected Text, got {:?}", other),
        }

        let item = store.get_todo(&id).await.unwrap();
        assert_eq!(item.status, TodoStatus::InProgress);
    }

    #[tokio::test]
    async fn update_missing_id_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool
            .execute(json!({"action": "update", "status": "completed"}), &ctx)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("id")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn update_missing_status_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool
            .execute(json!({"action": "update", "id": "some-id"}), &ctx)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("status")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn update_invalid_status_returns_error() {
        let store = make_store();
        let tool = TodoListTool::new(store.clone());
        let ctx = make_context();

        let id = store.add_todo("item".to_string(), None).await.unwrap();

        let result = tool
            .execute(
                json!({"action": "update", "id": id, "status": "invalid"}),
                &ctx,
            )
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("invalid"));
                assert!(msg.contains("pending"));
                assert!(msg.contains("in_progress"));
                assert!(msg.contains("completed"));
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn update_nonexistent_id_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool
            .execute(
                json!({"action": "update", "id": "nonexistent-id", "status": "completed"}),
                &ctx,
            )
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => assert!(msg.contains("nonexistent-id")),
            other => panic!("Expected ExecutionFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn list_returns_all_items() {
        let store = make_store();
        let tool = TodoListTool::new(store.clone());
        let ctx = make_context();

        store.add_todo("first".to_string(), None).await.unwrap();
        store
            .add_todo("second".to_string(), Some("s2".to_string()))
            .await
            .unwrap();

        let result = tool.execute(json!({"action": "list"}), &ctx).await.unwrap();
        match result {
            ToolOutput::Text(json_str) => {
                let items: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap();
                assert_eq!(items.len(), 2);
                assert_eq!(items[0]["content"], "first");
                assert_eq!(items[1]["content"], "second");
                assert_eq!(items[1]["active_form"], "s2");
            }
            other => panic!("Expected Text, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn list_empty_returns_empty_array() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool.execute(json!({"action": "list"}), &ctx).await.unwrap();
        match result {
            ToolOutput::Text(json_str) => {
                let items: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap();
                assert_eq!(items.len(), 0);
            }
            other => panic!("Expected Text, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn remove_deletes_item() {
        let store = make_store();
        let tool = TodoListTool::new(store.clone());
        let ctx = make_context();

        let id = store.add_todo("to remove".to_string(), None).await.unwrap();

        let result = tool
            .execute(json!({"action": "remove", "id": id}), &ctx)
            .await
            .unwrap();
        match result {
            ToolOutput::Text(msg) => assert!(msg.contains(&id)),
            other => panic!("Expected Text, got {:?}", other),
        }

        let get_result = store.get_todo(&id).await;
        assert!(get_result.is_err());
    }

    #[tokio::test]
    async fn remove_missing_id_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool.execute(json!({"action": "remove"}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("id")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn remove_nonexistent_id_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool
            .execute(json!({"action": "remove", "id": "ghost-id"}), &ctx)
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::ExecutionFailed(msg) => assert!(msg.contains("ghost-id")),
            other => panic!("Expected ExecutionFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn clear_completed_removes_only_completed() {
        let store = make_store();
        let tool = TodoListTool::new(store.clone());
        let ctx = make_context();

        let id1 = store.add_todo("a".to_string(), None).await.unwrap();
        store.add_todo("b".to_string(), None).await.unwrap();
        store
            .update_todo_status(&id1, TodoStatus::Completed)
            .await
            .unwrap();

        let result = tool
            .execute(json!({"action": "clear_completed"}), &ctx)
            .await
            .unwrap();
        match result {
            ToolOutput::Text(msg) => assert!(msg.contains("1")),
            other => panic!("Expected Text, got {:?}", other),
        }

        let remaining = store.list_todos().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].content, "b");
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool.execute(json!({"action": "destroy"}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("destroy"));
                assert!(msg.contains("add"));
                assert!(msg.contains("update"));
                assert!(msg.contains("list"));
                assert!(msg.contains("remove"));
                assert!(msg.contains("clear_completed"));
            }
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn missing_action_returns_error() {
        let tool = TodoListTool::new(make_store());
        let ctx = make_context();
        let result = tool.execute(json!({}), &ctx).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("action")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }
}
