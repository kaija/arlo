# Web UI for arlo — Design

Date: 2026-07-11
Status: Approved for planning

## Problem

arlo currently has two front-ends: a single-prompt CLI mode and a ratatui TUI
REPL. Both run in a terminal. There is no browser-based UI, and no way to
watch background sub-agent / task status other than the TUI's `/tasks` and
`/todos` slash commands.

We want a web UI that:
- lets a user chat with the agent (send prompts, see streamed responses)
- surfaces human-in-the-loop (HITL) tool approval requests and lets the user
  respond from the browser
- shows live task and sub-agent status (the `TaskStore` contents) and the
  todo list, the same data the TUI's `/tasks` and `/todos` commands expose

## Scope decisions (from brainstorming)

- **Relationship to the TUI**: new mode alongside the existing TUI, not a
  replacement. `arlo --web` starts a web server instead of the terminal UI.
  The ratatui TUI and single-prompt mode are unchanged.
- **Usage / trust model**: single local user, localhost-only. Same trust
  model as the existing TUI (whoever has access to the machine/terminal).
  No authentication in v1.
- **Frontend stack**: a static React (Vite + TypeScript) app, built ahead of
  time and embedded into the `agent-cli` binary. `arlo --web` remains a
  single binary with no separate Node process at runtime.
- **Interaction scope**: full chat + control UI — type prompts, see
  streaming output, approve/deny tool calls, and watch task/sub-agent status,
  all from the browser. Not a read-only dashboard.
- **Wire protocol**: [AG-UI](https://docs.ag-ui.com) event vocabulary over a
  single WebSocket per browser tab, rather than inventing a bespoke protocol.
  No mature server-side Rust SDK for AG-UI exists yet (`ag-ui-core` /
  `ag-ui-client` on crates.io are 0.1.0, client/HTTP-SSE only, no WebSocket,
  no server helpers) — we hand-roll a small `AguiEvent` enum in Rust that
  serializes to the same JSON shape AG-UI defines, so the frontend can use
  any AG-UI-compatible client tooling later if desired.

## Non-goals (v1)

- No authentication/authorization — do not expose the server beyond
  localhost.
- No multi-tab session sharing — a second WebSocket connection replaces the
  session driver for the first (documented limitation, not solved here).
- No reconnect-with-replay of in-flight streaming text — on reconnect the
  client gets a fresh snapshot (conversation history + task/todo state), not
  a byte-exact resume of a partial assistant message.
- No changes to the TUI or single-prompt mode.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│ Browser (React + TS, static build)                               │
│  - chat pane (streamed text, tool call cards)                    │
│  - approval banner (permission_request → allow/deny)             │
│  - sidebar: Tasks/Sub-agents panel, Todos panel                  │
└───────────────────────────▲───────────────────────────┬──────────┘
                             │ AG-UI events (JSON)        │ client msgs
                             │ over WebSocket              │ (JSON)
┌───────────────────────────┴───────────────────────────▼──────────┐
│ crates/agent-cli/src/web/                                         │
│  server.rs   — axum app: serves embedded static assets, /ws       │
│  session.rs  — per-connection driver (like tui/mod.rs)            │
│  agui.rs     — AguiEvent enum + From<RunEvent>                    │
│  ws_approval.rs — WebApprovalHandler: ApprovalHandler              │
└───────────────────────────┬────────────────────────────┬──────────┘
                             │ run_stream()                │ TaskStore
                             ▼                              ▼
                    crates/agent-core (unchanged: run loop, permission
                    engine, TaskStore, SessionStore)
```

The web mode reuses `agent-core` exactly as the TUI does: same `Agent`
builder, same `RunConfig`, same `TaskStore`/`SessionStore` wiring. Only a new
consumer is added at the `agent-cli` layer, following the pattern already
established by `tui/`.

### `crates/agent-cli/src/web/`

- **`server.rs`** — builds the axum `Router`: `GET /` and static asset routes
  serve the embedded frontend build (via `rust-embed`), `GET /ws` upgrades to
  a WebSocket and hands the socket to `session.rs`. Binds to
  `127.0.0.1:<port>` only (default port 8787, `--port` overrides).
- **`session.rs`** — the per-connection driver loop, structurally the web
  analogue of `tui/mod.rs::run_tui_repl`:
  - On connect: send a `MessagesSnapshot` (resumed history, if any) and a
    `Custom` `arlo.task_snapshot` / `arlo.todo_snapshot` with current state.
  - Reads client messages (`user_message`, `approval_response`, `abort`)
    from the WebSocket.
  - On `user_message`: appends to history, builds `Input::Items`, calls
    `run_stream()`, forwards each `RunEvent` out as one or more `AguiEvent`s.
  - On `approval_response`: forwards to the `WebApprovalHandler`'s response
    channel (same pattern as `InteractiveApprovalHandler`).
  - On `abort`: aborts the current stream-forwarding task (same as the TUI's
    `AbortRun`).
  - Runs a ~500ms ticker that queries `TaskStore::list_tasks` /
    `list_todos` and pushes fresh `arlo.task_snapshot` / `arlo.todo_snapshot`
    events whenever the content changed since the last tick. Unlike the
    TUI's Idle-gated notification poller (which exists to protect the
    exactly-once task-result acknowledgment path), this ticker only reads
    task/todo state for display — it never calls `acknowledge_task`, so it
    can run unconditionally without risking double-delivery of a task
    result to the model.
- **`agui.rs`** — `AguiEvent` enum, serde-tagged to match the AG-UI JSON
  event shapes (`type` field values matching AG-UI's names), plus
  `From<RunEvent> for Vec<AguiEvent>` (some `RunEvent`s expand to multiple
  AG-UI events, e.g. one `StreamChunk` is `TextMessageStart` (once) +
  `TextMessageContent` (each chunk) + `TextMessageEnd` (on the next
  non-text event)).
- **`ws_approval.rs`** — `WebApprovalHandler`, an `ApprovalHandler` impl
  identical in shape to `tui/approval.rs::InteractiveApprovalHandler`
  (mpsc request/response channels), except the request side is serialized
  as an `AguiEvent::Custom` permission-request event instead of a Rust
  struct sent over an in-process channel to the TUI.

### CLI wiring

- `parse_args_from` gains `--web` (bool flag) and `--port <N>` (default
  8787).
- `main.rs`'s mode dispatch gains a third branch: `--web` present → build
  the same `Agent`/tools/`TaskStore`/`SessionStore` as the TUI branch does,
  then call `web::run_web_server(...)` instead of `tui::run_tui_repl(...)`.
  `--web` combined with a positional prompt is an argument error (mirrors
  existing mutual-exclusion checks for `--resume`/`--list-sessions`).

## Wire protocol

### Server → client (AG-UI events, JSON, one per WebSocket text frame)

`RunEvent` → `AguiEvent` mapping:

| arlo `RunEvent` | AG-UI event(s) |
|---|---|
| `TurnStart{turn,agent}` | `StepStarted{stepName: agent}` |
| `StreamChunk` | `TextMessageStart{messageId,role:"assistant"}` (first chunk only) → `TextMessageContent{messageId,delta}` (each chunk) → `TextMessageEnd{messageId}` (emitted when the next non-StreamChunk event arrives) |
| `ToolStart{id,name}` | `ToolCallStart{toolCallId:id,toolCallName:name}` |
| `ToolEnd{id,name,output,is_error}` | `ToolCallEnd{toolCallId:id}` → `ToolCallResult{toolCallId:id,content:output,role:"tool"}` (an `is_error` result is flagged via a `Custom` sibling event, since core AG-UI `ToolCallResult` has no error field) |
| `Compaction{stage,messages_removed}` | `Custom{name:"arlo.compaction", value:{stage,messages_removed}}` |
| `StepResolved` | not forwarded (internal detail) |
| `AgentEnd{agent,output,usage}` | `RunFinished{outcome:{type:"success"}, result:{output,usage}}` |
| `Interruption{pending}` | one `Custom{name:"arlo.permission_request", value:{callId,toolName,toolInput,options}}` per pending call, `options` = `["allow_once","allow_always","reject_once","reject_always"]` (naming borrowed from ACP's permission option kinds, since AG-UI itself only has a coarse `interrupt` outcome) |
| `GuardrailTripped{name,reason}` | `RunError{message,code:"guardrail:"+name}` |
| `MaxTurns{count}` | `RunFinished{outcome:{type:"success"}, result:{maxTurns:count}}` (loop ended, not an error) |
| `Aborted{reason}` | `RunError{message:reason,code:"aborted"}` |
| `Error{error}` | `RunError{message:error}` |

Plus arlo-specific `Custom` events not derived from `RunEvent`:

- `arlo.task_snapshot` — `value: Vec<TaskEntry>` (from `TaskStore::list_tasks(None)`), pushed on connect and on the polling ticker when changed.
- `arlo.todo_snapshot` — `value: Vec<TodoItem>` (from `TaskStore::list_todos()`), same cadence.
- `MessagesSnapshot{messages}` — sent once on connect with the resumed session history (empty array for a fresh session).

### Client → server (JSON, one per WebSocket text frame)

```
{"type":"user_message","text":"..."}
{"type":"approval_response","responses":[{"callId":"...","decision":"allow_once"|"allow_always"|"reject_once"|"reject_always","pattern":"..."}]}
{"type":"abort"}
```

`approval_response.responses` maps to `Vec<ApprovalResponse>` (`allow_once`→`Allow`, `reject_once`/`reject_always`→`Deny`, `allow_always`→`AlwaysAllow{pattern}`); `reject_always` is accepted from the client but currently has no distinct backend behavior beyond `Deny` (no static-deny-list mutation), matching today's `ApprovalResponse` enum.

## Frontend

- Vite + React + TypeScript, no additional state management library needed
  at this scope (component state + a small event-reducer hook per
  WebSocket message is enough).
- Views:
  - **Chat pane** — renders `TextMessage*` as streamed assistant bubbles,
    user messages from local echo, tool calls as collapsible cards
    (`ToolCallStart`→pending, `ToolCallResult`→result/error).
  - **Approval banner** — appears when an `arlo.permission_request` custom
    event arrives; buttons for the four decisions; sends
    `approval_response` and dismisses.
  - **Sidebar: Tasks & Sub-agents** — renders the latest `arlo.task_snapshot`
    grouped by `TaskStatus` (Running/Pending/Completed/Failed/Killed), same
    grouping the TUI's `/tasks` command uses.
  - **Sidebar: Todos** — renders the latest `arlo.todo_snapshot` with
    Pending/InProgress/Completed indicators, mirroring `/todos`.
- Build output lands in `web/dist/`, embedded into the `agent-cli` binary at
  compile time via `rust-embed`. New `make web-build` target runs
  `npm ci && npm run build` in `web/`; `make build`/`make check` do not
  depend on it (the embedded assets are checked into a build artifact step
  only when `--web` support is being built — CI runs `make web-build` before
  `cargo build` when packaging the release binary).

## Error handling

- WebSocket disconnects mid-run: the stream-forwarding task is aborted
  (same as TUI's `AbortRun`), and any pending approval is denied gracefully
  (mirrors `InteractiveApprovalHandler`'s channel-closed behavior) so the
  run doesn't hang waiting for a response that will never come.
- A second WebSocket connection while one is active: the new connection
  takes over as the session driver; the old connection is closed server-side
  with a close frame explaining why. (No multi-tab sync in v1.)
- Malformed client JSON: server sends a `RunError`-shaped event describing
  the parse failure and ignores the message; the connection stays open.

## Testing

- Unit tests for `agui.rs`'s `RunEvent → AguiEvent` conversion (one test per
  mapping row above), following the existing colocated `#[cfg(test)]`
  convention.
- Unit tests for `ws_approval.rs::WebApprovalHandler`, mirroring the existing
  `tui/approval.rs` test suite (channel closed → deny all, response
  round-trip, `AlwaysAllow`).
- An integration test in `crates/agent-cli/tests/` that spins up the axum
  server on an ephemeral port, connects a test WebSocket client, sends a
  `user_message`, and asserts the expected AG-UI event sequence using a
  mock `Model`/`ModelProvider` (reusing the existing mocks from
  `run_loop.rs` tests where possible).
- No frontend test framework introduced in v1; manual verification via
  `make run -- --web` plus a browser, per the `verify` skill, before calling
  the feature complete.
