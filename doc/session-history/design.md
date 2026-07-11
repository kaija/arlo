# Session History Store — Design

## Overview

Two modules in `agent-core`, mirroring the existing `TaskStore` /
`InMemoryTaskStore` pattern:

```
crates/agent-core/src/
├── session_store.rs      # SessionStore trait, SessionMeta, SessionStoreError
└── fs_session_store.rs   # FsSessionStore (default, ~/.arlo/sessions/)
```

```
┌──────────────┐      ┌───────────────────┐      ┌──────────────────────────┐
│ caller (CLI, │─────▶│ dyn SessionStore  │─────▶│ FsSessionStore           │
│ run loop)    │      │ append/save/load/ │      │ ~/.arlo/sessions/*.jsonl │
└──────────────┘      │ list/delete       │      └──────────────────────────┘
                      └───────────────────┘
```

## Trait

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn append(&self, session_id: &str, messages: &[Message]) -> Result<(), SessionStoreError>;
    async fn save(&self, session_id: &str, messages: &[Message]) -> Result<(), SessionStoreError>;
    async fn load(&self, session_id: &str) -> Result<Vec<Message>, SessionStoreError>;
    async fn list(&self) -> Result<Vec<SessionMeta>, SessionStoreError>;
    async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError>;
}
```

Both `append` and `save` exist because the two write patterns differ:
the run loop appends new turns cheaply, while compaction rewrites history
wholesale. Collapsing them into `save`-only would make every turn an O(n)
rewrite.

## On-disk format

- **Path**: `~/.arlo/sessions/<session_id>.jsonl`
- **Format**: JSON Lines — one `Message` per line, using the existing serde
  representation of `Message` (tagged by `role`). Blank lines are skipped on
  read.

Why JSONL over a single JSON array:

| Concern            | JSONL                          | JSON array            |
|--------------------|--------------------------------|-----------------------|
| Append a turn      | O(1) file append               | O(n) rewrite          |
| Crash mid-append   | Trailing partial line only     | Whole file corrupt    |
| Inspect / debug    | `tail`, `grep`, `jq` friendly  | needs full parse      |

Why not SQLite: adds a dependency and a schema for what is one ordered list
per session. Revisit only if cross-session queries are needed.

## Key decisions

- **`~/.arlo` root**: resolved via `dirs::home_dir()` (already a workspace
  dependency). Falls back to `./.arlo/sessions` if no home dir — better than
  panicking in odd environments (containers with no HOME).
- **Atomic `save`**: write to `<id>.jsonl.tmp`, then `rename` over the
  target. Rename is atomic on POSIX, so `load` never observes a torn file.
  `append` relies on `O_APPEND` semantics — a crash can lose at most the
  trailing partial line, and `load` tolerates that only insofar as complete
  lines parse; a torn final line surfaces as a `Serialization` error rather
  than silent data loss.
- **Id validation in `path_for`**: single choke point rejecting `""`, `"."`,
  `".."`, separators, and NUL before any path is built. This is a trust
  boundary (session ids can come from user flags) and is not simplified away.
- **`SessionMeta` is id + mtime only**: everything derivable from `stat`,
  so `list()` never opens files. Message counts or titles would force a read
  per session; deferred until a UI needs them.
- **No caching, no locking**: the store is stateless; each call hits the
  filesystem. Sessions are single-writer in practice (one CLI process).

## Error handling

`std::io::ErrorKind::NotFound` on `load`/`delete` maps to
`SessionStoreError::NotFound { id }`; all other I/O errors pass through as
`Storage`. `serde_json` failures surface as `Serialization`.

## Testing

Unit tests in `fs_session_store.rs` against `tempfile::TempDir` roots cover:
append→load round-trip, save-overwrites-append, `NotFound` on missing
load/delete, list ordering (newest first) and empty-root behavior, and
rejection of every path-traversal id shape.

## CLI wiring

The store is wired at the CLI layer (`agent-core`'s run loop is untouched):

- **New session id** per invocation: `YYYYMMDD-HHMMSS-<pid>` (sortable,
  collision-safe enough for one machine).
- **TUI REPL**: the event loop already accumulates `state.history`
  (user prompt on `StartRun`, assistant reply on `AgentEnd`, denials on
  `ResumeRun`). At the bottom of each loop iteration, if `history.len()`
  changed, the full history is `save()`d — one choke point covers every
  mutation site. Save failures surface as a warning span, never crash the
  REPL.
- **Single-prompt mode**: prior history (if `--resume`) plus the prompt run
  via `Input::Items`; the final `result.state.messages` is saved after the
  run.
- **Flags**: `--sessions` lists stored sessions (timestamp + id) and exits;
  `--resume <ID>` loads a session's history into either mode and continues
  appending to the same id.

## Future extensions (not built)

- `SqliteSessionStore` / remote store — implement the trait, no caller
  changes.
- Per-turn persistence inside `run_loop` (crash resilience mid-run) —
  today persistence happens at user-turn granularity in the CLI.
- Session titles/summaries in `SessionMeta` for a resume-picker UI, and
  rendering resumed history in the TUI output buffer (currently a one-line
  "Resumed session" note; the model still sees the full history).
