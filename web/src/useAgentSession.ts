import { useCallback, useEffect, useReducer, useRef } from "react";
import type {
  AguiEvent,
  ApprovalDecision,
  ClientMessage,
  CoreMessage,
  PermissionRequestValue,
  TaskEntry,
  TaskNoticeValue,
  TodoItem,
} from "./types";

export type ConnectionState = "connecting" | "open" | "closed_takeover" | "closed_error";

export type TimelineItem =
  | { kind: "text"; id: string; role: "user" | "assistant" | "system"; text: string }
  | { kind: "tool"; id: string; name: string; status: "pending" | "done" | "error"; output?: string };

export interface ToastEntry {
  id: string;
  text: string;
  kind: "success" | "error";
}

interface State {
  connection: ConnectionState;
  timeline: TimelineItem[];
  approvals: PermissionRequestValue[];
  tasks: TaskEntry[];
  todos: TodoItem[];
  toasts: ToastEntry[];
  runActive: boolean;
  interruptedNotice: string | null;
}

const initialState: State = {
  connection: "connecting",
  timeline: [],
  approvals: [],
  tasks: [],
  todos: [],
  toasts: [],
  runActive: false,
  interruptedNotice: null,
};

function coreMessageToTimelineText(message: CoreMessage): string {
  // MessagesSnapshot only ever contains user/assistant/system messages in
  // practice (the backend's session.rs never persists tool_result — see
  // the backend plan's Task 4 note on spawn_run), but this function stays
  // total over CoreMessage rather than assuming its caller always filters.
  if (message.role === "system" || message.role === "tool_result") {
    return message.content;
  }
  return message.content
    .filter((block): block is { type: "text"; text: string } => block.type === "text")
    .map((block) => block.text)
    .join("");
}

type Action =
  | { kind: "connection_open" }
  | { kind: "connection_closed" }
  | { kind: "event"; event: AguiEvent }
  | { kind: "dismiss_toast"; id: string }
  | { kind: "reset_for_reconnect" };

let toastCounter = 0;
function nextToastId(): string {
  toastCounter += 1;
  return `toast-${toastCounter}`;
}

function reducer(state: State, action: Action): State {
  switch (action.kind) {
    case "connection_open":
      return { ...state, connection: "open" };

    case "connection_closed":
      // If a takeover close already fired (arlo.session_closed sets
      // connection to "closed_takeover" via applyCustomEvent below), leave
      // that as the terminal state; otherwise this is an unexpected drop.
      return state.connection === "closed_takeover" ? state : { ...state, connection: "closed_error" };

    case "reset_for_reconnect":
      return { ...initialState, connection: "connecting" };

    case "dismiss_toast":
      return { ...state, toasts: state.toasts.filter((t) => t.id !== action.id) };

    case "event":
      return applyEvent(state, action.event);

    default:
      return state;
  }
}

function applyEvent(state: State, event: AguiEvent): State {
  switch (event.type) {
    case "MessagesSnapshot": {
      const timeline: TimelineItem[] = event.messages
        .filter((m) => m.role === "user" || m.role === "assistant" || m.role === "system")
        .map((m, i) => ({
          kind: "text",
          id: `history-${i}`,
          role: m.role as "user" | "assistant" | "system",
          text: coreMessageToTimelineText(m),
        }));
      return { ...state, timeline };
    }

    case "TextMessageStart":
      return {
        ...state,
        timeline: [...state.timeline, { kind: "text", id: event.messageId, role: "assistant", text: "" }],
      };

    case "TextMessageContent":
      return {
        ...state,
        timeline: state.timeline.map((item) =>
          item.kind === "text" && item.id === event.messageId
            ? { ...item, text: item.text + event.delta }
            : item
        ),
      };

    case "TextMessageEnd":
      return state;

    case "ToolCallStart":
      return {
        ...state,
        runActive: true,
        timeline: [
          ...state.timeline,
          { kind: "tool", id: event.toolCallId, name: event.toolCallName, status: "pending" },
        ],
      };

    case "ToolCallEnd":
      return state;

    case "ToolCallResult":
      return {
        ...state,
        timeline: state.timeline.map((item) =>
          item.kind === "tool" && item.id === event.toolCallId
            ? { ...item, status: "done", output: event.content }
            : item
        ),
      };

    case "RunFinished":
      return { ...state, runActive: false };

    case "RunError":
      if (event.code === "aborted" && event.message === "session opened in another tab") {
        // The backend's takeover notice for the *new* tab (see backend
        // plan Task 5) — not a failure. SessionBanner renders this.
        return { ...state, runActive: false, interruptedNotice: event.message };
      }
      return {
        ...state,
        runActive: false,
        toasts: [...state.toasts, { id: nextToastId(), text: event.message, kind: "error" }],
      };

    case "Custom":
      return applyCustomEvent(state, event);

    default:
      return state;
  }
}

function applyCustomEvent(state: State, event: Extract<AguiEvent, { type: "Custom" }>): State {
  switch (event.name) {
    case "arlo.tool_error": {
      // The catch-all `Custom` variant's `value: unknown` prevents TS from
      // narrowing `event.value` off `event.name` alone, so each case casts
      // to the type documented for that name in the wire protocol.
      const value = event.value as { toolCallId: string };
      return {
        ...state,
        timeline: state.timeline.map((item) =>
          item.kind === "tool" && item.id === value.toolCallId ? { ...item, status: "error" } : item
        ),
      };
    }

    case "arlo.permission_request": {
      const value = event.value as PermissionRequestValue;
      return { ...state, approvals: [...state.approvals, value] };
    }

    case "arlo.task_snapshot": {
      const value = event.value as TaskEntry[];
      return { ...state, tasks: value };
    }

    case "arlo.todo_snapshot": {
      const value = event.value as TodoItem[];
      return { ...state, todos: value };
    }

    case "arlo.task_notice": {
      const value = event.value as TaskNoticeValue;
      const ok = value.status === "Completed";
      const text = ok ? `"${value.description}" completed` : `"${value.description}" failed: ${value.summary}`;
      return { ...state, toasts: [...state.toasts, { id: nextToastId(), text, kind: ok ? "success" : "error" }] };
    }

    case "arlo.session_closed":
      return { ...state, connection: "closed_takeover" };

    default:
      return state;
  }
}

function wsUrl(): string {
  if (import.meta.env.DEV) {
    // Vite's dev server (default :5173) and the arlo backend (default
    // :8787) are separate processes in development — see Global
    // Constraints. Override with VITE_ARLO_WS_URL if the backend runs on
    // a different port.
    return import.meta.env.VITE_ARLO_WS_URL ?? "ws://localhost:8787/ws";
  }
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/ws`;
}

export interface AgentSession extends State {
  dismissToast(id: string): void;
  sendUserMessage(text: string): void;
  sendApprovalResponse(callId: string, decision: ApprovalDecision, pattern: string | null): void;
  sendAbort(): void;
  reconnect(): void;
}

export function useAgentSession(): AgentSession {
  const [state, dispatch] = useReducer(reducer, initialState);
  const socketRef = useRef<WebSocket | null>(null);
  const [generation, bumpGeneration] = useReducer((n: number) => n + 1, 0);

  useEffect(() => {
    if (generation > 0) {
      // A fresh WebSocket for a new connection generation (Task 6's
      // "Reconnect here" button calls reconnect(), which bumps this) needs
      // to start from a clean slate — the old socket's history/approvals/
      // etc no longer apply.
      dispatch({ kind: "reset_for_reconnect" });
    }
    const socket = new WebSocket(wsUrl());
    socketRef.current = socket;

    socket.addEventListener("open", () => dispatch({ kind: "connection_open" }));
    socket.addEventListener("close", () => dispatch({ kind: "connection_closed" }));
    socket.addEventListener("error", () => dispatch({ kind: "connection_closed" }));
    socket.addEventListener("message", (ev) => {
      const event = JSON.parse(ev.data as string) as AguiEvent;
      dispatch({ kind: "event", event });
    });

    return () => socket.close();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [generation]);

  const send = useCallback((message: ClientMessage) => {
    socketRef.current?.send(JSON.stringify(message));
  }, []);

  return {
    ...state,
    dismissToast: (id: string) => dispatch({ kind: "dismiss_toast", id }),
    sendUserMessage: (text: string) => send({ type: "user_message", text }),
    sendApprovalResponse: (callId: string, decision: ApprovalDecision, pattern: string | null) =>
      send({ type: "approval_response", responses: [{ callId, decision, pattern }] }),
    sendAbort: () => send({ type: "abort" }),
    reconnect: () => bumpGeneration(),
  };
}
