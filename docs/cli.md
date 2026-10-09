# Command-line contract

This is the contract that callers, including the bundled agent skill, rely on: what each command reads and returns, its output records, and its exit codes. Each option's meaning is in `sessidx COMMAND --help`.

## Output and exit codes

Every command emits JSON Lines on stdout, except help/version text. Data records carry `type`: `session` for search (with `hits`), `hit` for grep, `record` for show (raw redacted JSON in `text`), `count`, `row` for SQL (columns inside `data`, preserving a column named `type`), `index`, or `doctor`. Select data before using `jq`, for example `jq -r 'select(.type == "record") | .text'`.

One final `end` record contains `complete`, `stale`, `next`, `searched`, `unavailable_ranges`, and refresh coverage. A limit, deadline, unavailable range or stale refresh makes `complete=false`; parse errors, unknown records and deferred tails are reported in refresh coverage without changing it. `searched.unit` identifies the records counted: selected session hits, attempted source ranges and successfully read bytes, aggregate rows, SQL rows, or index source records. These are observable counts, not estimates of SQLite pages or all underlying events examined.

Exit codes are 0 for data, 1 for no data, and 2 for errors. A complete empty result and an incomplete one are distinguished by `end.complete`. Errors produce one `end` record with `error`, `complete=false`, `stale=null` (unknown), `next=null`, and zero `searched` counts, without refresh coverage; no recovery instruction exists only on stderr. A reader that closes stdout early, such as `head`, ends the command quietly with exit 0. Successful maintenance exits 0; an `index` run that leaves the index stale (writer busy, or a previously indexed root missing) exits 2 with its normal records and no `error`.

## Sources and database

The default database is `~/.cache/sessidx/index.db`. Discovery reads `~/.claude/projects`, `~/.codex/sessions`, `~/.codex/archived_sessions` (where Codex moves an archived thread's file), and `~/.pi/agent/sessions`; a root that is itself a symbolic link to a directory is followed, but links inside a root are not. A root you do not have, such as a harness you do not use or the Codex archive before the first archived thread, is skipped; `end.refresh.missing_roots` lists each absent root as the `HARNESS=PATH` that `--root` accepts. A root that disappears after its sessions were indexed (moved or unmounted) keeps its rows and makes refreshes stale until it returns, and `index --full` refuses to run until it is restored or left out of `--root`. `--db PATH` selects another database and repeatable `--root HARNESS=PATH` replaces all default roots, so pass both Codex directories to keep archived threads; later commands, including the recovery commands below, need the same flags. An older schema fails with one recovery instruction: `run sessidx index --full`.

## Commands

### search

`search` uses ranked full-text search (SQLite FTS5) and returns one record per harness/session ID. Sessions are ordered by the best hit's BM25 relevance, then the latest matching hit timestamp (newest first); harness and ID break remaining ties. Each record contains up to three best hits in relevance/recency order and `matched_hits`, the number of matching indexed events. The top-level `path` is the best hit's source; each hit retains its own path and ref. The session limit is applied before selecting its hits. Whitespace-separated terms are joined with AND; quoted phrases preserve word order. Chinese character sequences use phrase matching; Latin terms match whole words. Full-text search covers redacted messages, tool inputs, and the first 2 KiB of tool result or diagnostic attachment text. Prefixes stop at a UTF-8 boundary and report `truncated` when text was omitted. Lookup identifiers (paths and `path:line` locators, hex, UUIDs, code identifiers and command-line flags) stay searchable.

### grep and show

`grep` matches regexes over original bytes before redacting its display. It requires a narrowing filter other than role or kind, reads only indexed byte ranges, and stops after two seconds or the hit limit. `show` expands a hit's `ref`, a native session ID, a `codex://threads/ID` address, or `path:line`. Each hit's `ref` is a precise `path:line` address. A range whose source changed since indexing is unavailable until reindexed: its record's `text` is the placeholder `[source range unavailable; run sessidx index]` instead of JSON, and `end.unavailable_ranges` counts it.

### Filters and pagination

Search, grep, and count share the filters listed by `--help`. Filters select results; they do not narrow discovery. Option values are checked before any refresh: an unknown enum value, a `--limit` outside 1–1000, a repeated `--by` grouping, or a malformed `--since`, `--until`, or `--root` exits 2 with an error that names the flag and, for a closed set, lists its valid values; there are no aliases.

`search` and `grep` default to 20 results; `show` defaults to 100. `--limit` accepts 1–1000. Pagination uses only `--cursor`: pass the opaque string from `end.next` to the same command and filters. Later pages stay below the first page event high-water mark. Search pagination covers the first 100 ranked sessions, whose order the cursor freezes; once a page reaches that bound while more sessions match, the end record has `complete=false` and no `next`, so narrow the query to reach the rest. Appends are allowed; a changed query, a replacement/deletion affecting those events, or a rebuild requires restarting without the cursor. Limits bound returned rows, not SQLite's work.

### count

`count METRIC` accepts `commands`, `failures`, or `denials`. Every row states its unit, numerator, denominator, and unclassified unit/count. Commands are static shell syntax sites, failures are native tool-call attempts, and denials are native tool-result events. Site counts do not prove execution; multi-command results remain call-level. Shell calls that could not be parsed are outside the `commands` denominator and are counted as unclassified. `--by` takes a comma-separated subset of `harness`, `model`, `role`, `week` (UTC ISO week), and `kind` (session kind); the default is `harness,model,role,week`, `--by` with no value returns one total row, and an empty value, an empty segment, or a repeated name is an error. Native IDs deduplicate history; identical text and retries remain separate. Unknown models and session kinds stay explicit. With `--program`, unclassified also includes calls whose program is unresolved and which fall outside the selected denominator. `count` uses canonical events; `doctor` coverage counts stored rows, including inherited copies and context.

### sql

`sql` opens the database read-only without refresh, accepts one SELECT/WITH, and fails instead of returning partial success above 10,000 rows or two seconds. Tables and views are defined in [`src/schema.sql`](../src/schema.sql). `commands.argv_json` retains static shell word spellings, including quoting, for flag questions such as `rg -rn`. Duplicate column names, including collisions after redaction, fail with an instruction to alias them uniquely. `sql` holds a nonblocking shared cache lock while reading; writer contention produces `stale=true` and `complete=false`. Narrow expensive queries to session paths or time windows.

### doctor

`doctor` reports shape coverage, parse errors, incomplete files, unparsed shell calls, unknown models/kinds, and unknown denial reasons. A session-row count is not a human-conversation denominator.

## Refresh

Each lookup, count, or diagnostic refreshes with a two-second budget. Refresh coverage reports writer contention, missing roots, parse errors and unknown shapes. Writer-lock acquisition does not wait; SQLite busy waits are limited to 50 ms. Filesystem operations and individual SQLite operations can exceed a deadline, so the budget is not a hard end-to-end guarantee. A normal unterminated final record is deferred until its newline arrives, separately from staleness. `index --full` rebuilds derived data and its schema under the writer lock.
