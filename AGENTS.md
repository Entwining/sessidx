# sessidx

sessidx indexes the local Claude Code, Codex, and Pi session logs of one user on one Apple Silicon Mac into SQLite, for two uses: lookup (find the session where something happened, the context behind a commit, a past failure) by the user and by agents, and retrospective counting by harness, model, role, and week with honest denominators. It replaces grepping about 16 GB per question and rewriting throwaway parsers per retrospective, which produced contradictory counts (one sweep found 0 Codex guard rejections where a verifier found 102 in 50 sessions). Agents reach it through the `sessidx` skill in the user's dotfiles (`dot_agents/skills/sessidx/SKILL.md` in the chezmoi source), which requires `sessidx` on `PATH`.

## Where things are owned

| File | Owns |
| --- | --- |
| `README.md` | Commands, filters, output records, exit codes: the contract callers rely on |
| `docs/design.md` | Storage, refresh, privacy, counting units, and known limits as they currently work |
| `docs/formats.md` | One row per observed log variant with its fixture; the only place a normalization rule's reason lives |
| This file | Purpose, decisions that are easy to undo by accident, workflow, and environment traps |

Change the owner, not a copy. A rule without a `docs/formats.md` row and a fixture that fails when the rule is removed is not finished. A change to the README contract also changes the dotfiles skill and its `CLI_SMOKE_COMMANDS` entries in `dot_agents/skills/scripts/validate-skills.rb`; agents learn the CLI from that skill, not from this repository.

## Decisions to keep

Each of these was argued and measured; reopen one only with new evidence, not on general reputation.

- **Rust.** Go was proposed for its shell parser's maturity. On this user's real commands `brush-parser` matched it, and the user's tooling is moving to Rust. Brush stays pinned exactly.
- **Raw logs are the source of truth and are never written.** The index is derived and can always be rebuilt with `index --full`.
- **Redacted text is stored.** The alternative, storing no text, was rejected by the user. Redaction leans toward recall, but identifiers are lookup keys and must stay searchable: paths and `path:line` locators, session and thread IDs, hexadecimal runs, UUIDs, and code identifiers such as `tools.exec_command`. A secret in one of those shapes is caught only by credential context. A change to the run pattern or its exemptions can silently redact hundreds of thousands of identifiers, so count redactions on a real-text sample before and after, by shape, without printing values.
- **Tool outputs are pointers plus their first 2 KiB in full-text search.** On held-out "find the session where this error happened" cases, `search` found 40 of 42 with the prefix and none without it; the only fallback, `grep` narrowed to a harness and month, found 14 and completed 3 within its deadline. The prefix costs about a quarter of the database and some ranking noise when output text outranks a message. Full outputs would multiply size and secret exposure; the rest is reached by `grep` or `show`.
- **Chinese text is matched as phrases over a character-spaced copy** in FTS5 `unicode61`. Trigram tokenization was rejected because queries under three characters fall back to a full scan.
- **Codex code-mode commands are taken only from `cmd` string literals** in the wrapper source; nothing evaluates JavaScript (user decision). Anything else in a wrapper that references a shell tool stays one unparsed site, so it remains in the denominator.
- **Refresh happens on query within a two-second budget.** There is no daemon or LaunchAgent. Never raise the budget to hide a slow path, and never let a query wait without bound on the writer lock: a wedged refresh that holds the lock blocks every later query.
- **Counts state their unit, numerator, denominator, and unclassified rows.** Shell command sites are static syntax and never prove execution. Inherited history is deduplicated only by native IDs, never by identical text.
- **No production switches.** No config file, environment variables, or feature toggles; rule ablation edits source temporarily through `scripts/ablate.py`.
- **No database size target.** Remove waste that does not reduce function and report the measured size.

Not in v1, with the reason: semantic search (one of thirteen lookup misses was a wording mismatch; the other misses came from unindexed data, redaction, ranking, or evaluation-setup errors), an MCP server, a TUI, token and cost accounting, Claude and Pi loaded-instruction attribution, Pi branch reconstruction, and any publishing.

## Changing the CLI

The consumer is usually an agent with a shell, so the ordinary path must be one short command and the output must compose with `jq`.

- Ranked lookup (`search`) and exhaustive matching (`grep`) are different contracts and stay different verbs; ranked results are grouped by session.
- Nothing an agent needs exists only on stderr or only in the exit code: the final `end` record carries completeness, staleness, the cursor, and any error with its recovery instruction.
- Enumerated values are validated and have no aliases: an evaluator once passed `claude_code`, got an empty result, and lost four tasks.
- Add a flag only for a lookup that failed without it; questions the verbs do not answer go through read-only `sql`.

## Develop and validate

- Logs contain credentials. Never copy a real log line into the repository, fixtures, docs, commit messages, or reports. Fixtures are synthetic records shaped like an observed variant; cite real evidence by `path:line` and its kind only.
- Never point a development build at the default database `~/.cache/sessidx/index.db`. Use `--db` with a scratch path and `--root HARNESS=PATH`.
- Run `cargo test`, and `scripts/ablate.py` after rule changes, never concurrently with edits or builds.
- Real-corpus acceptance references private sessions and lives outside the repository in `~/.local/share/sessidx-eval/` (its `README.md` lists the scripts and their inputs): frozen lookup replay, full-build measurement, independent gold counts, the output-prefix check, the code-mode comparison, and the leak scan. The release targets are warm lookup p95 within 2 s and indexing RSS within 512 MiB, measured on a full build.
- Corpus scans read only the three session roots, with `rg -j 2` or at most two concurrent readers.
- Git is local only. Pushing, adding a remote, a Homebrew tap, or a release is the user's decision.

## Environment traps

- macOS has no `timeout`; use `perl -e 'alarm shift; exec @ARGV' SECONDS COMMAND`.
- Inside the Codex sandbox, nested `sandbox-exec` exits 71 and `/usr/bin/time -l` fails; run memory and timing measurements from an ordinary shell.
- A read-only sandbox cannot open the live WAL database normally; `sqlite3 "file:$HOME/.cache/sessidx/index.db?immutable=1"` reads it, possibly as a stale snapshot.
