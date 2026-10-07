# sessidx

A local Rust index for Claude Code, Codex, and Pi JSONL sessions. Raw logs remain read-only and authoritative.

```sh
cargo test
cargo install --locked --path . --root ~/.local
```

The default database is `~/.cache/sessidx/index.db`. Discovery reads only `~/.claude/projects`, `~/.codex/sessions`, and `~/.pi/agent/sessions`, without following symlinks. `--db PATH --root codex=PATH` selects an isolated database and explicit input root; repeat `--root` for multiple harnesses. An older schema fails with one recovery instruction: `run sessidx index --full`.

```sh
sessidx search 'commit "instruction hash"' --cwd /path/to/project
sessidx grep 'Script error:' --session SESSION_ID
sessidx show /path/to/session.jsonl:42 --around 3
sessidx count commands --program rg --by harness,model,role,week
sessidx sql 'SELECT source,reason_id,count(*) FROM denials GROUP BY source,reason_id'
sessidx index --full
sessidx doctor
```

`search` uses ranked FTS and returns one record per harness/session ID. Sessions are ordered by the best hit's BM25 relevance, then the latest matching hit timestamp (newest first); harness and ID break remaining ties. Each record contains up to three best hits in relevance/recency order and `matched_hits`, the number of matching indexed events. The top-level `path` is the best hit's source; each hit retains its own path and ref. The session limit is applied before selecting its hits. Whitespace-separated terms are joined with AND; quoted phrases preserve word order. Chinese character sequences use phrase matching; Latin terms match whole words. FTS searches redacted messages, tool inputs, and the first 2 KiB of tool result or diagnostic attachment text. Prefixes stop at a UTF-8 boundary and report `truncated` when text was omitted. Lookup identifiers (paths and `path:line` locators, hex, UUIDs, code identifiers and command-line flags) stay searchable; [docs/design.md](docs/design.md#privacy-and-limits) defines the redaction rule and its accepted gaps.

`grep` matches regexes over original bytes before redacting its display. It requires a narrowing filter other than role or kind, reads only indexed byte ranges, and stops after two seconds or the hit limit. `show` expands a hit's `ref`, a native session ID, a `codex://threads/ID` address, or `path:line`; changed ranges are unavailable until reindexed. Each hit's `ref` is a precise `path:line` address.

Shared search/grep/count filters are repeatable `--harness` (`claude`, `codex`, `pi`, union), speaker `--role` (`user`, `assistant`, `tool`, `system`, `developer`, `unknown`), session `--kind` (`unknown`, `delegated`), inclusive `--since`, exclusive `--until`, `--cwd` (directory or descendants), and `--session` (native ID or the session file path printed in a hit). Unknown enum values exit 2 and list valid values; there are no aliases. Dates accept RFC 3339 or UTC `YYYY-MM-DD`.

`search` and `grep` default to 20 results; `show` defaults to 100. `--limit` accepts 1–1000. Pagination uses only `--cursor`: pass the opaque string from `end.next` to the same command and filters. Later pages stay below the first page event high-water mark, and search retains its first session ordering. Appends are allowed; a changed query, a replacement/deletion affecting those events, or a rebuild requires restarting without the cursor. Limits bound returned rows, not SQLite's work.

Every command emits JSON Lines on stdout, except help/version text. Data records carry `type`: `session` for search (with `hits`), `hit` for grep, `record` for show (raw redacted JSON in `text`), `count`, `row` for SQL (columns inside `data`, preserving a column named `type`), `index`, or `doctor`. One final `end` record contains `complete`, `stale`, `next`, `searched`, `unavailable_ranges`, and refresh coverage. A limit, deadline, unavailable range or stale refresh makes `complete=false`. `searched.unit` identifies the records counted: selected session hits (including a pagination lookahead), attempted source ranges and successfully read bytes, aggregate rows, SQL rows, or index source records. These are observable counts, not estimates of SQLite pages or all underlying events examined.

Exit codes are 0 for data, 1 for no data, and 2 for errors; successful maintenance exits 0. A complete empty result and an incomplete one are distinguished by `end.complete`. Errors produce one `end` record with `error`, `complete=false`, `stale=null` (unknown), and `next=null`; no recovery instruction exists only on stderr. Select data before using `jq`, for example `jq -r 'select(.type == "record") | .text'`.

Each lookup, count, or diagnostic refreshes with a two-second budget. Refresh coverage reports writer contention, missing roots, parse errors and unknown shapes. Writer-lock acquisition does not wait; SQLite busy waits are limited to 50 ms. Filesystem operations and individual SQLite operations can exceed a deadline, so the budget is not a hard end-to-end guarantee. A normal unterminated final record is deferred until its newline arrives, separately from staleness. `index --full` rebuilds derived data and its schema under the writer lock.

`count METRIC` accepts `commands`, `failures`, or `denials`. Every row states its unit, numerator, denominator, and unclassified unit/count. Commands are static shell syntax sites, failures are native tool-call attempts, and denials are native tool-result events. Site counts do not prove execution; multi-command results remain call-level. `--by` accepts harness, model, role, UTC ISO week, and session kind. Native IDs deduplicate history; identical text and retries remain separate. Unknown models and session kinds stay explicit. With --program, unclassified also includes calls whose program is unresolved and which fall outside the selected denominator. count uses canonical events; doctor coverage counts stored rows, including inherited copies and context.

`sql` opens the database read-only without refresh, accepts one SELECT/WITH, and fails instead of returning partial success above 10,000 rows or two seconds. commands.argv_json retains static shell word spellings, including quoting, for flag questions such as rg -rn. Duplicate column names, including collisions after redaction, fail with an instruction to alias them uniquely. SQL holds a nonblocking shared cache lock while reading; writer contention produces stale=true and complete=false. Narrow expensive queries to session paths or time windows. `doctor` reports shape coverage, parse errors, incomplete files, unparsed shell calls, unknown models/kinds, and unknown denial reasons. A session-row count is not a human-conversation denominator.

`docs/design.md` defines storage, privacy, counting, and parser limits. `docs/formats.md` maps the 80 inventory variants to synthetic fixtures and rules. `scripts/ablate.py REPORT_JSON [RULE ...]` temporarily removes rules, checks regression failures and unrelated controls, and restores exact source bytes. Do not run it concurrently with source edits or builds.
