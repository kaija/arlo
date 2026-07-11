// Task notification formatting and polling for the TUI.
//
// Polls the TaskStore for unacknowledged terminal tasks and formats them
// as OutputSpan entries for display in the output buffer.

use agent_core::{ContentBlock, Message, TaskEntry, TaskStatus};

use super::app::{AppState, OutputSpan, SpanStyle};

/// Format a terminal task into a notification OutputSpan.
///
/// - Completed + non-empty output → System style, description + first 200 chars (+ "…" if truncated)
/// - Completed + no output → System style, description + "completed with no output"
/// - Failed + error → Warning style, description + error message
/// - Failed + no error → Warning style, description + "failed with no error details"
pub fn format_task_notification(task: &TaskEntry) -> OutputSpan {
    match task.status {
        TaskStatus::Completed => {
            let detail = match &task.output {
                Some(output) if !output.is_empty() => {
                    if output.len() > 200 {
                        format!("{}…", &output[..200])
                    } else {
                        output.clone()
                    }
                }
                _ => "completed with no output".to_string(),
            };
            OutputSpan {
                text: format!("Task completed: \"{}\" — {}", task.description, detail),
                style: SpanStyle::System,
            }
        }
        TaskStatus::Failed => {
            let detail = match &task.last_error {
                Some(err) if !err.is_empty() => err.clone(),
                _ => "failed with no error details".to_string(),
            };
            OutputSpan {
                text: format!("Task failed: \"{}\" — {}", task.description, detail),
                style: SpanStyle::Warning,
            }
        }
        // For other terminal states (Killed), use a generic system message.
        _ => OutputSpan {
            text: format!("Task ended: \"{}\"", task.description),
            style: SpanStyle::System,
        },
    }
}

/// Poll for unacknowledged terminal tasks and append notifications to the output buffer.
///
/// For each unacknowledged terminal task:
/// 1. Format it as a notification OutputSpan
/// 2. Append to state.output_buffer
/// 3. Acknowledge the task to prevent duplicate notifications
pub async fn poll_notifications(state: &mut AppState) {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => return,
    };

    let tasks = match store.list_unacknowledged_terminal().await {
        Ok(t) => t,
        Err(_) => return, // Silently skip on error (per design: log at warn, skip display update)
    };

    for task in &tasks {
        let span = format_task_notification(task);

        // Record the result in conversation history so the model sees the
        // sub-agent's output on the next turn — without this the result is
        // only ever printed, never returned to the agent.
        state.history.push(Message::User {
            content: vec![ContentBlock::Text {
                text: format!("[background task update] {}", span.text),
            }],
        });

        state.output_buffer.push(OutputSpan {
            text: format!("\n{}\n", span.text),
            style: span.style,
        });

        // Acknowledge immediately to prevent duplicate notifications on next tick
        let _ = store.acknowledge_task(&task.id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{CreateTaskParams, InMemoryTaskStore, TaskStore, TaskType};
    use proptest::prelude::*;
    use std::sync::Arc;
    use std::time::SystemTime;

    /// Helper to create a completed TaskEntry with given output.
    fn completed_task(description: &str, output: Option<String>) -> TaskEntry {
        TaskEntry {
            id: "test-id-1".to_string(),
            status: TaskStatus::Completed,
            description: description.to_string(),
            task_type: TaskType::SubAgent,
            created_at: SystemTime::now(),
            completed_at: Some(SystemTime::now()),
            output,
            usage: None,
            dependencies: vec![],
            max_retries: 0,
            retry_count: 0,
            last_error: None,
            acknowledged: false,
        }
    }

    /// Helper to create a failed TaskEntry with given error.
    fn failed_task(description: &str, error: Option<String>) -> TaskEntry {
        TaskEntry {
            id: "test-id-2".to_string(),
            status: TaskStatus::Failed,
            description: description.to_string(),
            task_type: TaskType::SubAgent,
            created_at: SystemTime::now(),
            completed_at: Some(SystemTime::now()),
            output: None,
            usage: None,
            dependencies: vec![],
            max_retries: 0,
            retry_count: 0,
            last_error: error,
            acknowledged: false,
        }
    }

    #[test]
    fn completed_with_short_output() {
        let task = completed_task("Build project", Some("Success!".to_string()));
        let span = format_task_notification(&task);
        assert_eq!(span.style, SpanStyle::System);
        assert_eq!(span.text, "Task completed: \"Build project\" — Success!");
    }

    #[test]
    fn completed_with_long_output_truncated() {
        let long_output = "x".repeat(300);
        let task = completed_task("Long task", Some(long_output));
        let span = format_task_notification(&task);
        assert_eq!(span.style, SpanStyle::System);
        assert!(span.text.contains("Task completed: \"Long task\" — "));
        // Should have 200 'x' chars followed by '…'
        assert!(span.text.ends_with('…'));
        // The description part + 200 chars + "…"
        let expected_detail = format!("{}…", "x".repeat(200));
        assert!(span.text.contains(&expected_detail));
    }

    #[test]
    fn completed_with_exactly_200_chars_no_truncation() {
        let exact_output = "y".repeat(200);
        let task = completed_task("Exact task", Some(exact_output.clone()));
        let span = format_task_notification(&task);
        assert_eq!(span.style, SpanStyle::System);
        assert!(!span.text.ends_with('…'));
        assert!(span.text.contains(&exact_output));
    }

    #[test]
    fn completed_with_no_output() {
        let task = completed_task("Silent task", None);
        let span = format_task_notification(&task);
        assert_eq!(span.style, SpanStyle::System);
        assert_eq!(
            span.text,
            "Task completed: \"Silent task\" — completed with no output"
        );
    }

    #[test]
    fn completed_with_empty_output() {
        let task = completed_task("Empty task", Some(String::new()));
        let span = format_task_notification(&task);
        assert_eq!(span.style, SpanStyle::System);
        assert_eq!(
            span.text,
            "Task completed: \"Empty task\" — completed with no output"
        );
    }

    #[test]
    fn failed_with_error() {
        let task = failed_task("Broken task", Some("connection refused".to_string()));
        let span = format_task_notification(&task);
        assert_eq!(span.style, SpanStyle::Warning);
        assert_eq!(
            span.text,
            "Task failed: \"Broken task\" — connection refused"
        );
    }

    #[test]
    fn failed_with_no_error() {
        let task = failed_task("Mystery failure", None);
        let span = format_task_notification(&task);
        assert_eq!(span.style, SpanStyle::Warning);
        assert_eq!(
            span.text,
            "Task failed: \"Mystery failure\" — failed with no error details"
        );
    }

    #[test]
    fn failed_with_empty_error() {
        let task = failed_task("Empty error", Some(String::new()));
        let span = format_task_notification(&task);
        assert_eq!(span.style, SpanStyle::Warning);
        assert_eq!(
            span.text,
            "Task failed: \"Empty error\" — failed with no error details"
        );
    }

    #[tokio::test]
    async fn poll_notifications_with_no_store() {
        let permissions = agent_core::PermissionEngine::new(agent_core::PermissionMode::Bypass);
        let mut state = AppState::new(permissions, None);
        poll_notifications(&mut state).await;
        assert!(state.output_buffer.is_empty());
    }

    #[tokio::test]
    async fn poll_notifications_with_empty_store() {
        let store = Arc::new(InMemoryTaskStore::new()) as Arc<dyn TaskStore>;
        let permissions = agent_core::PermissionEngine::new(agent_core::PermissionMode::Bypass);
        let mut state = AppState::new(permissions, Some(store));
        poll_notifications(&mut state).await;
        assert!(state.output_buffer.is_empty());
    }

    #[tokio::test]
    async fn poll_notifications_formats_and_acknowledges() {
        let store = Arc::new(InMemoryTaskStore::new()) as Arc<dyn TaskStore>;
        let permissions = agent_core::PermissionEngine::new(agent_core::PermissionMode::Bypass);
        let mut state = AppState::new(permissions, Some(store.clone()));

        // Create and complete a task
        let id = store
            .create_task(CreateTaskParams {
                description: "Test task".to_string(),
                task_type: TaskType::SubAgent,
                dependencies: vec![],
                max_retries: 0,
            })
            .await
            .unwrap();
        store
            .transition_task(&id, TaskStatus::Running, None)
            .await
            .unwrap();
        store
            .transition_task(&id, TaskStatus::Completed, Some("done".to_string()))
            .await
            .unwrap();

        // Poll should pick it up
        poll_notifications(&mut state).await;
        assert_eq!(state.output_buffer.len(), 1);
        assert!(state.output_buffer[0].text.contains("Test task"));
        assert!(state.output_buffer[0].text.contains("done"));

        // Polling again should not duplicate
        poll_notifications(&mut state).await;
        assert_eq!(state.output_buffer.len(), 1);
    }

    // ─── Property-Based Tests ───────────────────────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Feature: task-manager-tui-integration, Property 11: Notification formatting and truncation
        /// Validates: Requirements 3.4, 5.1, 5.3
        #[test]
        fn prop_notification_formatting_and_truncation(
            description in "[a-zA-Z0-9 ]{1,100}",
            output_str in "[a-zA-Z0-9 ]{0,500}",
            use_failed in proptest::bool::ANY,
        ) {
            if use_failed {
                // Test Failed case with last_error
                let task = failed_task(&description, Some(output_str.clone()));
                let span = format_task_notification(&task);

                // Should always contain the description
                assert!(
                    span.text.contains(&description),
                    "Failed notification should contain description"
                );

                if output_str.is_empty() {
                    // Empty error treated as no error
                    assert!(
                        span.text.contains("failed with no error details"),
                        "Empty error should use fallback message"
                    );
                } else {
                    // Failed tasks include full error (no truncation per design for errors)
                    assert!(
                        span.text.contains(&output_str),
                        "Failed notification should contain full error"
                    );
                }
                assert_eq!(span.style, SpanStyle::Warning);
            } else {
                // Test Completed case with output
                let task = completed_task(&description, Some(output_str.clone()));
                let span = format_task_notification(&task);

                // Should always contain the description
                assert!(
                    span.text.contains(&description),
                    "Completed notification should contain description"
                );

                if output_str.is_empty() {
                    // Empty output treated as no output
                    assert!(
                        span.text.contains("completed with no output"),
                        "Empty output should use fallback message"
                    );
                } else if output_str.len() > 200 {
                    // Truncated: ends with "…" and contains first 200 chars
                    assert!(
                        span.text.ends_with('…'),
                        "Long output should be truncated with '…' suffix"
                    );
                    assert!(
                        span.text.contains(&output_str[..200]),
                        "Truncated notification should contain first 200 chars of output"
                    );
                } else {
                    // Not truncated: contains full output without trailing "…"
                    assert!(
                        span.text.contains(&output_str),
                        "Short output should be included in full"
                    );
                    assert!(
                        !span.text.ends_with('…'),
                        "Short output should not have '…' suffix"
                    );
                }
                assert_eq!(span.style, SpanStyle::System);
            }
        }
    }

    /// Feature: task-manager-tui-integration, Property 12: Acknowledge prevents duplicate notification
    /// Validates: Requirements 3.8, 5.6
    #[test]
    fn prop_acknowledge_prevents_duplicate_notification() {
        use proptest::strategy::Strategy;
        use proptest::test_runner::{TestCaseError, TestRunner};

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let mut runner = TestRunner::new(ProptestConfig::with_cases(100));

        runner
            .run(&("[a-zA-Z0-9 ]{1,50}".prop_map(|s| s)), |description| {
                rt.block_on(async {
                    let store = Arc::new(InMemoryTaskStore::new()) as Arc<dyn TaskStore>;
                    let permissions =
                        agent_core::PermissionEngine::new(agent_core::PermissionMode::Bypass);
                    let mut state = AppState::new(permissions, Some(store.clone()));

                    // Create a task and transition to terminal (Completed)
                    let id = store
                        .create_task(CreateTaskParams {
                            description: description.clone(),
                            task_type: TaskType::SubAgent,
                            dependencies: vec![],
                            max_retries: 0,
                        })
                        .await
                        .unwrap();
                    store
                        .transition_task(&id, TaskStatus::Running, None)
                        .await
                        .unwrap();
                    store
                        .transition_task(&id, TaskStatus::Completed, Some("done".to_string()))
                        .await
                        .unwrap();

                    // Poll notifications — should pick up the task
                    poll_notifications(&mut state).await;

                    // After poll, list_unacknowledged_terminal should be empty
                    let unacked = store.list_unacknowledged_terminal().await.unwrap();
                    if !unacked.is_empty() {
                        return Err(TestCaseError::Fail(
                            format!(
                                "Expected no unacknowledged tasks after poll, found {}",
                                unacked.len()
                            )
                            .into(),
                        ));
                    }

                    Ok(())
                })
            })
            .unwrap();
    }

    #[tokio::test]
    async fn poll_notifications_handles_failed_task() {
        let store = Arc::new(InMemoryTaskStore::new()) as Arc<dyn TaskStore>;
        let permissions = agent_core::PermissionEngine::new(agent_core::PermissionMode::Bypass);
        let mut state = AppState::new(permissions, Some(store.clone()));

        // Create and fail a task
        let id = store
            .create_task(CreateTaskParams {
                description: "Failing task".to_string(),
                task_type: TaskType::SubAgent,
                dependencies: vec![],
                max_retries: 0,
            })
            .await
            .unwrap();
        store
            .transition_task(&id, TaskStatus::Running, None)
            .await
            .unwrap();
        store
            .transition_task(&id, TaskStatus::Failed, Some("timeout".to_string()))
            .await
            .unwrap();

        poll_notifications(&mut state).await;
        assert_eq!(state.output_buffer.len(), 1);
        assert_eq!(state.output_buffer[0].style, SpanStyle::Warning);
        assert!(state.output_buffer[0].text.contains("Failing task"));
        assert!(state.output_buffer[0].text.contains("timeout"));
    }
}
