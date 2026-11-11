# Sessidx

Search and count your Claude Code, Codex, and Pi history in one local SQLite index. Use it from your shell or let your agent look up earlier work.

## Find the discussion behind a commit

`search` ranks sessions and returns their best hits. Each hit points back to its source record. A test reproduces both examples from a [synthetic history](tests/fixtures/readme).

```console
$ sessidx search 'Redis SQLite' | jq -c 'select(.type == "session") | .hits[] | {role, text}'
{"role":"tool","text":"[main 9f3e2a1] Replace Redis session cache with SQLite"}
{"role":"assistant","text":"{\"command\":\"git commit -m 'Replace Redis session cache with SQLite'\"}"}
{"role":"assistant","text":"Decision: switch the session cache from Redis to SQLite with WAL, because the deployment is single-host and Redis adds an extra failure point."}
```

`sessidx show REF` opens the records around a hit's `path:line`. For exhaustive matching, [`grep` scans original logs within a scope you choose](docs/cli.md#grep-and-show).

## Count what your agents did

Count shell command sites, tool failures, denials, token usage, and compactions. Group results by agent, model, role, week, or session kind.

```console
$ sessidx count failures --by agent | jq -c 'select(.type == "count") | {agent, calls, failed, succeeded, outcome_unknown}'
{"agent":"claude","calls":2,"failed":0,"succeeded":2,"outcome_unknown":0}
{"agent":"codex","calls":3,"failed":1,"succeeded":2,"outcome_unknown":0}
{"agent":"pi","calls":1,"failed":0,"succeeded":1,"outcome_unknown":0}
```

Unknown outcomes stay separate from successes. The [count definitions](docs/cli.md#count) name each population and what lies outside it.

## Let your agent search its history

The [`sessidx` skill](skills/sessidx/SKILL.md) teaches agents when to look up past work and how to read the results. Homebrew ships it with the binary; [plugin and package installs](docs/install.md#agent-skills) are also available.

## Get started

Developed and tested only on Apple Silicon macOS.

```sh
brew install entwining/tap/sessidx
sessidx index
```

If you have an older skill copy, [remove it first](docs/install.md#agent-skills). Install the bundled skill for Claude Code, Codex, and Pi:

```sh
sessidx skill install
```

Start a new agent session to load the skill. The [installation guide](docs/install.md) covers source builds, upgrades, and keeping old sessions searchable.

Queries refresh the index within a two-second budget. Nothing runs in the background. Results are JSON Lines for `jq`; check the final [`end.complete` field](docs/cli.md#output-and-exit-codes) before treating an empty result as absence.

## Your logs stay local

Sessidx makes no network connections and never writes to the source logs. The index can be rebuilt from them.

Redaction is heuristic. Keep the database as private as the logs themselves; the [privacy limits](docs/design.md#privacy-and-limits) describe what it stores and what redaction can miss.

## Contributing

[Normalization rules](docs/formats.md) pair supported log variants with synthetic fixtures and regression tests. [AGENTS.md](AGENTS.md) defines the goals, roadmap, and required development checks.

Sessidx is [MIT licensed](LICENSE).
