# Contributing to arlo

Thanks for your interest in contributing! This document covers the practical
workflow; [AGENTS.md](AGENTS.md) covers the codebase map and the invariants you
must not break — read both before a non-trivial change.

## Getting set up

Stable Rust via [rustup](https://rustup.rs), then:

```bash
git clone <repo-url>
cd arlo-rust
make check   # fast compile check — run this first after edits
make test    # full workspace test suite
```

## Development workflow

1. Fork and create a topic branch off `main`.
2. Make your change, keeping the diff focused — one logical change per PR.
3. Before pushing, all of these must pass:

   ```bash
   make fmt     # rustfmt (CI checks formatting)
   make lint    # clippy with -D warnings — warnings are errors
   make test    # cargo test --workspace
   ```

4. Open a PR with a clear description of *what* changed and *why*.

While iterating, scope tests to the area you're touching:

```bash
cargo test -p agent-core run_loop
```

## Code guidelines

- **Match the surrounding style.** Comment density, naming, and idioms should
  blend in with the file you're editing.
- **Read the invariants** in [AGENTS.md](AGENTS.md#invariants--do-not-break)
  before touching the run loop, permissions, sub-agents, or task delivery.
  PRs that break exactly-once result delivery, terminal-event guarantees, or
  permission-layer semantics will be rejected regardless of test status.
- **Tool errors are non-fatal** — they return to the model as `is_error` tool
  results, never as `RunError`.
- **Don't add dependencies** for what a few lines of code or an existing
  workspace dependency can do. New workspace dependencies need justification
  in the PR description.

## Testing conventions

- Unit tests are colocated in each source file (`#[cfg(test)] mod tests`);
  integration tests live in `crates/agent-core/tests/`.
- Property-based tests (`proptest`) are the norm for state machines, stores,
  and the permission engine — extend the existing strategies rather than
  writing ad-hoc loops.
- Mock `Model`/`ModelProvider`/`Tool` implementations already exist in the
  `run_loop.rs` and `sub_agent.rs` test modules; reuse them.
- New behavior needs a test that fails without your change.

## Common contributions

- **New built-in tool**: implement `Tool` in `agent-tools/src/<name>.rs`
  (declare `parameters_schema`, `concurrency`, `approval_requirement` —
  `Always` for anything destructive), export from `lib.rs`, register in the
  agent builder in `agent-cli/src/main.rs`.
- **New provider**: `agent-llm/src/provider.rs` (`UnifiedProvider::from_profile`)
  plus profile fields in `agent-core/src/profile.rs`. Include model metadata
  (context window, pricing) — cost tracking and compaction thresholds read it.
- **New recovery behavior**: map the `ModelError` variant in
  `recovery.rs::map_error_to_strategy`, handle the strategy in
  `run_loop.rs::apply_recovery_run`.

More recipes in [AGENTS.md](AGENTS.md#recipes).

## Commit messages

Use conventional-commit style, scoped to the crate when it applies:

```
feat(agent-llm): add Anthropic HTTP provider
fix: deliver background sub-agent results to the running agent
```

## Reporting bugs

Open an issue with: what you did, what you expected, what happened, and the
output of `cargo --version` / your OS. For agent-loop bugs, `--dump-prompt`
output and the model/provider used are very helpful.

## Security

Never commit API keys, tokens, or provider URLs pointing at internal
infrastructure. Local run scripts matching `run*` are gitignored for this
reason — keep secrets in environment variables or ignored files only.
