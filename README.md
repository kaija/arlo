# Arlo

**Arlo** is an open-source, Rust-native agentic AI framework — built as a leaner,
more general-purpose take on what tools like Claude Code offer. It gives you a
streaming agent run loop with tool use, human-in-the-loop permissions, sub-agents,
background tasks, MCP integration, and Agent Skills, exposed through multiple
interfaces (interactive TUI, one-shot CLI prompts, and an embeddable library).

Where Claude Code is a product tied to one provider, Arlo is a small, provider-agnostic
core you can point at Anthropic, OpenAI-compatible APIs, or your own local model
server — and embed in whatever surface you need.

> 🇹🇼 [繁體中文版 README](README.zh-TW.md) ・ 🇯🇵 [日本語版 README](README.ja.md)

## Why Arlo

- **Minimal, not minimal-featured** — a small Rust core (5 crates) instead of a
  monolithic app; every piece (tools, providers, permissions) is swappable.
- **Provider-agnostic** — Anthropic and OpenAI-compatible HTTP providers out of the
  box, with custom `base_url` support for local/self-hosted models.
- **Multiple interfaces** — interactive TUI, one-shot CLI, and a Rust library you can
  embed directly in your own application.

## Features

| Feature | Description |
|---|---|
| **Agentic run loop** | Turn-based loop (compact → request → stream → execute tools → resolve next step) with typed state transitions and automatic error recovery |
| **MCP client** | Connect external tool servers via the Model Context Protocol (MCP) alongside built-in tools |
| **Agent Skills** | Discover and load Markdown-defined skills from project-level (`.arlo/skills/`) and user-level (`~/.arlo/skills/`) directories, with template variable substitution |
| **Autonomous agent loop** | The agent plans, calls tools, and iterates on its own between turns, streaming progress events (`TurnStart`, `ToolStart`/`ToolEnd`, `StepResolved`, …) |
| **Human-in-the-loop permissions** | 4-layer permission engine — glob patterns (`shell(npm *)`, `file_write(/tmp/*)`), static allow/deny lists, and interactive approval prompts |
| **Task management & sub-agents** | Built-in Task/Todo store; spawn foreground or background sub-agents with isolated histories — background results are delivered back to the parent exactly once |
| **Built-in tools** | File read/write/edit, glob, grep, shell, web fetch, web search |
| **Context compaction** | 3-layer pipeline — tool-result compaction → session memory → full summarization — to stay within the context window |
| **Multiple interfaces** | Interactive TUI, one-shot CLI prompt, and embeddable Rust crates |

## Workspace layout

| Crate | Purpose |
|---|---|
| `agent-core` | Run loop, permissions, tools trait, sub-agents, task store, skills, compaction |
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

cargo run -p agent-cli                          # interactive TUI
cargo run -p agent-cli -- "summarize this repo" # one-shot prompt
```

```
arlo [--model PROVIDER:MODEL] [--profile NAME] [--dump-prompt] ["prompt"]
```

No prompt starts the interactive REPL.

## Configuration

Runtime config lives in `.arlo/settings.json` (project) and `~/.arlo/settings.json`
(user); project settings win.

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
