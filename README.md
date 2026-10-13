# sessidx

sessidx indexes your local Claude Code, Codex, and Pi session logs into SQLite. You, or your agent, can then find the past session where something happened, such as the discussion behind a commit or an earlier failure, with one command instead of grepping raw JSONL, and count behavior, such as failed tool calls out of all tool calls, by agent, model, role, and week. The raw logs stay read-only and authoritative; the index can always be rebuilt from them.

The database, `~/.cache/sessidx/index.db`, holds redacted copies of messages, tool inputs, shell argv, and the first 2 KiB of each tool output, plus exact source paths and session IDs. Redaction is heuristic ([accepted gaps](docs/design.md#privacy-and-limits)), so keep the database as private as the logs themselves. sessidx creates it readable only by you and makes no network connections; deleting `~/.cache/sessidx` removes everything it stored.

## Install

sessidx is developed and tested only on Apple Silicon macOS.

```sh
brew install entwining/tap/sessidx
```

To build from source instead, you need Rust at or above the `rust-version` in `Cargo.toml` and a C compiler for the bundled SQLite (the Xcode Command Line Tools); the code uses Unix file APIs:

```sh
cargo install --locked --git https://github.com/Entwining/sessidx --root ~/.local
```

This installs `~/.local/bin/sessidx`; put that directory on `PATH`. From a checkout, use `--path .` instead of `--git`.

Then build the index once:

```sh
sessidx index
```

It reads `~/.claude/projects`, `~/.codex/sessions`, `~/.codex/archived_sessions`, and `~/.pi/agent/sessions`. A long history takes several minutes, and an interrupted build resumes from the files it finished. From then on, each query first indexes new and changed logs within a two-second budget, so nothing runs in the background. Before the first build finishes, two seconds per query cannot cover a long history, so results stay incomplete. The same holds after an upgrade that changes how logs are read or indexed until `sessidx index` runs; an upgrade that changes the index's tables says in its release notes to run `sessidx index --full`.

### Keep history searchable

When an agent deletes an old session file, sessidx drops its rows too, so the session is no longer searchable. Claude Code deletes transcripts older than [`cleanupPeriodDays`](https://code.claude.com/docs/en/claude-directory#cleaned-up-automatically), 30 days by default; raise it to keep a longer searchable history. Transcripts of sessions you started in Claude Desktop or Cowork, or most recently continued there, are kept at any age by default.

## Use it from an agent

The [`sessidx` skill](skills/sessidx/SKILL.md) tells an agent when to reach for sessidx and how to read its results. Each agent installs it as a plugin or package with the commands below; start a new session afterwards to load it. If you installed the skill another way before, remove that copy first, or the agent finds two and may keep the old one. The skill does not include the `sessidx` binary: the agent runs the one you installed above, so it must be on `PATH`.

### Claude Code

```sh
claude plugin install sessidx --marketplace Entwining/sessidx
```

Claude Code does not update plugins from third-party marketplaces by default. To have it update this one, run `/plugin` in a session, select `entwining` on the Marketplaces tab, and choose Enable auto-update; otherwise run `claude plugin update sessidx@entwining`.

### Codex

```sh
codex plugin marketplace add Entwining/sessidx
codex plugin add sessidx@entwining
```

Codex updates the plugin when it starts.

### Pi

```sh
pi install git:github.com/Entwining/sessidx
```

Update it with `pi update git:github.com/Entwining/sessidx`.

## Use it directly

Every command writes JSON Lines for `jq`. The examples below run on a small synthetic history, [`tests/fixtures/readme`](tests/fixtures/readme), and a test checks that the output shown here is what sessidx prints for it. To find the discussion behind a commit:

```console
$ sessidx search 'Redis SQLite' | jq -c 'select(.type == "session") | .hits[] | {role, text}'
{"role":"tool","text":"[main 9f3e2a1] Replace Redis session cache with SQLite"}
{"role":"assistant","text":"{\"command\":\"git commit -m 'Replace Redis session cache with SQLite'\"}"}
{"role":"assistant","text":"Decision: switch the session cache from Redis to SQLite with WAL, because the deployment is single-host and Redis adds an extra failure point."}
```

`search` returns one record per session with its best hits. Each hit also carries a `ref`, the `path:line` of its record in the session log: `sessidx show REF` prints the records around it, and `sessidx show SESSION_ID` pages through the whole session.

Counts are named for what they count, and they add up: every tool call `failed`, `succeeded`, or has an unknown outcome, which is never taken for success.

```console
$ sessidx count failures --by agent | jq -c 'select(.type == "count") | {agent, calls, failed, succeeded, outcome_unknown}'
{"agent":"claude","calls":2,"failed":0,"succeeded":2,"outcome_unknown":0}
{"agent":"codex","calls":3,"failed":1,"succeeded":2,"outcome_unknown":0}
{"agent":"pi","calls":1,"failed":0,"succeeded":1,"outcome_unknown":0}
```

The last line of every command is an `end` record. Check its `complete` field before reading an empty result as absence: a result limit, the 24 KiB page size, the two-second deadline, a source range that changed since indexing, or an index still catching up makes it `false`.

| Command | Use it to |
| --- | --- |
| `search 'TERMS'` | Rank sessions by relevance, with up to three best hits each |
| `grep 'REGEX' --agent codex` | Return every match within a scope narrowed by session, directory, time, or agent |
| `show REF` | Open a hit's `path:line`, a session ID, or a `codex://threads/ID` link |
| `count commands --program rg` | Count shell commands, tool failures, or denials by agent, model, role, week, or session kind |
| `sql 'SELECT ...'` | Run one read-only query over the [schema](src/schema.sql), without refreshing |
| `doctor` | Report parser and log-shape coverage |
| `index --full` | Rebuild the index, as a schema change requires |

[docs/cli.md](docs/cli.md) is the full contract, and `sessidx COMMAND --help` lists each command's options.

## Documentation

- [docs/cli.md](docs/cli.md): commands, output records, exit codes, and refresh behavior.
- [docs/design.md](docs/design.md): storage, refresh, privacy, counting units, and known limits.
- [docs/formats.md](docs/formats.md): each normalization rule and observed log variant with its synthetic fixture and regression test.
- [skills/sessidx/SKILL.md](skills/sessidx/SKILL.md): how an agent uses sessidx and reads its results.
- [AGENTS.md](AGENTS.md): goals, decisions to keep, the roadmap, and how to change and validate the code.

## License

[MIT](LICENSE)
