# Implementation Plan: Slack Integration

## Overview

Build the `slack-proxy` Python service bottom-up: pure formatting first (no I/O,
fully testable), then the AG-UI client, then the renderer that consumes it, then
the Slack wiring that drives everything, then packaging and deployment.

Dependency chain: `mrkdwn` → `arlo` → `render` → `history` → `app` → packaging.

## Tasks

- [x] 1. Project skeleton and packaging
  - [x] 1.1 Create `pyproject.toml` (src layout, hatchling, `requires-python >=3.12`)
    - Runtime deps: `slack-bolt`, `aiohttp` (socket mode transport), `httpx`, `httpx-sse`
    - Dev deps: `pytest`, `pytest-asyncio`
    - _Requirements: 11.7_
  - [x] 1.2 Create `src/arlo_slack/__init__.py` and add `/integration/slack/.env` to the repo `.gitignore`
    - _Requirements: 11.6_

- [x] 2. `mrkdwn.py` — Message_Formatter
  - [x] 2.1 `to_mrkdwn(text)`: split on fenced code blocks, convert only non-code segments
    - Single-pass alternation regex for links / bold / italic so converted output is never re-converted (`**x**` must not degrade to `_*x*_`), headings applied after
    - _Requirements: 7.1, 7.2_
  - [x] 2.2 `chunk(text, limit)`: split preferring `\n\n`, then `\n`, then space; never mid-word
    - Track fence state across boundaries: close with ``` and reopen with the same language tag
    - _Requirements: 7.3, 7.4, 7.5_
  - [x] 2.3 Tests: bold-before-italic ordering, fences untouched, links, headings, chunk limit, no mid-word split, rejoin equals input modulo inserted fences, fence-crossing reopen
    - _Requirements: 7.1–7.5_

- [x] 3. `arlo.py` — AG-UI client
  - [x] 3.1 `stream(messages, thread_id, run_id)` async generator: POST camelCase `RunAgentInput` with `Accept: text/event-stream`, yield parsed SSE events
    - Empty `tools` array; incremental iteration, no buffering
    - _Requirements: 5.1, 5.2, 5.3, 5.4_

- [x] 4. `render.py` — Run_Renderer
  - [x] 4.1 Post placeholder, consume events, throttled `chat.update` on a monotonic-clock check
    - _Requirements: 6.1, 6.2_
  - [x] 4.2 Event table: text accumulation, tool status line on START, cleared on END, `STEP_STARTED`/`TOOL_CALL_ARGS` ignored, no tool args or output rendered
    - _Requirements: 6.3, 6.4, 6.5, 6.6_
  - [x] 4.3 Terminal handling: `RUN_FINISHED` success final flush; `RUN_ERROR` starting `RUN_FINISHED with active steps` treated as success; other `RUN_ERROR` preserves partial text; `interrupt` outcome reports unsupported approvals; missing terminal event reports interrupted stream; empty output gets an explicit notice
    - _Requirements: 6.7, 6.7a, 6.8, 9.1, 9.3, 9.4_
  - [x] 4.4 Multi-chunk output: only changed chunks are edited, earlier chunks posted once
    - _Requirements: 7.6_
  - [x] 4.5 429 handling: honour `Retry-After`, retry, never drop buffered text; run timeout abandons the stream and reports in-thread; transport failure reported in-thread
    - _Requirements: 6.9, 9.2, 5.5_
  - [x] 4.6 Tests driven by canned AG-UI event lists and a fake Slack client
    - _Requirements: 6.2, 6.6, 6.7a, 9.1, 9.3_

- [x] 5. `history.py` — History_Builder
  - [x] 5.1 `thread_replies()` helper (also used by app.py for thread-participation detection)
    - _Requirements: 2.3, 4.2_
  - [x] 5.2 `build()`: thread replies for in-thread turns, channel history for top-level mentions, role mapping, display-name prefixing, `users.info` cache
    - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5, 4.6_
  - [x] 5.3 Mention stripping, leading `system` message, oldest-first trim to `HISTORY_CHAR_BUDGET` pinning the trigger, degrade to trigger-only on API failure
    - _Requirements: 4.7, 4.8, 4.9, 4.10_
  - [x] 5.4 Tests with canned Slack payloads
    - _Requirements: 4.4, 4.5, 4.7, 4.8, 4.10_

- [x] 6. `app.py` — wiring, Access_Filter, Run_Registry
  - [x] 6.1 Config loading and startup token validation (log + non-zero exit on missing/malformed)
    - _Requirements: 1.3, 11.5_
  - [x] 6.2 Socket Mode connection, no inbound port, auto-ack, reconnect logging
    - _Requirements: 1.1, 1.2, 1.4, 1.5_
  - [x] 6.3 Event triggers and ignore rules: `app_mention`, `message.im`, threaded `message.channels`/`message.groups`, bot-authored, subtypes, `(channel, event_ts)` dedup
    - _Requirements: 2.1–2.7_
  - [x] 6.4 Thread follow-up detection via bot participation in the fetched thread
    - _Requirements: 2.3_
  - [x] 6.5 Thread_Key derivation, in-thread replies, `threadId`/`runId` on every request
    - _Requirements: 3.1, 3.2, 3.3, 3.4_
  - [x] 6.6 Access_Filter: `SLACK_ALLOWED_CHANNELS` / `SLACK_ALLOWED_USERS`, membership default, log-and-drop
    - _Requirements: 10.1–10.5_
  - [x] 6.7 Run_Registry: per-thread lock + global semaphore, ⏳ on queue, removed on start, FIFO per thread
    - _Requirements: 8.1–8.6_
  - [x] 6.8 Structured run logs (thread key, runId, event count, duration), no message text/args/tokens; unhandled exceptions logged with traceback and reported generically
    - _Requirements: 9.5, 9.6, 9.7_
  - [x] 6.9 SIGTERM/SIGINT drain up to `SHUTDOWN_GRACE_SECONDS`
    - _Requirements: 1.6_

- [x] 7. Deployment and documentation
  - [x] 7.1 `Dockerfile` for slack-proxy: non-root, read-only-compatible
    - _Requirements: 11.3, 12.5_
  - [x] 7.2 `docker-compose.yml`: `arlo` + `slack-proxy` only, root Dockerfile with `context: ../..`, no published ports, non-root, read-only rootfs, tmpfs workspace, mem/pid limits
    - _Requirements: 11.1, 11.2, 11.4, 12.1, 12.2, 12.3, 12.4_
  - [x] 7.3 `.env.example` documenting every variable
    - _Requirements: 11.5, 11.6_
  - [x] 7.4 `README.md`: Slack app manifest, scopes, `compose up`, manual smoke check, and a plain statement of the unattended-tool-execution posture
    - _Requirements: 12.6_

- [x] 8. Checkpoint — run the test suite
