// Slash command parsing and execution for the TUI.
//
// Handles `/tasks`, `/tasks ack`, `/tasks clear`, and `/todos` commands
// that let users inspect background task and todo state without involving the LLM.

use agent_core::{TaskStatus, TodoStatus};

use super::app::{AppState, OutputSpan, SpanStyle, UpdateResult};

/// Available slash commands listed in error messages.
const AVAILABLE_COMMANDS: &str = "Available commands: /tasks, /tasks ack, /tasks clear, /todos";

/// Parse and execute a slash command.
///
/// Returns `Some(UpdateResult)` if the input was handled as a slash command,
/// or `None` if the input does not start with `/` (and should be processed normally).
pub fn try_handle_slash_command(input: &str, state: &mut AppState) -> Option<UpdateResult> {
    // Only treat inputs starting with '/' as commands
    if !input.starts_with('/') {
        return None;
    }

    // Strip the '/' prefix and trim whitespace
    let command_body = input[1..].trim();

    match command_body {
        "tasks" => {
            handle_tasks_list(state);
            Some(UpdateResult::Continue)
        }
        "tasks ack" => {
            handle_tasks_ack(state);
            Some(UpdateResult::Continue)
        }
        "tasks clear" => {
            handle_tasks_clear(state);
            Some(UpdateResult::Continue)
        }
        "todos" => {
            handle_todos_list(state);
            Some(UpdateResult::Continue)
        }
        _ => {
            // Unknown command
            state.output_buffer.push(OutputSpan {
                text: format!("Unknown command: /{}. {}", command_body, AVAILABLE_COMMANDS),
                style: SpanStyle::Error,
            });
            Some(UpdateResult::Continue)
        }
    }
}

/// List non-acknowledged tasks grouped by status (Running, Pending, Completed, Failed).
fn handle_tasks_list(state: &mut AppState) {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => {
            state.output_buffer.push(OutputSpan {
                text: "Task store not available.".to_string(),
                style: SpanStyle::Error,
            });
            return;
        }
    };

    let tasks = match block_on_async(store.list_unacknowledged_terminal()) {
        Ok(mut terminal_tasks) => {
            // Also get running and pending tasks
            let running = block_on_async(store.list_tasks(Some(TaskStatus::Running)))
                .unwrap_or_default();
            let pending = block_on_async(store.list_tasks(Some(TaskStatus::Pending)))
                .unwrap_or_default();

            let mut all_tasks = Vec::new();
            all_tasks.extend(running);
            all_tasks.extend(pending);
            all_tasks.append(&mut terminal_tasks);
            all_tasks
        }
        Err(e) => {
            state.output_buffer.push(OutputSpan {
                text: format!("Error listing tasks: {}", e),
                style: SpanStyle::Error,
            });
            return;
        }
    };

    if tasks.is_empty() {
        state.output_buffer.push(OutputSpan {
            text: "No tasks.".to_string(),
            style: SpanStyle::System,
        });
        return;
    }

    // Group by status in the specified order: Running, Pending, Completed, Failed
    let groups: &[(TaskStatus, &str)] = &[
        (TaskStatus::Running, "Running"),
        (TaskStatus::Pending, "Pending"),
        (TaskStatus::Completed, "Completed"),
        (TaskStatus::Failed, "Failed"),
    ];

    let mut output_lines = Vec::new();

    for (status, label) in groups {
        let group_tasks: Vec<_> = tasks.iter().filter(|t| t.status == *status).collect();
        if !group_tasks.is_empty() {
            output_lines.push(format!("── {} ──", label));
            for task in group_tasks {
                output_lines.push(format!("  • {}", task.description));
            }
        }
    }

    state.output_buffer.push(OutputSpan {
        text: output_lines.join("\n"),
        style: SpanStyle::System,
    });
}

/// Acknowledge all unacknowledged terminal tasks and display the count.
fn handle_tasks_ack(state: &mut AppState) {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => {
            state.output_buffer.push(OutputSpan {
                text: "Task store not available.".to_string(),
                style: SpanStyle::Error,
            });
            return;
        }
    };

    let tasks = match block_on_async(store.list_unacknowledged_terminal()) {
        Ok(t) => t,
        Err(e) => {
            state.output_buffer.push(OutputSpan {
                text: format!("Error listing tasks: {}", e),
                style: SpanStyle::Error,
            });
            return;
        }
    };

    let count = tasks.len();
    for task in &tasks {
        if let Err(e) = block_on_async(store.acknowledge_task(&task.id)) {
            state.output_buffer.push(OutputSpan {
                text: format!("Error acknowledging task {}: {}", task.id, e),
                style: SpanStyle::Error,
            });
            return;
        }
    }

    state.output_buffer.push(OutputSpan {
        text: format!("Acknowledged {} task(s).", count),
        style: SpanStyle::System,
    });
}

/// Evict acknowledged terminal tasks and display the count.
fn handle_tasks_clear(state: &mut AppState) {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => {
            state.output_buffer.push(OutputSpan {
                text: "Task store not available.".to_string(),
                style: SpanStyle::Error,
            });
            return;
        }
    };

    match block_on_async(store.evict_acknowledged()) {
        Ok(count) => {
            state.output_buffer.push(OutputSpan {
                text: format!("Cleared {} task(s).", count),
                style: SpanStyle::System,
            });
        }
        Err(e) => {
            state.output_buffer.push(OutputSpan {
                text: format!("Error clearing tasks: {}", e),
                style: SpanStyle::Error,
            });
        }
    }
}

/// List all todo items with status and content.
fn handle_todos_list(state: &mut AppState) {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => {
            state.output_buffer.push(OutputSpan {
                text: "Task store not available.".to_string(),
                style: SpanStyle::Error,
            });
            return;
        }
    };

    match block_on_async(store.list_todos()) {
        Ok(items) => {
            if items.is_empty() {
                state.output_buffer.push(OutputSpan {
                    text: "No todo items.".to_string(),
                    style: SpanStyle::System,
                });
                return;
            }

            let mut output_lines = Vec::new();
            for item in &items {
                let status_indicator = match item.status {
                    TodoStatus::Pending => "[ ]",
                    TodoStatus::InProgress => "[~]",
                    TodoStatus::Completed => "[x]",
                };
                output_lines.push(format!("  {} {}", status_indicator, item.content));
            }

            state.output_buffer.push(OutputSpan {
                text: output_lines.join("\n"),
                style: SpanStyle::System,
            });
        }
        Err(e) => {
            state.output_buffer.push(OutputSpan {
                text: format!("Error listing todos: {}", e),
                style: SpanStyle::Error,
            });
        }
    }
}

/// Helper to call an async function from synchronous context.
///
/// Uses `tokio::task::block_in_place` with the current runtime handle to avoid
/// blocking the async executor.
fn block_on_async<F: std::future::Future>(f: F) -> F::Output {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(f)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use agent_core::{InMemoryTaskStore, CreateTaskParams, TaskStore, TaskType};

    /// Helper to create an AppState with a fresh InMemoryTaskStore for testing.
    fn test_state() -> AppState {
        let store = Arc::new(InMemoryTaskStore::new()) as Arc<dyn TaskStore>;
        let permissions = agent_core::PermissionEngine::new(agent_core::PermissionMode::Bypass);
        AppState::new(permissions, Some(store))
    }

    /// Helper to create an AppState with no task store.
    fn test_state_no_store() -> AppState {
        let permissions = agent_core::PermissionEngine::new(agent_core::PermissionMode::Bypass);
        AppState::new(permissions, None)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_slash_input_returns_none() {
        let mut state = test_state();
        assert!(try_handle_slash_command("hello", &mut state).is_none());
        assert!(try_handle_slash_command("", &mut state).is_none());
        assert!(try_handle_slash_command("tasks", &mut state).is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_tasks_empty_state() {
        let mut state = test_state();
        let result = try_handle_slash_command("/tasks", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert_eq!(state.output_buffer.len(), 1);
        assert_eq!(state.output_buffer[0].text, "No tasks.");
        assert_eq!(state.output_buffer[0].style, SpanStyle::System);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_todos_empty_state() {
        let mut state = test_state();
        let result = try_handle_slash_command("/todos", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert_eq!(state.output_buffer.len(), 1);
        assert_eq!(state.output_buffer[0].text, "No todo items.");
        assert_eq!(state.output_buffer[0].style, SpanStyle::System);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_command_shows_error() {
        let mut state = test_state();
        let result = try_handle_slash_command("/foobar", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert_eq!(state.output_buffer.len(), 1);
        assert_eq!(state.output_buffer[0].style, SpanStyle::Error);
        assert!(state.output_buffer[0].text.contains("Unknown command"));
        assert!(state.output_buffer[0].text.contains("/tasks"));
        assert!(state.output_buffer[0].text.contains("/tasks ack"));
        assert!(state.output_buffer[0].text.contains("/tasks clear"));
        assert!(state.output_buffer[0].text.contains("/todos"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_tasks_with_whitespace_trimming() {
        let mut state = test_state();
        // Extra whitespace after / should still match
        let result = try_handle_slash_command("/  tasks  ", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert_eq!(state.output_buffer[0].text, "No tasks.");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_todos_with_whitespace_trimming() {
        let mut state = test_state();
        let result = try_handle_slash_command("/ todos ", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert_eq!(state.output_buffer[0].text, "No todo items.");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_store_shows_error() {
        let mut state = test_state_no_store();
        let result = try_handle_slash_command("/tasks", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert_eq!(state.output_buffer[0].style, SpanStyle::Error);
        assert!(state.output_buffer[0].text.contains("not available"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_tasks_ack_empty() {
        let mut state = test_state();
        let result = try_handle_slash_command("/tasks ack", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert!(state.output_buffer[0].text.contains("Acknowledged 0 task(s)"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_tasks_clear_empty() {
        let mut state = test_state();
        let result = try_handle_slash_command("/tasks clear", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert!(state.output_buffer[0].text.contains("Cleared 0 task(s)"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_tasks_with_tasks_present() {
        let mut state = test_state();
        let store = state.task_store.clone().unwrap();

        // Create a task and transition it to running
        let id = store.create_task(CreateTaskParams {
            description: "Test background task".to_string(),
            task_type: TaskType::SubAgent,
            dependencies: vec![],
            max_retries: 0,
        }).await.unwrap();

        store.transition_task(&id, agent_core::TaskStatus::Running, None).await.unwrap();

        let result = try_handle_slash_command("/tasks", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert!(state.output_buffer[0].text.contains("Running"));
        assert!(state.output_buffer[0].text.contains("Test background task"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_todos_with_items() {
        let mut state = test_state();
        let store = state.task_store.clone().unwrap();

        store.add_todo("Write tests".to_string(), None).await.unwrap();
        store.add_todo("Review PR".to_string(), None).await.unwrap();

        let result = try_handle_slash_command("/todos", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert!(state.output_buffer[0].text.contains("Write tests"));
        assert!(state.output_buffer[0].text.contains("Review PR"));
        assert!(state.output_buffer[0].text.contains("[ ]")); // Pending indicator
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slash_tasks_ack_with_terminal_tasks() {
        let mut state = test_state();
        let store = state.task_store.clone().unwrap();

        // Create and complete a task
        let id = store.create_task(CreateTaskParams {
            description: "Done task".to_string(),
            task_type: TaskType::SubAgent,
            dependencies: vec![],
            max_retries: 0,
        }).await.unwrap();
        store.transition_task(&id, agent_core::TaskStatus::Running, None).await.unwrap();
        store.transition_task(&id, agent_core::TaskStatus::Completed, Some("output".to_string())).await.unwrap();

        let result = try_handle_slash_command("/tasks ack", &mut state);
        assert_eq!(result, Some(UpdateResult::Continue));
        assert!(state.output_buffer[0].text.contains("Acknowledged 1 task(s)"));

        // Verify it's actually acknowledged
        let unacked = store.list_unacknowledged_terminal().await.unwrap();
        assert!(unacked.is_empty());
    }

    // ─── Property-Based Tests ───────────────────────────────────────────

    use proptest::prelude::*;

    /// Feature: task-manager-tui-integration, Property 13: Slash command routing
    /// — Input starting with `/` never produces `StartRun`
    /// **Validates: Requirements 4.5, 4.6, 4.9**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_slash_command_never_produces_start_run(
            suffix in "\\PC{0,50}"
        ) {
            let input = format!("/{}", suffix);
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut state = test_state();
                let result = try_handle_slash_command(&input, &mut state);
                // Any input starting with '/' must be handled (Some)
                prop_assert!(result.is_some(), "Input '{}' was not handled as a slash command", input);
                // And the result is never StartRun
                let r = result.unwrap();
                match r {
                    UpdateResult::StartRun(_) => {
                        prop_assert!(false, "Input '{}' produced StartRun, which violates Property 13", input);
                    }
                    _ => {} // Any other result is acceptable
                }
                Ok(())
            })?;
        }
    }

    /// Feature: task-manager-tui-integration, Property 14: Slash command whitespace tolerance
    /// — Commands with extra whitespace match correctly
    /// **Validates: Requirements 4.5, 4.6, 4.9**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_slash_command_whitespace_tolerance(
            cmd in prop_oneof![
                Just("tasks".to_string()),
                Just("todos".to_string()),
                Just("tasks ack".to_string()),
                Just("tasks clear".to_string()),
            ],
            leading_ws in "[ \\t]{0,5}",
            trailing_ws in "[ \\t]{0,5}",
        ) {
            let input = format!("/{}{}{}", leading_ws, cmd, trailing_ws);
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut state = test_state();
                let result = try_handle_slash_command(&input, &mut state);
                // Must be handled
                prop_assert!(result.is_some(), "Input '{}' was not handled", input);
                // Must produce Continue (not StartRun, not error for known commands)
                prop_assert_eq!(result.unwrap(), UpdateResult::Continue,
                    "Input '{}' did not produce Continue", input);
                // Must NOT have produced an unknown command error
                let has_error = state.output_buffer.iter().any(|span|
                    span.style == SpanStyle::Error && span.text.contains("Unknown command")
                );
                prop_assert!(!has_error,
                    "Input '{}' produced an unknown command error", input);
                Ok(())
            })?;
        }
    }

    /// Feature: task-manager-tui-integration, Property 15: Unknown slash command error
    /// — Unrecognized commands produce Error span listing available commands
    /// **Validates: Requirements 4.5, 4.6, 4.9**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn prop_unknown_slash_command_error(
            suffix in "[a-z]{1,10}".prop_filter("must not be a known command",
                |s| {
                    let trimmed = s.trim();
                    trimmed != "tasks"
                        && trimmed != "todos"
                        && trimmed != "tasks ack"
                        && trimmed != "tasks clear"
                }
            )
        ) {
            let input = format!("/{}", suffix);
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut state = test_state();
                let result = try_handle_slash_command(&input, &mut state);
                // Must be handled
                prop_assert!(result.is_some(), "Input '{}' was not handled", input);
                prop_assert_eq!(result.unwrap(), UpdateResult::Continue);
                // Output buffer must have an Error span
                prop_assert!(!state.output_buffer.is_empty(),
                    "Input '{}' produced no output", input);
                let error_span = &state.output_buffer[0];
                prop_assert_eq!(error_span.style.clone(), SpanStyle::Error,
                    "Input '{}' did not produce Error style span", input);
                // Error must list available commands
                prop_assert!(error_span.text.contains("/tasks"),
                    "Error for '{}' missing /tasks", input);
                prop_assert!(error_span.text.contains("/tasks ack"),
                    "Error for '{}' missing /tasks ack", input);
                prop_assert!(error_span.text.contains("/tasks clear"),
                    "Error for '{}' missing /tasks clear", input);
                prop_assert!(error_span.text.contains("/todos"),
                    "Error for '{}' missing /todos", input);
                Ok(())
            })?;
        }
    }
}
