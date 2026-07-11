//! Converts `agent_core::RunEvent`s into AG-UI-shaped wire events.
//!
//! See docs/superpowers/specs/2026-07-11-web-ui-design.md's "Wire protocol"
//! section for the full mapping table this module implements.

use agent_core::{Message, RunEvent, StreamChunk};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// A single AG-UI-shaped event, ready to serialize as one WebSocket text frame.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum AguiEvent {
    StepStarted { step_name: String },
    TextMessageStart { message_id: String, role: String },
    TextMessageContent { message_id: String, delta: String },
    TextMessageEnd { message_id: String },
    ToolCallStart { tool_call_id: String, tool_call_name: String },
    ToolCallEnd { tool_call_id: String },
    ToolCallResult { tool_call_id: String, content: String, role: String },
    RunFinished { outcome: Value, result: Value },
    RunError { message: String, code: Option<String> },
    MessagesSnapshot { messages: Vec<Message> },
    Custom { name: String, value: Value },
}

/// Converts a stream of `RunEvent`s into wire-ready `AguiEvent`s.
///
/// Stateful because an AG-UI text message needs a start/content*/end triple
/// derived from a run of `StreamChunk::TextDelta` events, and the end is only
/// knowable once a *different kind* of `RunEvent` arrives — see `convert`.
pub struct AguiEventConverter {
    open_message_id: Option<String>,
}

impl AguiEventConverter {
    pub fn new() -> Self {
        Self { open_message_id: None }
    }

    pub fn convert(&mut self, event: RunEvent) -> Vec<AguiEvent> {
        let mut out = Vec::new();

        if !matches!(event, RunEvent::StreamChunk(_)) {
            if let Some(message_id) = self.open_message_id.take() {
                out.push(AguiEvent::TextMessageEnd { message_id });
            }
        }

        match event {
            RunEvent::TurnStart { agent, .. } => {
                out.push(AguiEvent::StepStarted { step_name: agent });
            }
            RunEvent::StreamChunk(StreamChunk::TextDelta { text }) => {
                let is_new_message = self.open_message_id.is_none();
                let message_id = self
                    .open_message_id
                    .get_or_insert_with(|| Uuid::new_v4().to_string())
                    .clone();
                if is_new_message {
                    out.push(AguiEvent::TextMessageStart {
                        message_id: message_id.clone(),
                        role: "assistant".to_string(),
                    });
                }
                out.push(AguiEvent::TextMessageContent { message_id, delta: text });
            }
            RunEvent::StreamChunk(_) => {
                // ThinkingDelta / ToolUseStart / ToolUseInputDelta / ToolUseEnd /
                // MessageStop: low-level streaming detail. Tool lifecycle is
                // surfaced via the ToolStart/ToolEnd RunEvents below instead.
            }
            RunEvent::ToolStart { id, name } => {
                out.push(AguiEvent::ToolCallStart { tool_call_id: id, tool_call_name: name });
            }
            RunEvent::ToolEnd { id, output, is_error, .. } => {
                out.push(AguiEvent::ToolCallEnd { tool_call_id: id.clone() });
                out.push(AguiEvent::ToolCallResult {
                    tool_call_id: id.clone(),
                    content: output,
                    role: "tool".to_string(),
                });
                if is_error {
                    out.push(AguiEvent::Custom {
                        name: "arlo.tool_error".to_string(),
                        value: json!({ "toolCallId": id }),
                    });
                }
            }
            RunEvent::Compaction { stage, messages_removed } => {
                out.push(AguiEvent::Custom {
                    name: "arlo.compaction".to_string(),
                    value: json!({ "stage": stage, "messagesRemoved": messages_removed }),
                });
            }
            RunEvent::StepResolved(_) => {
                // Internal control-flow detail; not forwarded to the client.
            }
            RunEvent::AgentEnd { output, usage, .. } => {
                out.push(AguiEvent::RunFinished {
                    outcome: json!({ "type": "success" }),
                    result: json!({ "output": output, "usage": usage }),
                });
            }
            RunEvent::Interruption { pending } => {
                // Dead in practice: both TUI and web always register an
                // ApprovalHandler, so run_loop.rs's NextStep::Interruption
                // handling resolves approvals inline and never emits this
                // variant (see run_loop.rs's NextStep::Interruption match arm).
                // Handled defensively so this match stays exhaustive.
                out.push(AguiEvent::RunError {
                    message: format!(
                        "interrupted with no approval handler configured ({} pending)",
                        pending.len()
                    ),
                    code: Some("no_handler".to_string()),
                });
            }
            RunEvent::GuardrailTripped { name, reason } => {
                out.push(AguiEvent::RunError {
                    message: reason,
                    code: Some(format!("guardrail:{}", name)),
                });
            }
            RunEvent::MaxTurns { count } => {
                out.push(AguiEvent::RunFinished {
                    outcome: json!({ "type": "success" }),
                    result: json!({ "maxTurns": count }),
                });
            }
            RunEvent::Aborted { reason } => {
                out.push(AguiEvent::RunError { message: reason, code: Some("aborted".to_string()) });
            }
            RunEvent::Error { error } => {
                out.push(AguiEvent::RunError { message: error, code: None });
            }
        }

        out
    }
}

impl Default for AguiEventConverter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{PendingApproval, Usage};

    fn usage() -> Usage {
        Usage { input_tokens: 10, output_tokens: 20, cache_read_tokens: None }
    }

    #[test]
    fn turn_start_maps_to_step_started() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::TurnStart { turn: 1, agent: "arlo".to_string() });
        assert_eq!(out, vec![AguiEvent::StepStarted { step_name: "arlo".to_string() }]);
    }

    #[test]
    fn first_text_delta_emits_start_and_content() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "hi".to_string(),
        }));
        assert_eq!(out.len(), 2);
        match (&out[0], &out[1]) {
            (
                AguiEvent::TextMessageStart { message_id: id1, role },
                AguiEvent::TextMessageContent { message_id: id2, delta },
            ) => {
                assert_eq!(id1, id2);
                assert_eq!(role, "assistant");
                assert_eq!(delta, "hi");
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn second_text_delta_only_emits_content_with_same_message_id() {
        let mut conv = AguiEventConverter::new();
        let first = conv.convert(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "a".to_string(),
        }));
        let first_id = match &first[0] {
            AguiEvent::TextMessageStart { message_id, .. } => message_id.clone(),
            other => panic!("expected TextMessageStart, got {other:?}"),
        };
        let second = conv.convert(RunEvent::StreamChunk(StreamChunk::TextDelta {
            text: "b".to_string(),
        }));
        assert_eq!(
            second,
            vec![AguiEvent::TextMessageContent { message_id: first_id, delta: "b".to_string() }]
        );
    }

    #[test]
    fn non_streamchunk_event_closes_open_text_message() {
        let mut conv = AguiEventConverter::new();
        conv.convert(RunEvent::StreamChunk(StreamChunk::TextDelta { text: "a".to_string() }));
        let out = conv.convert(RunEvent::ToolStart { id: "t1".to_string(), name: "shell".to_string() });
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], AguiEvent::TextMessageEnd { .. }));
        assert!(matches!(out[1], AguiEvent::ToolCallStart { .. }));
    }

    #[test]
    fn non_text_delta_chunks_produce_no_events_and_do_not_close_text_message() {
        let mut conv = AguiEventConverter::new();
        let first = conv.convert(RunEvent::StreamChunk(StreamChunk::TextDelta { text: "a".to_string() }));
        let first_id = match &first[0] {
            AguiEvent::TextMessageStart { message_id, .. } => message_id.clone(),
            other => panic!("expected TextMessageStart, got {other:?}"),
        };
        let mid = conv.convert(RunEvent::StreamChunk(StreamChunk::ThinkingDelta {
            text: "hmm".to_string(),
        }));
        assert!(mid.is_empty());
        let second = conv.convert(RunEvent::StreamChunk(StreamChunk::TextDelta { text: "b".to_string() }));
        assert_eq!(
            second,
            vec![AguiEvent::TextMessageContent { message_id: first_id, delta: "b".to_string() }]
        );
    }

    #[test]
    fn tool_start_maps_to_tool_call_start() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::ToolStart { id: "t1".to_string(), name: "shell".to_string() });
        assert_eq!(
            out,
            vec![AguiEvent::ToolCallStart { tool_call_id: "t1".to_string(), tool_call_name: "shell".to_string() }]
        );
    }

    #[test]
    fn tool_end_success_maps_to_end_and_result() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::ToolEnd {
            id: "t1".to_string(),
            name: "shell".to_string(),
            output: "ok".to_string(),
            is_error: false,
        });
        assert_eq!(
            out,
            vec![
                AguiEvent::ToolCallEnd { tool_call_id: "t1".to_string() },
                AguiEvent::ToolCallResult {
                    tool_call_id: "t1".to_string(),
                    content: "ok".to_string(),
                    role: "tool".to_string(),
                },
            ]
        );
    }

    #[test]
    fn tool_end_error_also_emits_custom_tool_error() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::ToolEnd {
            id: "t1".to_string(),
            name: "shell".to_string(),
            output: "boom".to_string(),
            is_error: true,
        });
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[2],
            AguiEvent::Custom {
                name: "arlo.tool_error".to_string(),
                value: json!({ "toolCallId": "t1" }),
            }
        );
    }

    #[test]
    fn compaction_maps_to_custom_event() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::Compaction {
            stage: "summarize".to_string(),
            messages_removed: 4,
        });
        assert_eq!(
            out,
            vec![AguiEvent::Custom {
                name: "arlo.compaction".to_string(),
                value: json!({ "stage": "summarize", "messagesRemoved": 4 }),
            }]
        );
    }

    #[test]
    fn step_resolved_is_not_forwarded() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::StepResolved(agent_core::NextStep::Continue));
        assert!(out.is_empty());
    }

    #[test]
    fn agent_end_maps_to_run_finished_success() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::AgentEnd {
            agent: "arlo".to_string(),
            output: "done".to_string(),
            usage: usage(),
        });
        assert_eq!(
            out,
            vec![AguiEvent::RunFinished {
                outcome: json!({ "type": "success" }),
                result: json!({ "output": "done", "usage": usage() }),
            }]
        );
    }

    #[test]
    fn interruption_maps_defensively_to_run_error() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::Interruption {
            pending: vec![PendingApproval {
                tool_name: "shell".to_string(),
                tool_input: json!({}),
                request_id: "approval-t1".to_string(),
            }],
        });
        assert_eq!(
            out,
            vec![AguiEvent::RunError {
                message: "interrupted with no approval handler configured (1 pending)".to_string(),
                code: Some("no_handler".to_string()),
            }]
        );
    }

    #[test]
    fn guardrail_tripped_maps_to_run_error_with_guardrail_code() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::GuardrailTripped {
            name: "no_secrets".to_string(),
            reason: "found an API key".to_string(),
        });
        assert_eq!(
            out,
            vec![AguiEvent::RunError {
                message: "found an API key".to_string(),
                code: Some("guardrail:no_secrets".to_string()),
            }]
        );
    }

    #[test]
    fn max_turns_maps_to_run_finished_with_max_turns_result() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::MaxTurns { count: 25 });
        assert_eq!(
            out,
            vec![AguiEvent::RunFinished {
                outcome: json!({ "type": "success" }),
                result: json!({ "maxTurns": 25 }),
            }]
        );
    }

    #[test]
    fn aborted_maps_to_run_error_with_aborted_code() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::Aborted { reason: "user cancelled".to_string() });
        assert_eq!(
            out,
            vec![AguiEvent::RunError {
                message: "user cancelled".to_string(),
                code: Some("aborted".to_string()),
            }]
        );
    }

    #[test]
    fn error_maps_to_run_error_with_no_code() {
        let mut conv = AguiEventConverter::new();
        let out = conv.convert(RunEvent::Error { error: "model timeout".to_string() });
        assert_eq!(
            out,
            vec![AguiEvent::RunError { message: "model timeout".to_string(), code: None }]
        );
    }

    #[test]
    fn agui_event_serializes_with_camel_case_fields_and_pascal_case_type_tag() {
        let event = AguiEvent::ToolCallStart {
            tool_call_id: "t1".to_string(),
            tool_call_name: "shell".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            json!({ "type": "ToolCallStart", "toolCallId": "t1", "toolCallName": "shell" })
        );
    }
}
