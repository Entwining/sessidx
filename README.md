# sessidx

A local Rust index for Claude Code, Codex, and Pi JSONL sessions. Raw logs remain read-only and authoritative.

```sh
cargo test
cargo install --locked --path . --root ~/.local
~/.local/bin/sessidx index
sessidx search 太长 --harness codex --since 2026-10-01 --json
```

The default database is `~/.cache/sessidx/index.db`. Discovery reads only `~/.claude/projects`, `~/.codex/sessions`, and `~/.pi/agent/sessions`, without following symlinks. `index --full` replaces the derived index. An older schema fails with `run sessidx index --full`; rebuilding requires the writer lock. An unterminated final record is deferred until its newline arrives. `--db PATH --root codex=PATH` selects an isolated database and explicit input root; repeat `--root` for multiple harnesses.

```sh
sessidx search 'commit "instruction hash"' --cwd /path/to/project --json
sessidx search --scan 'Script error:' --session SESSION_ID --json
sessidx show codex://threads/SESSION_ID --json
sessidx show /path/to/session.jsonl:42 --around 3
sessidx count --metric commands --program rg --by harness,model,role,week
sessidx count --metric failures --by harness,model
sessidx count --metric denials --by harness,kind
sessidx doctor
sessidx sql 'SELECT source,reason_id,count(*) FROM denials GROUP BY source,reason_id'
```

FTS joins whitespace-separated terms with AND; quoted phrases preserve word order. Chinese character sequences use phrase matching, and Latin terms match whole words. FTS searches redacted message and tool-input text. Tool outputs are pointer-only and require `--scan` or `show`. Pure hex, wallet addresses and UUIDs stay searchable unless credential context or a known secret prefix qualifies them for redaction. Narrowed raw scanning matches original bytes before redacting its display.

Search filters are `--harness`, speaker `--role`, inclusive `--since`, exclusive `--until`, `--cwd` (directory or descendants), `--session`, and repeatable `--file`. Dates accept RFC 3339 or UTC `YYYY-MM-DD`. FTS defaults to 20 results, with `--limit` and `--offset`. Raw scanning requires at least one filter other than role, reads only indexed byte ranges, and reports incomplete coverage and an event `--cursor` after its two-second deadline. `show` defaults to 100 records and supports `--cursor`.

Each lookup, count, or diagnostic refreshes within a two-second budget. Staleness, writer contention, missing roots, parse errors, and unknown shapes appear on stderr. A normal deferred tail is separate from staleness; `index` reports `deferred_tails`. JSON Lines lookup output contains `harness`, `session_id`, `path`, `line_no`, `ts`, `role`, `model`, `snippet`, and `ref`. Exit codes are 0 for hits, 1 for no hits, and 2 for errors. Incomplete no-hit scans always include a coverage notice. Changed source ranges are unavailable until reindexed.

`count` always emits JSON Lines with an explicit unit, numerator, denominator, and unclassified unit/count. Commands are static shell syntax sites, failures are native tool-call attempts, and denials are native tool-result events. Site counts do not prove execution; multi-command results remain call-level. `--by` accepts harness, model, role, UTC ISO week, and session kind. Native IDs deduplicate history; identical text and retries are preserved. Unknown models and session kinds remain explicit.

`doctor` reports shape coverage, parse errors, deferred/budget-limited files, unparsed shell calls, unknown models/kinds, and unknown denial reasons. `sql` opens the database read-only without refresh, accepts one SELECT/WITH, and fails rather than returning a partial result above 10,000 rows or two seconds. Narrow expensive SQL and counts to files or time windows. Empty/invalid files can have session metadata rows, so a session-row count is not a human-conversation denominator.

`docs/design.md` defines privacy, counting, and parser limits. `docs/formats.md` maps all 80 inventory variants to synthetic fixtures and rules. `scripts/ablate.py REPORT_JSON [RULE ...]` temporarily removes rules, checks regression failures and unrelated controls, and restores exact source bytes. Do not run it concurrently with source edits or builds.
