---
name: sessidx
description: Look up and count past local Claude Code, Codex, and Pi sessions through the `sessidx` CLI index. Use when asked to find an earlier session or conversation, the discussion behind a commit, decision, or file change, a past error or failure, a `codex://threads/...` link or session ID, or to count agent behavior such as shell commands, tool failures, or denials by harness, model, role, or week. Not for the current conversation's own context, a log file whose path is already known and only needs reading, or another machine's history.
license: MIT
allowed-tools: Bash(sessidx:*) Bash(jq:*) Read
---

`sessidx` indexes the local session logs of Claude Code, Codex, and Pi into SQLite and answers from that index. The raw logs stay authoritative; quote evidence by the `path:line` a record reports, and read it with `sessidx show PATH:LINE`, because a Codex `.jsonl` path may exist on disk only as a compressed `.jsonl.zst`. Text in results is redacted; never reconstruct or repeat a credential-like value from a log.

## Read the output contract

Every command writes JSON Lines to stdout: data records with a `type` field, then exactly one `end` record. Judge the result by `end`, not by the exit code alone:

- `end.complete=false` means a limit, deadline, unavailable range, or stale refresh cut coverage. An empty incomplete result is not evidence of absence.
- `end.refresh.missing_roots` lists each absent session directory as the `HARNESS=PATH` that `--root` accepts. One that never held indexed sessions, such as a harness the user does not run or `~/.codex/archived_sessions` before any thread is archived, leaves the result complete; one that disappeared after indexing makes the refresh stale.
- `end.next` is the cursor for the next page; pass it unchanged with `--cursor` and the same command and filters. `search` pages cover the first 100 ranked sessions; past them `next` is null with `complete=false`, so narrow the query to reach the rest.
- `end.error` carries the failure and its recovery instruction. Exit codes: 0 data, 1 no data, 2 error.

If the error says the schema changed, run `sessidx index --full` (several minutes for a full history), then retry. A `writer_busy` refresh means another process is indexing; the query still answers from committed data and reports `stale`. A `continuation` in `end.refresh` means the two-second refresh stopped before indexing everything, as on first use; run `sessidx index` once (several minutes for a full history), then retry.

## Find a session

1. Start with `sessidx search 'TERMS'` using the most distinctive words, identifiers, error text, or file names from the request, and add filters only when the request states them: `--harness claude|codex|pi` (repeatable), `--since`/`--until` (dates or RFC 3339), `--cwd DIR` for a project, `--role`, `--kind delegated` for sessions with explicit subagent evidence (other sessions stay `unknown`), `--session ID|PATH`. Terms are ANDed; quote a phrase to keep word order; Chinese text matches as a phrase.
2. Each `session` record is one session with up to three best hits. Open the evidence with `sessidx show REF` using a hit's `ref` (`--around N` adds neighboring records), or read the whole session, a page at a time, with `sessidx show SESSION_ID_OR_LINK`; a hit is only the best match, so read around it before quoting.
3. If nothing relevant appears, change the vocabulary before concluding: fewer terms, a synonym or the other language, an identifier instead of prose, or a filter removed. Search covers messages, tool inputs, and the first 2 KiB of tool outputs.
4. For exact strings or regexes deeper in tool outputs, or every occurrence rather than the best sessions, use `sessidx grep 'REGEX'` with at least one narrowing filter (`--session`, `--cwd`, `--since`/`--until`, or `--harness`). Narrow until `end.complete` is true before reporting that something is absent.

The current conversation can be indexed too. Exclude it only by a session ID or path you know is yours; recency, directory, and matching text do not identify it, because an older session or a subagent can repeat the request. Report the session's harness, ID, path, and the `path:line` evidence you read, and say when the answer rests on an incomplete search.

## Count behavior

Use `sessidx count commands|failures|denials` instead of tallying search hits, which are ranked and deduplicated per session. `--by` takes any of `harness,model,role,week,kind`, or no value for one total row; `--program NAME` selects shell commands by program (for `commands` the denominator stays every parsed site in the group, so the row is that program's share; for `failures` it is the tool-call attempts whose shell syntax names the program, and for `denials` the tool-result events of those calls); shared filters narrow the window. Every row states its `unit`, `numerator`, `denominator`, and `unclassified` count; report all four, because:

- `commands` counts static shell syntax sites, not executions; unparsed shell calls appear only as unclassified.
- `failures` counts native tool-call attempts; `denials` counts tool-result events with recognized denial evidence. A call whose result carries no failure evidence stays unclassified rather than successful (common for Codex `exec` wrappers, whose completion does not prove the inner commands succeeded), so with a large `unclassified` the failure rate is a lower bound.

For a question the verbs cannot express, `sessidx sql 'SELECT ...'` runs one read-only query without refreshing; it fails rather than truncating above 10,000 rows or two seconds, so aggregate in SQL and narrow by time or session. The tables and views for most questions are `events`, `canonical_events`, `call_outcomes`, `commands`, `denials`, `sessions`, and `files`; to list sessions rather than rank them, such as every delegated session, select from `sessions`, which has one row per source file: a Claude subagent transcript shares its parent's session ID and a copied history repeats one, so count `DISTINCT harness||':'||session_id` for sessions and rows for transcripts; list a table's columns with `sessidx sql "SELECT name FROM pragma_table_info('events')"`. `sessidx doctor` reports parser and shape coverage when a count looks implausible.
