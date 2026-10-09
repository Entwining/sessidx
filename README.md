# sessidx

sessidx indexes local Claude Code, Codex, and Pi session logs (JSONL) into SQLite for two uses: finding the past session where something happened, by a person or by an agent with a shell, and counting behavior by harness, model, role, and week with stated denominators. Raw logs remain read-only and authoritative; the index is derived and can always be rebuilt.

The database holds redacted copies of messages, tool inputs, shell argv, and the first 2 KiB of each tool output, plus exact source paths and session IDs. Redaction is heuristic ([accepted gaps](docs/design.md#privacy-and-limits)), so keep the database as private as the logs themselves.

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

`sessidx index` builds the index from `~/.claude/projects`, `~/.codex/sessions`, `~/.codex/archived_sessions`, and `~/.pi/agent/sessions`; a long history takes several minutes the first time. After that, each query refreshes the index incrementally within a two-second budget. The index follows the logs: when a harness deletes an old session file, its rows go too. Claude Code deletes transcripts older than [`cleanupPeriodDays`](https://code.claude.com/docs/en/data-usage#data-retention) (30 days by default; sessions started or last continued in Claude Desktop or Cowork are exempt by default), so raise that setting to keep a longer searchable history.

```sh
sessidx index
sessidx search 'commit "instruction hash"' --cwd /path/to/project
sessidx grep 'Script error:' --session SESSION_ID
sessidx show /path/to/session.jsonl:42 --around 3
sessidx count commands --program rg --by harness,model,role,week
sessidx sql 'SELECT source,reason_id,count(*) FROM denials GROUP BY source,reason_id'
sessidx index --full
sessidx doctor
```

`search` ranks sessions by relevance; `grep` returns every regex match within a narrowed scope; `show` opens a hit's `path:line` or a whole session; `count` aggregates shell commands, tool failures, and denials with their denominators; `sql` runs one read-only query. Every command writes JSON Lines and ends with one `end` record that says whether the result is complete and how to continue, so its output composes with `jq`. [docs/cli.md](docs/cli.md) is the full contract, and `sessidx COMMAND --help` lists each command's options.

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
