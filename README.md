# sessidx

sessidx indexes local Claude Code, Codex, and Pi session logs (JSONL) into SQLite for two uses: finding the past session where something happened, by a person or by an agent with a shell, and counting behavior by harness, model, role, and week with stated denominators. Raw logs remain read-only and authoritative; the index is derived and can always be rebuilt.

The database, `~/.cache/sessidx/index.db`, holds redacted copies of messages, tool inputs, shell argv, and the first 2 KiB of each tool output, plus exact source paths and session IDs. Redaction is heuristic ([accepted gaps](docs/design.md#privacy-and-limits)), so keep the database as private as the logs themselves. sessidx creates it readable only by you and makes no network connections; deleting `~/.cache/sessidx` removes everything it stored.

## Install

sessidx is developed and tested only on Apple Silicon macOS.

```sh
brew install entwining/tap/sessidx
```

To build from source instead, you need Rust 1.98 or later and a C compiler for the bundled SQLite (the Xcode Command Line Tools); the code uses Unix file APIs:

```sh
cargo install --locked --git https://github.com/Entwining/sessidx --root ~/.local
```

This installs `~/.local/bin/sessidx`; put that directory on `PATH`. From a checkout, use `--path .` instead of `--git`.

## Use

Build the index once before querying:

```sh
sessidx index
```

It reads `~/.claude/projects`, `~/.codex/sessions`, `~/.codex/archived_sessions`, and `~/.pi/agent/sessions`. A long history takes several minutes; an interrupted build resumes from the files it finished. After that, each query refreshes the index incrementally within a two-second budget, so nothing runs in the background. Queries also index, but only for those two seconds each, so on a long history they report incomplete results until the first build is done.

Every command writes JSON Lines for `jq`. The examples below run on a small synthetic history, [`tests/fixtures/readme`](tests/fixtures/readme), and a test checks that the output shown here is what sessidx prints for it. To find the discussion behind a commit:

```console
$ sessidx search 'Redis SQLite' | jq -c 'select(.type == "session") | .hits[] | {role, snippet}'
{"role":"tool","snippet":"[main 9f3e2a1] Replace Redis session cache with SQLite"}
{"role":"assistant","snippet":"{\"command\":\"git commit -m 'Replace Redis session cache with SQLite'\"}"}
{"role":"assistant","snippet":"Decision: switch the session cache from Redis to SQLite with WAL, because the deployment is single-host and Redis adds an extra failure point."}
```

`search` returns one record per session with its best hits. Each hit also carries a `ref`, the `path:line` of its record in the session log: `sessidx show REF` prints the records around it, and `sessidx show SESSION_ID` pages through the whole session.

Counts state their unit and denominator, and how many units they could not classify:

```console
$ sessidx count failures --by harness | jq -c 'select(.type == "count") | {harness, numerator, denominator, unclassified, unit}'
{"harness":"claude","numerator":0,"denominator":2,"unclassified":0,"unit":"tool_call_attempts"}
{"harness":"codex","numerator":1,"denominator":3,"unclassified":0,"unit":"tool_call_attempts"}
{"harness":"pi","numerator":0,"denominator":1,"unclassified":0,"unit":"tool_call_attempts"}
```

The last line of every command is an `end` record. Check its `complete` field before reading an empty result as absence: a result limit, the two-second deadline, a source range that changed since indexing, or an index still catching up makes it `false`.

| Command | Use it to |
| --- | --- |
| `search 'TERMS'` | Rank sessions by relevance, with up to three best hits each |
| `grep 'REGEX' --harness codex` | Return every match within a scope narrowed by session, directory, time, or harness |
| `show REF` | Open a hit's `path:line`, a session ID, or a `codex://threads/ID` link |
| `count commands --program rg` | Count shell commands, tool failures, or denials by harness, model, role, week, or session kind |
| `sql 'SELECT ...'` | Run one read-only query over the [schema](src/schema.sql), without refreshing |
| `doctor` | Report parser and log-shape coverage |
| `index --full` | Rebuild the index, as a schema change requires |

[docs/cli.md](docs/cli.md) is the full contract, and `sessidx COMMAND --help` lists each command's options.

### Keep history searchable

When a harness deletes an old session file, sessidx drops its rows too, so the session is no longer searchable. Claude Code deletes transcripts older than [`cleanupPeriodDays`](https://code.claude.com/docs/en/data-usage#data-retention) (30 days by default; sessions started or last continued in Claude Desktop or Cowork are exempt by default), so raise that setting to keep a longer searchable history.

## Use it from an agent

The [`sessidx` skill](skills/sessidx/SKILL.md) teaches an agent with a shell when to reach for sessidx and how to read its results. With `sessidx` on `PATH`, install it for your user with the GitHub CLI:

```sh
gh skill install Entwining/sessidx sessidx --scope user
```

## Documentation

- [docs/cli.md](docs/cli.md): commands, output records, exit codes, and refresh behavior.
- [docs/design.md](docs/design.md): storage, refresh, privacy, counting units, and known limits.
- [docs/formats.md](docs/formats.md): each normalization rule and observed log variant with its synthetic fixture and regression test.
- [skills/sessidx/SKILL.md](skills/sessidx/SKILL.md): how an agent uses sessidx and reads its results.
- [AGENTS.md](AGENTS.md): decisions to keep and how to change and validate the code.

## License

[MIT](LICENSE)
