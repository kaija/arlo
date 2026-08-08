# Arlo Web Chat

A browser chat front-end for Arlo that proves the AG-UI interface works end-to-end — streaming text, tool call visibility, and human-in-the-loop tool approval — against Arlo's own AG-UI server from a real browser client.

## Quick start

1. Copy `.env.example` to `.env` and fill in your credentials:

   ```bash
   cp .env.example .env
   # Edit .env and set OPENAI_API_KEY and ARLO_MODEL
   ```

2. Start both services:

   ```bash
   docker compose --env-file .env up --build
   ```

3. Open http://localhost:3000 in your browser.

## Configuration

Copy `.env.example` to `.env` and set:

| Variable | Required | Description |
|---|---|---|
| `OPENAI_API_KEY` | Yes* | OpenAI API key |
| `ANTHROPIC_API_KEY` | Yes* | Anthropic API key (alternative to OpenAI) |
| `OPENAI_BASE_URL` | No | Custom OpenAI-compatible endpoint |
| `ARLO_MODEL` | Yes | Model name, e.g. `openai:gpt-4o` |

*At least one provider key is required.

`ARLO_URL` is set in `compose.yaml` (`http://arlo:8080`) and must **not** be added to `.env` — it stays server-side and never reaches the browser.

## How it works

The browser talks to the Next.js app; the Next.js app talks to Arlo's AG-UI server. Arlo never exposes a port to the host — only port 3000 (Next.js) is published.

```
Browser → Next.js (port 3000) → Arlo (internal port 8080)
```

When the agent needs to run a tool that requires approval, a card appears inline in the conversation. Click **Allow once**, **Always allow**, or **Deny** to continue.

## Session expiry

Pending approval requests expire after **10 minutes**. If you wait too long:
- The approval card disables its buttons and shows "Session expired"
- Restarting the conversation (reload the page) is required
- On the server, the reaped session denies all pending approvals and abandons the run

## Disabling approval (`--yolo`)

The `arlo` service in `compose.yaml` runs **without** `--yolo`. To skip all permission checks (useful for clients that can't answer approvals), add `--yolo` to the command:

```yaml
command: ["arlo", "--serve", "0.0.0.0:8080", "--model", "${ARLO_MODEL}", "--yolo"]
```

With `--yolo`, the approval card is never shown and all tools run automatically.

## Security

No authentication is required. Anyone who can reach port 3000 can drive the agent and approve its tool calls.

- `ARLO_URL` stays server-side — the browser has no direct route to Arlo
- Arlo runs in a read-only container with a tmpfs working directory, no host mounts, no Docker socket, and pid/memory limits — the blast radius of an approved tool call is limited
- Nothing is persisted: no transcript, no credential, no tool output survives a page reload or container restart
