# sessidx

A local Rust index for Claude Code, Codex, and Pi JSONL sessions. Raw logs remain read-only and authoritative.

```sh
cargo test
cargo run -- index
cargo install --path . --root ~/.local
```

The default database is `~/.cache/sessidx/index.db`. Discovery reads only `~/.claude/projects`, `~/.codex/sessions`, and `~/.pi/agent/sessions`, without following symlinks. Use `index --full` to rebuild. An incomplete trailing record is deferred until its newline arrives. `--db PATH --root codex=PATH` selects a separate database and explicit synthetic or frozen input roots.

Search, display, counting, and diagnostics are delivered in the next two implementation slices.
