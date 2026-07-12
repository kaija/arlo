// Mirrors crates/agent-cli/src/web/agui.rs's AguiEvent and the agent-core
// Message/TaskEntry/TodoItem types it serializes as-is. Keep in sync with
// docs/superpowers/plans/2026-07-11-web-ui-backend.md's wire protocol
// reference if either side changes.

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "image"; media_type: string; data: string; source_type: string }
  | { type: "tool_use"; id: string; name: string; input: unknown };

export interface Usage {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens?: number | null;
}

export type CoreMessage =
  | { role: "system"; content: string }
  | { role: "user"; content: ContentBlock[] }
  | { role: "assistant"; content: ContentBlock[]; usage?: Usage }
  | { role: "tool_result"; tool_use_id: string; content: string; is_error: boolean };

/** Rust's `std::time::SystemTime` serde representation — not an ISO string. */
export interface SerdeSystemTime {
  secs_since_epoch: number;
  nanos_since_epoch: number;
}

export type TaskStatus = "Pending" | "Running" | "Completed" | "Failed" | "Killed";
export type TaskType = "SubAgent" | "Background";

export interface TaskUsage {
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
}

export interface TaskEntry {
  id: string;
  status: TaskStatus;
  description: string;
  task_type: TaskType;
  created_at: SerdeSystemTime;
  completed_at: SerdeSystemTime | null;
  output: string | null;
  usage: TaskUsage | null;
  dependencies: string[];
  max_retries: number;
  retry_count: number;
  last_error: string | null;
  acknowledged: boolean;
}

export type TodoStatus = "Pending" | "InProgress" | "Completed";

export interface TodoItem {
  id: string;
  content: string;
  status: TodoStatus;
  active_form: string | null;
}

export interface PermissionRequestValue {
  callId: string;
  agentName: string | null;
  toolName: string;
  toolInput: unknown;
  options: string[];
}

export interface TaskNoticeValue {
  taskId: string;
  status: TaskStatus;
  description: string;
  summary: string;
}

export type AguiEvent =
  | { type: "StepStarted"; stepName: string }
  | { type: "TextMessageStart"; messageId: string; role: string }
  | { type: "TextMessageContent"; messageId: string; delta: string }
  | { type: "TextMessageEnd"; messageId: string }
  | { type: "ToolCallStart"; toolCallId: string; toolCallName: string }
  | { type: "ToolCallEnd"; toolCallId: string }
  | { type: "ToolCallResult"; toolCallId: string; content: string; role: string }
  | { type: "RunFinished"; outcome: { type: string }; result: Record<string, unknown> }
  | { type: "RunError"; message: string; code: string | null }
  | { type: "MessagesSnapshot"; messages: CoreMessage[] }
  | { type: "Custom"; name: "arlo.tool_error"; value: { toolCallId: string } }
  | { type: "Custom"; name: "arlo.compaction"; value: { stage: string; messagesRemoved: number } }
  | { type: "Custom"; name: "arlo.permission_request"; value: PermissionRequestValue }
  | { type: "Custom"; name: "arlo.task_snapshot"; value: TaskEntry[] }
  | { type: "Custom"; name: "arlo.todo_snapshot"; value: TodoItem[] }
  | { type: "Custom"; name: "arlo.task_notice"; value: TaskNoticeValue }
  | { type: "Custom"; name: "arlo.session_closed"; value: { reason: string } }
  | { type: "Custom"; name: string; value: unknown };

export type ApprovalDecision = "allow_once" | "allow_always" | "reject_once" | "reject_always";

export type ClientMessage =
  | { type: "user_message"; text: string }
  | { type: "approval_response"; responses: { callId: string; decision: ApprovalDecision; pattern: string | null }[] }
  | { type: "abort" };
