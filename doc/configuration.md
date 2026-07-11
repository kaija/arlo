# Configuration Guide

arlo uses `.arlo/settings.json` for configuration. This file supports two independent top-level sections: **permissions** (tool access control) and **profiles** (LLM provider configuration).

## File Locations

| Level | Path | Purpose |
|-------|------|---------|
| User (global) | `~/.arlo/settings.json` | Default settings for all projects |
| Project | `.arlo/settings.json` (relative to working dir) | Project-specific overrides |

Project-level settings take precedence over user-level settings.

---

## Permissions

The `"permissions"` key controls which tool invocations are allowed or denied without prompting.

```json
{
  "permissions": {
    "allow": ["<pattern>", ...],
    "deny": ["<pattern>", ...]
  }
}
```

### Pattern Format

Two pattern forms are supported:

#### Bare Patterns

Match against the **tool name** only. Supports glob metacharacters `*` (zero or more characters) and `?` (exactly one character).

| Pattern | Matches |
|---------|---------|
| `file_read` | Exact match: only `file_read` |
| `file_*` | Any tool starting with `file_`: `file_read`, `file_write`, `file_edit` |
| `*` | All tools |
| `?hell` | `shell` (one char + "hell") |

#### Compound Patterns

Match a **specific tool** with a **glob on its primary argument**: `ToolName(arg_glob)`

The primary argument is extracted from the tool's input JSON by checking keys in order: `command`, `path`, `url`.

| Pattern | Matches |
|---------|---------|
| `shell(npm*)` | `shell` tool with commands starting with "npm" |
| `shell(cargo *)` | `shell` tool with commands starting with "cargo " |
| `file_write(/tmp/*)` | `file_write` to paths under `/tmp/` |
| `web_fetch(https://docs.rs/*)` | `web_fetch` to URLs under docs.rs |

### Available Tool Names

| Tool Name | Description | Primary Arg Key |
|-----------|-------------|-----------------|
| `shell` | Execute shell commands | `command` |
| `file_read` | Read file contents | `path` |
| `file_write` | Create or overwrite files | `path` |
| `file_edit` | Apply edits to existing files | `path` |
| `glob` | Find files by glob pattern | `path` |
| `grep` | Search file contents with regex | `path` |
| `web_fetch` | Fetch a URL and convert to text | `url` |
| `web_search` | Web search via Brave API | — |

### Priority Order (Permissions)

When the same pattern appears at multiple levels:

1. **Runtime grants** (session-level, highest priority)
2. **Project-level** `.arlo/settings.json`
3. **User-level** `~/.arlo/settings.json`

Within the same level, if a pattern appears in both `allow` and `deny`, **deny wins**.

### Examples

```json
{
  "permissions": {
    "allow": [
      "file_*",
      "glob",
      "grep",
      "shell(cargo *)",
      "shell(npm *)",
      "shell(git *)"
    ],
    "deny": [
      "shell(rm -rf *)",
      "shell(sudo *)",
      "file_write(/etc/*)"
    ]
  }
}
```

---

## Profiles

The `"profiles"` key defines named LLM provider configurations. Use profiles to switch between providers, models, or API endpoints without changing environment variables.

```json
{
  "profiles": {
    "default": "<profile_name>",
    "<profile_name>": {
      "provider": "openai" | "anthropic" | "ollama",
      "api_key": "<string>",
      "base_url": "<string>",
      "model": "<string>",
      "context_window": <number>,
      "max_output_tokens": <number>,
      "extra": { ... }
    }
  }
}
```

### Profile Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `provider` | string | Recommended | Provider backend: `"openai"`, `"anthropic"`, or `"ollama"` |
| `api_key` | string | For openai/anthropic | API key or token |
| `base_url` | string | Optional | Custom API endpoint (e.g., proxy, local server) |
| `model` | string | Recommended | Default model name for this profile |
| `context_window` | number | Optional | Override model's default context window (tokens) |
| `max_output_tokens` | number | Optional | Override model's max output token limit |
| `extra` | object | Optional | Provider-specific key-value pairs (e.g., `{"organization": "org-xxx"}`) |

All fields are optional. Unrecognized fields are silently ignored (forward-compatible).

### The `"default"` Key

A special `"default"` key (string value, not an object) within `"profiles"` specifies which profile to use when no `--profile` flag is given.

- If `"default"` is set and references a valid profile → that profile is used
- If `"default"` references a nonexistent profile → warning logged, falls back to env vars
- If `"default"` is absent and no `--profile` flag → falls back to env vars

### Profile Selection Priority

1. `--profile <name>` CLI flag (highest)
2. `"default"` key in project-level settings
3. `"default"` key in user-level settings
4. Environment variable detection (existing behavior)

### Profile Merging (User vs. Project)

- Same profile name in both → **project fully replaces user** (no field-level blending)
- Different profile names → unioned (both available)
- `"default"` key → project takes priority, user as fallback

### Environment Variable Overrides

Environment variables always take **highest priority**, overriding profile values:

| Provider | Env Variable | Overrides |
|----------|-------------|-----------|
| `openai` | `OPENAI_API_KEY` | `api_key` |
| `openai` | `OPENAI_BASE_URL` | `base_url` |
| `anthropic` | `ANTHROPIC_API_KEY` | `api_key` |
| `ollama` | `OLLAMA_HOST` | `base_url` |

### CLI Flags

| Flag | Description |
|------|-------------|
| `--profile <name>` | Select a named profile for this invocation |
| `--model <name>` | Override the profile's model (works with or without `--profile`) |

### Examples

```json
{
  "profiles": {
    "default": "work",
    "work": {
      "provider": "anthropic",
      "api_key": "sk-ant-api03-xxxxxxxxxxxx",
      "model": "claude-sonnet-4-20250514",
      "context_window": 200000,
      "max_output_tokens": 16384
    },
    "local": {
      "provider": "ollama",
      "base_url": "http://localhost:11434",
      "model": "llama3",
      "context_window": 8192
    },
    "openai-proxy": {
      "provider": "openai",
      "base_url": "https://my-proxy.example.com/v1",
      "model": "gpt-4o",
      "extra": {
        "organization": "org-abc123"
      }
    }
  }
}
```

Usage:

```bash
# Uses "work" profile (default)
arlo "explain this code"

# Explicitly select a profile
arlo --profile local "summarize this file"

# Override model within a profile
arlo --profile work --model claude-sonnet-4-20250514 "refactor this"

# Env var overrides profile api_key
ANTHROPIC_API_KEY=sk-override arlo "hello"
```

---

## Complete Example

A full `settings.json` combining both sections:

```json
{
  "permissions": {
    "allow": [
      "file_*",
      "glob",
      "grep",
      "shell(cargo *)",
      "shell(npm *)",
      "shell(git *)",
      "web_search",
      "web_fetch(https://docs.rs/*)"
    ],
    "deny": [
      "shell(rm -rf *)",
      "shell(sudo *)",
      "shell(chmod *)",
      "file_write(/etc/*)",
      "file_write(~/.ssh/*)"
    ]
  },
  "profiles": {
    "default": "work",
    "work": {
      "provider": "anthropic",
      "api_key": "sk-ant-api03-xxxxxxxxxxxx",
      "model": "claude-sonnet-4-20250514",
      "context_window": 200000,
      "max_output_tokens": 16384
    },
    "local": {
      "provider": "ollama",
      "base_url": "http://localhost:11434",
      "model": "llama3",
      "context_window": 8192
    },
    "openai": {
      "provider": "openai",
      "api_key": "sk-proj-xxxxxxxxxxxx",
      "model": "gpt-4o",
      "context_window": 128000,
      "max_output_tokens": 4096,
      "extra": {
        "organization": "org-abc123"
      }
    }
  }
}
```

---

## Backward Compatibility

- If `"profiles"` key is absent → existing env-var-based behavior unchanged
- If `"permissions"` key is absent → no permission rules (all tools prompt for approval)
- Both keys are parsed independently; the presence of one does not affect the other
- Unknown top-level keys are ignored
