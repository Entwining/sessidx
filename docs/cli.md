# Command-line contract

This is the contract that callers, including the bundled agent skill, rely on: what each command reads and returns, its output records, and its exit codes. Each option's meaning is in `sessidx COMMAND --help`.

## Output and exit codes

Every command emits JSON Lines on stdout, except help/version text. Data records carry `type`: `session` for search (with `hits`), `hit` for grep, `record` for show, `count`, `row` for SQL (columns inside `data`, preserving a column named `type`), `index`, or `doctor`. Select data before using `jq`, for example `jq -c 'select(.type == "record") | .record.message'`.

One final `end` record contains `complete`, `stale`, `next`, `searched`, `unavailable_ranges`, and refresh coverage in `refresh`. A limit, deadline, unavailable range, or stale refresh makes `complete=false`; parse errors, unknown records, and deferred tails are reported in `refresh` without changing it.

`searched.returned` is the number of data records written (for `index`, the source records it indexed). `searched.records` counts the unit that `searched.unit` names, as observed, not as an estimate of SQLite pages or of all underlying events examined:

- `session_hits` (`search`): hits inside the returned sessions.
- `source_ranges` (`grep`, `show`): attempted ranges; `searched.bytes` is the total read from those that could be read.
- `aggregate_rows` (`count`) and `sql_rows` (`sql`).
- `source_records` (`index`).
- `diagnostic` (`doctor`), which counts none.

`refresh` reports:

- `records` and `files_changed`: what this run indexed.
- `deferred_tails`: files whose final record still awaits its newline.
- `parse_errors` and `unknown_records`: totals over the whole index.
- `missing_roots` (see [Sources and database](#sources-and-database)), `writer_busy`, and `stale`.
- `continuation`: the file where a budget-limited refresh stopped.

`sql` does not refresh, so its `refresh` holds only `stale` and `writer_busy`.

Exit codes are 0 for data, 1 for no data, and 2 for errors. A complete empty result and an incomplete one are distinguished by `end.complete`. Errors produce one `end` record with `error`, `complete=false`, `stale=null` (unknown), `next=null`, and zero `searched` counts, without refresh coverage. Any recovery instruction is in `error`, never only on stderr. A reader that closes stdout early, such as `head`, ends the command quietly with exit 0. Successful maintenance exits 0; an `index` run that leaves the index stale (writer busy, or a previously indexed root missing) exits 2 with its normal records and no `error`.

## Sources and database

The default database is `~/.cache/sessidx/index.db`. Discovery reads `~/.claude/projects`, `~/.codex/sessions`, `~/.codex/archived_sessions` (where Codex moves an archived thread's file), and `~/.pi/agent/sessions`; a root that is itself a symbolic link to a directory is followed, but links inside a root are not. Session files are `*.jsonl`, and Codex's compressed `*.jsonl.zst` rollouts are reported under their `.jsonl` path, which may not exist on disk; read such a record with `show`. A root you do not have, such as an agent you do not use or the Codex archive before the first archived thread, is skipped; `end.refresh.missing_roots` lists each absent root as the `AGENT=PATH` that `--root` accepts. A root that disappears after its sessions were indexed (moved or unmounted) keeps its rows and makes refreshes stale until it returns, and `index --full` refuses to run until it is restored or left out of `--root`. `--db PATH` selects another database and repeatable `--root AGENT=PATH` replaces all default roots, so pass both Codex directories to keep archived threads; later commands, including the recovery commands below, need the same flags. An older schema fails with one recovery instruction: `run sessidx index --full`.

## Sessions

A session is one transcript file, identified by the agent's native ID from that transcript's own header or name, never from history copied into it: the Codex thread ID in its first `session_meta`, the Pi ID in its session header, the Claude session ID that names a main transcript, or the `agentId` of a Claude subagent transcript, whose `parent_id` is the session that spawned it. A copied event belongs to the transcript it appears in; counts still use only its first copy. `session_kind` is `main` for a session started on its own and `subagent` for one spawned by another session of the same agent, from native evidence only: Claude `agentId` or `isSidechain`, Codex `session_meta.source`, and Pi, which has no subagents; otherwise it is `unknown`. `cwd` is the directory the session started in, and `started_at` its first timestamp. One ID can name more than one transcript: Claude agent IDs are unique only within their parent session, and Codex can continue a thread in a second rollout file. A session selector (`--session`, or a `show` target) that names more than one transcript fails and lists their paths; pass one of them instead.

## Commands

### search

`search` uses ranked full-text search (SQLite FTS5) and returns one record per session. Sessions are ordered by the best hit's BM25 relevance, then the latest matching hit timestamp (newest first); index order breaks remaining ties. Each record carries the session's `agent`, `session_id`, `session_kind`, `parent_id`, `path`, `cwd`, and `started_at`, up to three best hits in relevance/recency order, and `matched_hits`, the number of matching indexed events. Each hit carries its `ref`, `ts`, `role`, `model`, and matched `text`. The session limit is applied before selecting its hits. Whitespace-separated terms are joined with AND; quoted phrases preserve word order. Chinese character sequences use phrase matching; Latin terms match whole words. Full-text search covers redacted messages, tool inputs, and the first 2 KiB of tool result or diagnostic attachment text. Prefixes stop at a UTF-8 boundary and report `truncated` when text was omitted. Lookup identifiers (absolute, relative and dotfile paths and `path:line` locators, versioned names such as model IDs, hex, UUIDs, code identifiers and command-line flags) stay searchable.

### grep and show

`grep` matches regexes over original bytes before redacting its display. It requires a narrowing filter other than role or session kind, reads only indexed byte ranges, and stops after two seconds or the hit limit. `show` expands a hit's `ref` (`path:line`), a session ID, a transcript path, or a `codex://threads/ID` address. Both return the whole source line as a redacted JSON object in `record`, once per role, however many events the index derived from it. A range whose source changed since indexing is unavailable until reindexed, and `end.unavailable_ranges` counts it: `grep` cannot match it, and `show` returns its record with `unavailable`, the reason with its recovery instruction, instead of `record`.

### Filters and pagination

Search, grep, and count share the filters listed by `--help`. Filters select results; they do not narrow discovery. Option values are checked before any refresh: an unknown enum value, a `--limit` outside 1–1000, a repeated `--by` grouping, or a malformed `--since`, `--until`, or `--root` exits 2 with an error that names the flag and, for a closed set, lists its valid values; there are no aliases.

`search` and `grep` default to 20 results; `show` defaults to 100. Pagination uses only `--cursor`: pass the opaque string from `end.next` to the same command and filters. Later pages include only events indexed before the first page. Search pagination covers the first 100 ranked sessions, whose order the cursor freezes; once a page reaches that bound while more sessions match, the end record has `complete=false` and no `next`, so narrow the query to reach the rest. Appends are allowed; a changed query, a replacement/deletion affecting those events, or a rebuild requires restarting without the cursor. Limits bound returned rows, not SQLite's work.

### count

`count METRIC` accepts `commands`, `failures`, or `denials`. Each row names its `metric`, its groups, the selected `program` if any, and counts named for what they count; the counts of a metric partition its population, and anything outside the population has a name of its own:

- `commands` counts static shell syntax sites in tool calls: `sites` that parsed, of which `sites_without_program` have no literal program word (such as `$CMD args`). `unparsed_calls` counts the shell calls whose syntax did not parse, which are outside `sites` because their sites are unknown. With `--program`, `matching_sites` is the part of `sites` that runs the program. Sites do not prove execution.
- `failures` counts native tool-call attempts: `calls` = `failed` + `succeeded` + `outcome_unknown`, where a call whose result carries no failure or success evidence has an unknown outcome rather than a successful one. With `--program`, `calls` holds the calls whose shell syntax names the program, and `calls_with_unresolved_program` counts the other calls with a site whose program is unknown, which might run it.
- `denials` counts native tool-result events: `results`, of which `denied` carry denial evidence, of which `denied_reason_unknown` have a denial whose reason is unknown. With `--program`, `results` holds the results of calls that name the program, and `results_with_unresolved_program` the other results whose call has a site whose program is unknown.

`failures` counts tool calls and `denials` counts tool-result events; a call that runs several shell commands contributes once to each, not once per command. `--by` takes a comma-separated subset of `agent`, `model`, `role`, `week` (UTC ISO week), and `session_kind`; the default is `agent,model,role,week`, `--by` with no value returns one total row, and an empty value, an empty segment, or a repeated name is an error. Native IDs deduplicate history; identical text and retries remain separate. Unknown models and session kinds stay explicit. `count` uses canonical events; `doctor` counts stored rows, including inherited copies and context.

### sql

`sql` opens the database read-only without refresh, accepts one SELECT/WITH, and fails instead of returning partial success above 10,000 rows or two seconds. Query `sessions`, `events`, `canonical_events`, `call_outcomes`, `commands`, and `denials`, defined in [`src/schema.sql`](../src/schema.sql); the other tables are index storage. `call_outcomes` has one row per canonical tool call with its outcome: `ok` is 0, 1, or NULL when unknown, as `count failures` classifies it. `events` carries each event's `agent`, `session_id`, `file_id`, and `path`, and reads `text` only when a query names it. For per-transcript correlation with `sessions`, use `e.file_id = s.file_id` or the indexed `e.path = s.path`; `(agent, session_id)` can name several transcripts and combines their events. `denials.reason_id` names the guard rule by its identifier in the agent-guard source (or the earlier dotfiles guards' name for a rule only they had, or a shared refusal class), with `unknown` for unmapped text; historical wordings of the same rule share an ID. `commands.argv_json` retains static shell word spellings, including quoting and assignment-shaped arguments after the command name (`-f title=hello`) but not prefix assignments (`FOO=1 env`), for flag questions such as `rg -rn`. Duplicate column names, including collisions after redaction, fail with an instruction to alias them uniquely. `sql` holds a nonblocking shared cache lock while reading; writer contention produces `stale=true` and `complete=false`. Narrow expensive queries to session paths or time windows.

### doctor

`doctor` names each count after the rows it counts, next to the population it belongs to: `files` and `incomplete_files`, `parse_errors`, `records` and `records_with_unknown_shape` with the most frequent `unknown_shape_signatures`, `sessions` and `sessions_with_unknown_kind`, `events` and `canonical_events`, `tool_events` and `tool_events_without_model` (stored tool calls and results, inherited copies included; `count --by model` reports the canonical ones without a model as `unknown`), `shell_calls` and `shell_calls_unparsed`, and `denials` and `denials_with_unknown_reason`. A session count is not a human-conversation denominator.

## Refresh

`search`, `grep`, `show`, `count`, and `doctor` refresh with a two-second budget; `index` refreshes without one, and `sql` does not refresh. Refresh coverage reports writer contention, missing roots, parse errors and unknown shapes. Writer-lock acquisition does not wait; SQLite busy waits are limited to 50 ms. Filesystem operations and individual SQLite operations can exceed a deadline, so the budget is not a hard end-to-end guarantee. A normal unterminated final record is deferred until its newline arrives, separately from staleness. `index --full` rebuilds derived data and its schema under the writer lock.
