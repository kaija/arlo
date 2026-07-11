# Session History Store — Specification

## Purpose

Persist conversation history (`Vec<Message>`) across process restarts so a
session can be resumed, listed, or deleted. The store is an abstraction
(`SessionStore` trait) with a default filesystem implementation
(`FsSessionStore`) rooted at `~/.arlo/sessions/`.

## Scope

- **In scope**: message history per session id — append, full rewrite
  (post-compaction), load, list, delete — plus CLI integration:
  auto-persistence in both CLI modes, `--sessions`, and `--resume <ID>`.
- **Out of scope**: full `RunState` snapshots (already covered by
  `RunState::serialize`), task/todo state (`TaskStore`), permission grants,
  cross-session search, encryption.

## Requirements

### R1 — Abstraction

1.1 A `SessionStore` trait in `agent-core` defines the contract; callers
    depend only on the trait (`Arc<dyn SessionStore>`).
1.2 The trait is `async` and `Send + Sync` so implementations can be
    filesystem, database, or remote-backed.

### R2 — Operations

2.1 `append(session_id, messages)` — appends messages to a session; creates
    the session if absent.
2.2 `save(session_id, messages)` — replaces the session's entire history
    (used after compaction rewrites history); creates the session if absent.
2.3 `load(session_id)` — returns the full ordered history; `NotFound` for
    unknown ids.
2.4 `list()` — returns `SessionMeta { id, updated_at }` for every stored
    session, most recently updated first.
2.5 `delete(session_id)` — removes the session; `NotFound` for unknown ids.

### R3 — Data integrity

3.1 Messages round-trip losslessly: `load` after `append`/`save` yields
    messages equal to what was written, in order.
3.2 `save` is atomic with respect to readers: a concurrent `load` sees either
    the old or the new history, never a partial file.
3.3 Session ids are validated at the trust boundary: empty ids and ids
    containing path separators, NUL, `.` or `..` are rejected with
    `InvalidId` (prevents path traversal in filesystem stores).

### R4 — Default filesystem implementation

4.1 `FsSessionStore::new()` roots the store at `~/.arlo/sessions/`
    (via `dirs::home_dir()`); `with_root(path)` allows any directory
    (tests, alternate homes).
4.2 One file per session: `<root>/<session_id>.jsonl`, one JSON-serialized
    `Message` per line.
4.3 The root directory is created on first write; `list()` on a missing root
    returns an empty list, not an error.
4.4 Files not ending in `.jsonl` under the root are ignored by `list()`.

### R5 — Errors

`SessionStoreError` variants: `NotFound`, `InvalidId`, `Serialization`
(wraps `serde_json::Error`), `Storage` (wraps `std::io::Error`). Error
displays include the offending session id where applicable.

### R6 — CLI integration

6.1 Every CLI invocation gets a session id (`YYYYMMDD-HHMMSS-<pid>`) unless
    `--resume <ID>` names an existing one.
6.2 TUI REPL history is persisted after every history change (user prompt,
    assistant reply, permission denial); single-prompt mode persists the
    final run messages.
6.3 `arlo --sessions` lists stored sessions (last-updated timestamp + id),
    newest first, and exits.
6.4 `arlo --resume <ID>` loads the stored history as conversation context in
    either mode and continues appending to the same session.
6.5 Persistence failures warn but never abort the conversation.

## Non-requirements (deliberately deferred)

- **Cross-process locking** — single-process CLI usage assumed; add advisory
  file locks if concurrent writers appear.
- **Message-count / title in `SessionMeta`** — requires reading each file;
  add when a session-picker UI needs it.
- **Retention / GC** — files are small text; add an eviction policy when
  `~/.arlo` size becomes a real complaint.
