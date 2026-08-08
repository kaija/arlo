# Arlo in Slack

A Slack bot front-end for Arlo. A small Python service (`slack-proxy`) holds a
Slack **Socket Mode** websocket, replays the thread to a stateless
`arlo --serve` instance over AG-UI, and streams the answer back into the thread.

```
Slack  ⇄  slack-proxy  ⇄  arlo --serve
        websocket        HTTP + SSE
```

No inbound ports, no database, no host mounts. **Slack is the conversation
store** — the proxy re-reads the thread on every turn, which is also what makes
"@arlo, what did we decide?" work with no memory of its own.

> [!WARNING]
> **Serve mode executes tools without approval.** `Surface::Serve` maps to
> `PermissionMode::Bypass` (`crates/agent-cli/src/assembly.rs`), so anyone who
> can mention the bot can cause shell commands, file writes, and web fetches to
> run inside the `arlo` container. The mitigation is containment, not
> authorization: disposable container, no host mounts, no Docker socket,
> non-root, read-only rootfs, tmpfs workspace, memory and pid limits.
>
> Prompt injection is live — channel history is untrusted input and the agent
> can fetch the web. Do not invite the bot to channels containing secrets.
> `SLACK_ALLOWED_CHANNELS` is the kill switch for an unexpected invite.

## 1. Create the Slack app

From <https://api.slack.com/apps> → **Create New App** → **From an app
manifest**, paste this:

```yaml
display_information:
  name: Arlo
  description: Arlo agent
features:
  bot_user:
    display_name: Arlo
    always_online: false
oauth_config:
  scopes:
    bot:
      - app_mentions:read
      - chat:write
      - channels:history
      - groups:history
      - im:history
      - reactions:write
      - users:read
settings:
  event_subscriptions:
    bot_events:
      - app_mention
      - message.im
      - message.channels
      - message.groups
  interactivity:
    is_enabled: false
  socket_mode_enabled: true
  org_deploy_enabled: false
  token_rotation_enabled: false
```

Then:

1. **Basic Information → App-Level Tokens** → generate a token with the
   `connections:write` scope. That is `SLACK_APP_TOKEN` (`xapp-…`).
2. **Install App** → install to the workspace. That gives you the **Bot User
   OAuth Token**, `SLACK_BOT_TOKEN` (`xoxb-…`).
3. Invite the bot to a channel: `/invite @Arlo`.

`channels:history` is the scope an admin will question. It is required because
thread follow-ups arrive as ordinary `message.channels` events; the proxy
discards everything that is not a threaded message in a thread it has already
posted in. The alternative is requiring an @-mention on every turn.

## 2. Run it

```bash
cp .env.example .env   # fill in the two Slack tokens and a model provider key
docker compose up --build
```

The first build compiles the Rust workspace — expect several minutes. Two
services come up: `arlo` (no published ports, reachable only at
`http://arlo:8080/` on the compose network) and `slack-proxy` (no ports at all).

## 3. Use it

| In Slack | What happens |
|---|---|
| `@Arlo what did we decide about SSE?` in a channel | Reads the last 20 channel messages for context, replies in a thread |
| Any reply in that thread | Continues the conversation — no mention needed |
| A DM to the bot | Same, no mention needed |
| A second message while a run is going | Gets a ⏳, runs when the first finishes |

The reply is posted immediately as `_thinking…_`, updated about once a second as
text streams in, shows `_running {tool}…_` while a tool runs, and is finalized
when the run ends. Answers over 3800 characters continue as further replies.

## Configuration

Everything is environment variables; see [.env.example](.env.example).

| Variable | Default | Purpose |
|---|---|---|
| `SLACK_BOT_TOKEN` | — | `xoxb-…`, required |
| `SLACK_APP_TOKEN` | — | `xapp-…`, required |
| `ARLO_MODEL` | — | passed to `arlo --model`, e.g. `openai:gpt-4o` |
| `ARLO_URL` | `http://arlo:8080/` | set by compose; do not put it in `.env` |
| `CHANNEL_HISTORY_LIMIT` | `20` | messages read on a top-level mention |
| `THREAD_HISTORY_LIMIT` | `100` | thread replies read per turn |
| `HISTORY_CHAR_BUDGET` | `12000` | trim threshold, oldest-first |
| `RUN_TIMEOUT_SECONDS` | `300` | abandon the SSE stream past this |
| `MAX_CONCURRENT_RUNS` | `4` | global cap; extra runs queue |
| `UPDATE_INTERVAL_SECONDS` | `1.0` | `chat.update` throttle |
| `MAX_MESSAGE_CHARS` | `3800` | chunk size |
| `SLACK_ALLOWED_CHANNELS` | *(empty = all joined)* | comma-separated channel ids |
| `SLACK_ALLOWED_USERS` | *(empty = all)* | comma-separated user ids |
| `SHUTDOWN_GRACE_SECONDS` | `30` | drain window on SIGTERM |
| `LOG_LEVEL` | `INFO` | |

Nothing is logged but ids, counts and durations — no message text, no tool
arguments, no token values.

## Troubleshooting: the bot stays silent

Set `LOG_LEVEL=DEBUG` in `.env` and `docker compose up --build` (without
`--build` you keep running the old image). Every inbound event is then logged
with the reason it was or was not acted on:

```
DEBUG event type=app_mention channel=C1 ts=… thread_ts=None channel_type=channel mention=True
INFO  run started thread=C1:… run=…
```

| What you see | Meaning |
|---|---|
| No `event …` line at all | Slack is not delivering. Check Event Subscriptions are set, the app was **reinstalled** after adding scopes, Socket Mode is on, and the bot was invited to the channel. |
| `ignored (…)` | The reason is printed — subtype, no mention, allowlist, duplicate. |
| `run started` but nothing appears in Slack | `chat:write` is missing, or the bot is not in the channel. |
| `run started` then `Could not reach Arlo` | The `arlo` container is down or `ARLO_URL` is wrong. |

To check the Arlo half on its own, from the proxy container:

```bash
docker compose exec slack-proxy python -c "import httpx;print(httpx.post('http://arlo:8080/',json={'threadId':'t','runId':'r','messages':[{'id':'1','role':'user','content':'hi'}],'state':None,'tools':[],'context':[],'forwardedProps':{}},headers={'Accept':'text/event-stream'},timeout=60).text[:800])"
```

## Development

```bash
uv venv --python 3.13 .venv && uv pip install -e ".[dev]"
.venv/bin/python -m pytest
```

The unit tests cover the pieces with real algorithmic risk — mrkdwn conversion
and chunking, history reconstruction, the SSE render loop, and the event
filters — with no live Slack and no live Arlo.

### Manual smoke check

Mocking a whole SSE agent run costs more than it catches, so the end-to-end path
is checked by hand against a local Arlo:

```bash
cargo run --bin arlo -- --model openai:gpt-4o --serve 127.0.0.1:8080
```

```bash
SLACK_BOT_TOKEN=xoxb-... SLACK_APP_TOKEN=xapp-... ARLO_URL=http://127.0.0.1:8080/ .venv/bin/arlo-slack
```

Then mention the bot in a channel it has been invited to.

## Files

| File | Contents |
|---|---|
| [`app.py`](src/arlo_slack/app.py) | Bolt wiring, event filters, per-thread run registry |
| [`history.py`](src/arlo_slack/history.py) | Slack → AG-UI `messages` array |
| [`arlo.py`](src/arlo_slack/arlo.py) | AG-UI client: POST + SSE iteration |
| [`render.py`](src/arlo_slack/render.py) | SSE events → throttled Slack edits |
| [`mrkdwn.py`](src/arlo_slack/mrkdwn.py) | Markdown → Slack mrkdwn, chunking |
| [`requirements.md`](requirements.md), [`design.md`](design.md), [`tasks.md`](tasks.md) | the spec this was built from |

## Not supported

Approval buttons (serve mode has no approval gate), slash commands, per-thread
model overrides, host repository mounts, more than one proxy replica, and
rendering tool output into Slack. See the "Out of Scope" table in
[requirements.md](requirements.md) for the trigger that would justify each.
