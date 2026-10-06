# sessidx

A local Rust index for Claude Code, Codex, and Pi JSONL sessions. Raw logs remain read-only and authoritative.

```sh
cargo test
cargo run -- index
cargo install --path . --root ~/.local
```

The default database is `~/.cache/sessidx/index.db`. Discovery reads only `~/.claude/projects`, `~/.codex/sessions`, and `~/.pi/agent/sessions`, without following symlinks. Use `index --full` to rebuild. An incomplete trailing record is deferred until its newline arrives. `--db PATH --root codex=PATH` selects a separate database and explicit synthetic or frozen input roots.

```sh
sessidx search 太长 --harness codex --since 2026-10-01 --json
sessidx search 'commit "instruction hash"' --cwd /path/to/project
sessidx search --scan 'Script error:' --session SESSION_ID --json
sessidx show codex://threads/SESSION_ID --json
sessidx show /path/to/session.jsonl:42 --around 3
```

Search joins whitespace-separated terms with AND; quoted phrases preserve word order. Results default to 20; `--limit` and `--offset` page FTS hits. Filters include `--harness`, speaker `--role`, `--since` (inclusive), `--until` (exclusive), `--cwd` (exact directory or descendants), `--session`, and repeatable `--file`. Dates accept RFC 3339 or UTC `YYYY-MM-DD`.

Raw `--scan` requires a narrowing filter and reads only matching event ranges. It matches redacted JSON representations, including pointer-only tool results. A two-second scan deadline reports incomplete coverage and an event `--cursor` for continuation. `show` accepts session IDs, Codex links, and source locations; large sessions page with `--cursor`. Changed source ranges are reported unavailable instead of displayed as the old event.

Each lookup refreshes the index within a two-second budget. Staleness, writer contention, missing roots, and incomplete display/scan coverage are JSON notices on stderr. JSON Lines on stdout contain `harness`, `session_id`, `path`, `line_no`, `ts`, `role`, `model`, `snippet`, and `ref`. Exit codes are 0 for hits, 1 for no hits, and 2 for errors; no-hit results with incomplete coverage always include the notice.

Counting and diagnostics are delivered in the final implementation slice.
