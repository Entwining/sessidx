# Installation

sessidx is developed and tested only on Apple Silicon macOS. Install it with Homebrew:

```sh
brew install entwining/tap/sessidx
```

## Build from source

You need Rust at or above the `rust-version` in [Cargo.toml](../Cargo.toml). Bundled SQLite needs a C compiler, provided by the Xcode Command Line Tools. The code uses Unix file APIs.

```sh
cargo install --locked --git https://github.com/Entwining/sessidx --root ~/.local
```

This installs `~/.local/bin/sessidx`; put that directory on `PATH`. From a checkout, use `--path .` instead of `--git`.

## Build and update the index

Build the index once:

```sh
sessidx index
```

It reads `~/.claude/projects`, `~/.codex/sessions`, `~/.codex/archived_sessions`, and `~/.pi/agent/sessions`. A long history takes several minutes. An interrupted build resumes from the files it finished.

Each query first indexes new and changed logs within a two-second budget. Nothing runs in the background. Before the first build finishes, two seconds per query cannot cover a long history, so results stay incomplete.

An upgrade that changes how logs are read or indexed also leaves results incomplete until `sessidx index` runs. An upgrade that changes the index's tables says in its release notes to run `sessidx index --full`. The [refresh contract](cli.md#refresh) defines coverage and recovery.

## Keep history searchable

When an agent deletes an old session file, sessidx drops its rows too. The session is no longer searchable.

Claude Code deletes transcripts older than [`cleanupPeriodDays`](https://code.claude.com/docs/en/claude-directory#cleaned-up-automatically), 30 days by default. Raise it to keep a longer searchable history. Transcripts of sessions you started in Claude Desktop or Cowork, or most recently continued there, are kept at any age by default.

## Agent skills

The [`sessidx` skill](../skills/sessidx/SKILL.md) tells an agent when to reach for sessidx and how to read its results. Choose one installation method below. Start a new session afterwards to load it.

If you installed the skill another way before, remove that copy first. Otherwise the agent finds two and may keep the old one. The skill does not include the `sessidx` binary. The agent runs the one you installed above, so it must be on `PATH`.

### Homebrew

A Homebrew install includes the skill from the same release. Link it into the skills directories that Claude Code (`~/.claude/skills`), Codex, and Pi (`~/.agents/skills`) read:

```sh
sessidx skill install
```

The links point into Homebrew's package, so the skill changes together with the binary on every upgrade. `sessidx skill show` prints the same skill. After `brew uninstall sessidx`, remove the two links with `rm -f ~/.agents/skills/sessidx ~/.claude/skills/sessidx`.

### Claude Code

```sh
claude plugin install sessidx --marketplace Entwining/sessidx
```

Claude Code does not update plugins from third-party marketplaces by default. To enable updates, run `/plugin` in a session. Select `entwining` on the Marketplaces tab and choose Enable auto-update. Otherwise run `claude plugin update sessidx@entwining`.

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
