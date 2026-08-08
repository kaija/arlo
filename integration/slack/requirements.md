# Requirements Document: Slack Integration

## Introduction

This feature adds a Slack bot front-end for Arlo. A Python proxy service holds a
Slack **Socket Mode** websocket, translates Slack conversations into AG-UI
`RunAgentInput` requests against a running `arlo --serve` instance, and renders
the resulting SSE event stream back into Slack messages.

The proxy is deployed as its own container alongside an Arlo container. It owns
no database: **Slack itself is the conversation store**, re-read on every turn.
This satisfies the "read chat history" requirement and the "replay the transcript
to a stateless agent" requirement with one code path.

Arlo's serve mode is stateless with respect to conversation — `ArloBridge`
rebuilds the prompt from the `messages` array on every request
(`crates/agent-cli/src/serve/bridge.rs`) — so the proxy MUST supply the full
transcript each turn. Arlo's `thread_id` session only survives an interrupt and
is reaped after 10 minutes idle; the proxy does not depend on it.

## Glossary

- **Slack_Proxy**: The Python service. Holds the Socket Mode websocket, calls Arlo, posts to Slack.
- **Arlo_Server**: An `arlo --serve` process exposing `POST /` with an SSE response.
- **Session**: One logical conversation, identified by a Thread_Key. Has no stored state.
- **Thread_Key**: `{channel_id}:{thread_ts}` — the identity of a Session.
- **History_Builder**: The component that reads Slack and produces the AG-UI `messages` array.
- **Access_Filter**: The component that decides whether an inbound Slack event is allowed to start a run.
- **Run_Renderer**: The component that consumes Arlo's SSE stream and maintains the Slack reply message.
- **Message_Formatter**: The component that converts model Markdown to Slack `mrkdwn` and splits over-long output.
- **Run_Registry**: The in-process map of Thread_Key → per-thread lock, used for concurrency control.

## Requirements

### Requirement 1: Slack Connection

**User Story:** As an operator, I want the bot to connect to Slack without exposing a public endpoint, so that I can run it behind a firewall with no ingress, certificate, or tunnel.

#### Acceptance Criteria

1. THE Slack_Proxy SHALL connect to Slack using Socket Mode over a websocket, authenticated with an app-level token (`SLACK_APP_TOKEN`) and a bot token (`SLACK_BOT_TOKEN`).
2. THE Slack_Proxy SHALL NOT listen on any inbound network port.
3. IF either `SLACK_APP_TOKEN` or `SLACK_BOT_TOKEN` is absent or malformed at startup, THEN THE Slack_Proxy SHALL log a descriptive error and exit with a non-zero status.
4. WHEN the websocket disconnects, THE Slack_Proxy SHALL reconnect automatically with backoff and log each reconnect at `warning` level.
5. THE Slack_Proxy SHALL acknowledge every Socket Mode envelope within Slack's 3-second budget, independently of how long the resulting agent run takes.
6. WHEN the process receives SIGTERM or SIGINT, THE Slack_Proxy SHALL stop accepting new events, allow in-flight runs to finish (up to `SHUTDOWN_GRACE_SECONDS`, default 30), and exit.

### Requirement 2: Event Triggers

**User Story:** As a Slack user, I want to start a conversation by mentioning the bot and continue it by simply replying in the thread, so that I do not have to @-mention it on every turn.

#### Acceptance Criteria

1. WHEN an `app_mention` event arrives, THE Slack_Proxy SHALL start a Session.
2. WHEN a `message.im` event arrives from a human user, THE Slack_Proxy SHALL start or continue a Session.
3. WHEN a `message.channels` (or `message.groups`) event arrives that carries a `thread_ts` AND the bot has previously posted in that thread, THE Slack_Proxy SHALL continue the Session without requiring a mention.
4. IF a `message.*` event carries no `thread_ts` AND contains no mention of the bot, THEN THE Slack_Proxy SHALL ignore it.
5. THE Slack_Proxy SHALL ignore events authored by any bot, including its own (`bot_id` present or `user` equal to the bot's own user id), to prevent self-triggering loops.
6. THE Slack_Proxy SHALL ignore message subtypes that are not user messages (`message_changed`, `message_deleted`, `channel_join`, and similar).
7. WHEN a duplicate event is redelivered by Slack, THE Slack_Proxy SHALL process it at most once, keyed by `(channel, event_ts)`.

### Requirement 3: Session Identity

**User Story:** As a Slack user, I want each thread to be its own independent conversation, so that concurrent discussions do not bleed into each other.

#### Acceptance Criteria

1. THE Slack_Proxy SHALL derive the Thread_Key as `{channel_id}:{thread_ts}`, where `thread_ts` is the event's `thread_ts` if present, otherwise the event's own `ts`.
2. WHEN replying to a Session started by a top-level channel mention, THE Slack_Proxy SHALL post into the thread rooted at that mention, never to the channel top level.
3. THE Slack_Proxy SHALL pass the Thread_Key as the AG-UI `threadId` and a freshly generated UUID as the AG-UI `runId` on every request.
4. THE Slack_Proxy SHALL support an unbounded number of distinct Sessions, limited only by the concurrency controls in Requirement 8.

### Requirement 4: Conversation History

**User Story:** As a Slack user, I want the bot to already know what we were just talking about when I mention it, so that I do not have to restate context.

#### Acceptance Criteria

1. THE History_Builder SHALL reconstruct conversation history from Slack on every turn and SHALL NOT persist a transcript of its own.
2. WHEN a Session continues inside an existing thread, THE History_Builder SHALL fetch that thread via `conversations.replies`, up to `THREAD_HISTORY_LIMIT` (default 100) most recent messages.
3. WHEN a Session is started by a top-level mention, THE History_Builder SHALL additionally fetch the `CHANNEL_HISTORY_LIMIT` (default 20) most recent channel messages via `conversations.history` and include them as prior context ahead of the mention.
4. THE History_Builder SHALL map messages authored by the bot to AG-UI role `assistant`, and all other user messages to AG-UI role `user`.
5. THE History_Builder SHALL prefix each `user` message with the author's display name so the model can distinguish speakers in a multi-party thread.
6. THE History_Builder SHALL resolve user ids to display names via `users.info`, cached in-process for the lifetime of the process.
7. WHEN the reconstructed history exceeds `HISTORY_CHAR_BUDGET` (default 12000 characters), THE History_Builder SHALL drop the oldest messages until it fits, always retaining the triggering message.
8. THE History_Builder SHALL strip the bot's own `<@Uxxxx>` mention token from the triggering message text before sending it to Arlo.
9. THE History_Builder SHALL prepend a `system` message describing the Slack context (channel, that replies render in Slack, and a brevity instruction).
10. IF a Slack history API call fails, THEN THE Slack_Proxy SHALL proceed with only the triggering message and SHALL note the degraded context in its log output.

### Requirement 5: Agent Invocation

**User Story:** As a developer, I want the proxy to speak the AG-UI protocol correctly, so that it works against any `arlo --serve` instance without special-casing.

#### Acceptance Criteria

1. THE Slack_Proxy SHALL `POST` a JSON `RunAgentInput` body to `ARLO_URL` (default `http://arlo:8080/`) with `Accept: text/event-stream`.
2. THE Slack_Proxy SHALL serialize the request in camelCase (`threadId`, `runId`, `forwardedProps`) to match the AG-UI protocol wire format.
3. THE Slack_Proxy SHALL send an empty `tools` array, since Arlo ignores client-supplied tools and uses its own.
4. THE Slack_Proxy SHALL consume the response as a Server-Sent Events stream and process events incrementally rather than buffering the whole response.
5. IF the connection to Arlo_Server cannot be established, THEN THE Slack_Proxy SHALL report the failure in the Slack thread and log it at `error` level.

### Requirement 6: Response Rendering

**User Story:** As a Slack user, I want to see that the agent is working and watch the answer appear, so that a multi-minute run does not look like a hang.

#### Acceptance Criteria

1. WHEN a run starts, THE Run_Renderer SHALL immediately post a placeholder message in the thread and retain its `ts` for subsequent edits.
2. WHILE the run streams `TEXT_MESSAGE_CONTENT` events, THE Run_Renderer SHALL update the placeholder with the accumulated text at most once per `UPDATE_INTERVAL_SECONDS` (default 1.0).
3. WHEN a `TOOL_CALL_START` event arrives, THE Run_Renderer SHALL display a status line naming the running tool.
4. WHEN a `TOOL_CALL_END` event arrives, THE Run_Renderer SHALL clear that status line.
5. THE Run_Renderer SHALL NOT render tool arguments or tool output into Slack.
6. THE Run_Renderer SHALL ignore `STEP_STARTED` events, which Arlo emits per thinking-delta and per turn and which carry no user-facing value.
7. WHEN `RUN_FINISHED` arrives with outcome `success`, THE Run_Renderer SHALL perform a final update containing the complete answer with no status line.
7a. WHEN `RUN_ERROR` arrives with a message beginning `RUN_FINISHED with active steps`, THE Run_Renderer SHALL treat the run as successfully completed and apply criterion 7. (Arlo's `EventMapper` opens a `turn-N` step it never closes, so today this is the terminal event of every successful run — see design.md, "Upstream defect".)
8. IF the run produced no assistant text, THEN THE Run_Renderer SHALL replace the placeholder with an explicit "no output" notice rather than leaving the placeholder in place.
9. IF a `chat.update` call is rate-limited, THEN THE Run_Renderer SHALL honour the `Retry-After` header and SHALL NOT drop the pending text.

### Requirement 7: Message Formatting

**User Story:** As a Slack user, I want answers to render as readable Slack messages, so that formatting and long content are not mangled.

#### Acceptance Criteria

1. THE Message_Formatter SHALL convert model Markdown to Slack `mrkdwn` before every post or update: `**bold**` → `*bold*`, `*italic*`/`_italic_` → `_italic_`, `[text](url)` → `<url|text>`, `# Heading` → `*Heading*`.
2. THE Message_Formatter SHALL leave fenced code blocks byte-for-byte unmodified during conversion.
3. WHEN formatted output exceeds `MAX_MESSAGE_CHARS` (default 3800), THE Message_Formatter SHALL split it into consecutive thread replies.
4. THE Message_Formatter SHALL prefer to split on paragraph boundaries, then line boundaries, and SHALL NOT split inside a word.
5. WHEN a split falls inside a fenced code block, THE Message_Formatter SHALL close the fence at the end of the chunk and reopen it with the same language tag at the start of the next.
6. WHILE output spans multiple chunks, THE Run_Renderer SHALL apply streaming updates only to the final chunk; earlier chunks are posted once and not edited again.

### Requirement 8: Concurrency

**User Story:** As an operator, I want a busy workspace to degrade predictably, so that concurrent conversations neither garble each other nor overwhelm the Arlo container.

#### Acceptance Criteria

1. THE Run_Registry SHALL hold at most one in-flight run per Thread_Key.
2. WHEN a message arrives for a Thread_Key whose run is still in flight, THE Slack_Proxy SHALL add a ⏳ reaction to that message and queue it, starting it when the in-flight run completes.
3. THE Slack_Proxy SHALL remove the ⏳ reaction when the queued run starts.
4. THE Slack_Proxy SHALL process queued messages for a given Thread_Key in arrival order.
5. THE Slack_Proxy SHALL cap total concurrent runs across all Sessions at `MAX_CONCURRENT_RUNS` (default 4).
6. WHEN the global cap is reached, THE Slack_Proxy SHALL queue rather than reject, applying the same ⏳ reaction.

### Requirement 9: Error Handling

**User Story:** As a Slack user, I want failures reported in the thread, so that I know the bot broke rather than assuming it is still thinking.

#### Acceptance Criteria

1. WHEN a `RUN_ERROR` event arrives that is not the unclosed-step case in Requirement 6.7a, THE Run_Renderer SHALL replace the placeholder with the error message, preserving any partial text already streamed.
2. IF a run exceeds `RUN_TIMEOUT_SECONDS` (default 300), THEN THE Slack_Proxy SHALL abandon the SSE connection and report the timeout in the thread.
3. IF the SSE stream terminates without a `RUN_FINISHED` or `RUN_ERROR` event, THEN THE Run_Renderer SHALL report an interrupted-stream error and retain any partial text.
4. IF a `RUN_FINISHED` event arrives with outcome `interrupt`, THEN THE Slack_Proxy SHALL report that the agent requested approval and that approvals are unsupported, and SHALL NOT attempt to resume. (Arlo's serve mode uses `PermissionMode::Bypass`, so this is a misconfiguration signal.)
5. THE Slack_Proxy SHALL emit structured logs including Thread_Key, `runId`, event counts, and duration for every run.
6. THE Slack_Proxy SHALL NOT log message text, tool arguments, or token values.
7. IF an unhandled exception occurs while handling an event, THEN THE Slack_Proxy SHALL log it with a traceback, report a generic failure in the thread, and continue serving other events.

### Requirement 10: Access Control

**User Story:** As a workspace admin, I want to constrain where the bot will act, so that an unexpected channel invite does not grant unattended tool execution.

#### Acceptance Criteria

1. THE Access_Filter SHALL treat channel membership as the default gate: the bot acts in any channel it has been invited to.
2. WHEN `SLACK_ALLOWED_CHANNELS` is set to a non-empty comma-separated list, THE Access_Filter SHALL ignore every event from a channel not in that list.
3. WHEN `SLACK_ALLOWED_USERS` is set to a non-empty comma-separated list, THE Access_Filter SHALL ignore every event from a user not in that list.
4. WHEN both variables are empty or unset, THE Access_Filter SHALL allow all events from channels the bot is a member of.
5. WHEN an event is filtered out, THE Slack_Proxy SHALL log the decision at `info` level and SHALL NOT reply in Slack.

### Requirement 11: Deployment

**User Story:** As an operator, I want to bring the whole integration up with one command from a clean clone, so that setup is reproducible.

#### Acceptance Criteria

1. THE integration SHALL provide a `docker-compose.yml` under `integration/slack/` defining exactly two services: `arlo` and `slack-proxy`.
2. THE `arlo` service SHALL build from a multi-stage `Dockerfile` at the repository root and run `arlo --serve 0.0.0.0:8080`.
3. THE `slack-proxy` service SHALL build from `integration/slack/Dockerfile` and declare `depends_on: [arlo]`.
4. THE `arlo` service SHALL NOT publish any port to the host; it SHALL be reachable only over the compose network.
5. THE integration SHALL read all configuration from environment variables, documented in a committed `.env.example`.
6. THE integration SHALL NOT commit any token, key, or `.env` file.
7. THE `slack-proxy` project SHALL declare its dependencies in `pyproject.toml` targeting the latest stable Python.

### Requirement 12: Containment

**User Story:** As a security reviewer, I want the blast radius of an unattended tool call to be a disposable container, so that Bypass-mode permissions are acceptable.

#### Acceptance Criteria

1. THE `arlo` service SHALL run as a non-root user.
2. THE `arlo` service SHALL mount no host path and SHALL NOT mount the Docker socket.
3. THE `arlo` service SHALL run with a read-only root filesystem and a `tmpfs` mount for its working directory.
4. THE `arlo` service SHALL declare memory and pid limits.
5. THE `slack-proxy` service SHALL run as a non-root user with a read-only root filesystem.
6. THE documentation SHALL state plainly that serve mode executes tools without approval (`crates/agent-cli/src/assembly.rs` sets `PermissionMode::Bypass` for `Surface::Serve`), and that anyone who can mention the bot can trigger tool execution inside the `arlo` container.

## Out of Scope

Deliberately excluded from v1, with the trigger that would justify adding each:

| Excluded | Add when |
|---|---|
| Human-in-the-loop approval buttons | The bot gains a host mount or access to real infrastructure. Requires Rust changes: `Surface::Serve` hardwires `DenyAllApprovalHandler`, and `bridge.rs` cannot yet reattach to the event stream after a resume. |
| Slash commands (`/arlo reset`, `/arlo status`) | Users ask for them. "Reset" is close to meaningless while Slack is the store — starting a new thread already resets context. |
| `/healthz` endpoint and Prometheus metrics | This runs on orchestrated infrastructure with health-based restarts, rather than a single compose host. |
| Per-thread model override | More than one model is genuinely needed. Arlo fixes its model at assembly time and ignores `forwardedProps`, so this means a Rust change or one container per model. |
| Host repository mounts | The bot's purpose changes from Q&A to codebase work. Re-open Requirement 12 and HITL together at that point. |
| Multiple proxy replicas | One replica cannot keep up. The per-thread lock is in-process and would need to move to shared state. |
| Rendering tool output in Slack | Users need to audit what the agent ran. |
