# arlo

An LLM agent framework in Rust: a streaming run loop with tools, human-in-the-loop
permissions, sub-agents, background tasks, context compaction, and a terminal UI.

## Features

- **Agentic run loop** — turn-based loop (compact → request → stream → execute tools →
  resolve next step) with typed state transitions and error recovery
- **Human-in-the-loop permissions** — 4-layer permission engine with glob patterns
  (`shell(npm*)`, `file_write(/tmp/*)`), static allow/deny lists, and interactive approval
- **Sub-agents** — spawn foreground or background sub-agents with isolated histories;
  background results are delivered back to the parent exactly once
- **Built-in tools** — file read/write/edit, glob, grep, shell, web fetch, web search
- **MCP client** — connect external tool servers via the Model Context Protocol
- **Context compaction** — 3-layer pipeline (tool-result compaction → session memory →
  full summarization) to stay within the context window
- **Provider-agnostic** — OpenAI-compatible and Anthropic HTTP providers with custom
  `base_url` support (works with local model servers), configurable via profiles
- **TUI** — interactive REPL with streaming output and approval prompts

## Workspace layout

| Crate | Purpose |
|---|---|
| `agent-core` | Run loop, permissions, tools trait, sub-agents, task store, compaction |
| `agent-llm` | LLM providers (OpenAI-compatible, Anthropic), retry, model overrides |
| `agent-tools` | Built-in tools: file ops, glob, grep, shell, web fetch/search |
| `agent-mcp` | MCP client |
| `agent-cli` | The `arlo` binary: TUI, agent wiring |

## Quick start

Requires stable Rust (install via [rustup](https://rustup.rs)).

```bash
git clone <repo-url>
cd arlo-rust
cargo build --release
```

Set credentials for your provider and run:

```bash
export OPENAI_API_KEY="sk-..."
# optional: point at any OpenAI-compatible server
# export OPENAI_BASE_URL="http://localhost:8000/v1"

cargo run -p agent-cli                          # REPL
cargo run -p agent-cli -- "summarize this repo" # one-shot prompt
```

CLI usage:

```
arlo [--model PROVIDER:MODEL] [--profile NAME] [--dump-prompt] ["prompt"]
```

No prompt starts the interactive REPL.

## Configuration

Runtime config lives in `.arlo/settings.json` (project) and `~/.arlo/settings.json`
(user); project settings win. Two sections:

- **`permissions`** — `allow`/`deny` tool patterns, e.g. `"shell(cargo *)"`,
  `"web_fetch(https://docs.rs/*)"`
- **`profiles`** — named LLM provider configs (model, base URL, context window,
  max output tokens), selected with `--profile`

Skills live in `.arlo/skills/` (project) and `~/.arlo/skills/` (user).

See [doc/configuration.md](doc/configuration.md) for the full reference.

## Documentation

- [doc/configuration.md](doc/configuration.md) — settings, permission patterns, profiles
- [doc/agent-framework.md](doc/agent-framework.md) — full architecture deep-dive
- [doc/sub-agent-task-coordination.md](doc/sub-agent-task-coordination.md) — sub-agent
  and background-task design, sequence diagrams, known limits
- [AGENTS.md](AGENTS.md) — codebase map and invariants (also read by coding agents)

## Development

```bash
make check   # cargo check --workspace (fast)
make test    # cargo test --workspace
make lint    # clippy, warnings are errors
make fmt     # rustfmt
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to contribute.

## License

Licensed under the [MIT License](LICENSE).
