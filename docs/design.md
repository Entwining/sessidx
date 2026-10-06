# Design

## Ownership and storage

One Rust package discovers three read-only session roots and streams complete records through harness adapters. SQLite uses bundled FTS5, WAL, foreign keys, and a serialized writer lock. Every event retains a file, line, byte offset, and byte length; multiple content blocks share the original range and have distinct ordinals. Tool results have pointers and outcome metadata only. Unknown records contribute structural signatures and coverage counts, never raw values.

FTS stores a copy of redacted message and tool-input text with spaces around CJK characters. A phrase query preserves contiguous Chinese character order; Latin words retain `unicode61` word semantics. The first executable test proves both behaviors with bundled SQLite.

## Incremental indexing

A file resumes when its device/inode and stored prefix hash agree and it grows; otherwise its owned rows are replaced transactionally. Prefix length is stored, so appending to a file shorter than 4 KiB does not incorrectly invalidate its old prefix. Model and session context survive in file state. Only complete newline-terminated records advance the cursor. Records over 16 MiB are streamed past and reported as parse errors, keeping memory bounded.

Explicit indexing is unbounded. Query refresh receives a two-second deadline, commits only complete records, and reports staleness and a continuation file when it cannot finish. A writer conflict returns immediately with incomplete coverage. A missing root also prevents a complete-coverage claim.

## Privacy

Message and tool-input text is redacted before storage. Redaction recognizes PEM private keys, Bearer authorization, common token prefixes, credential assignments/headers, and long runs with Shannon entropy at least 4 bits per character. Tool outputs are never copied into SQLite. Display applies redaction again. Heuristics cannot identify every secret; ordinary prose, short low-entropy secrets, and credentials split across separate fields remain potential limitations. Raw logs are the user's existing source, not copied fixtures.

## Dependencies

`brush-parser =0.4.0`, bundled `rusqlite`, and `serde_json` are specified by the decision. All direct versions and the lockfile are pinned. `serde` derives typed persisted state and JSON output instead of hand-maintained serializers; `anyhow` removes error conversion boilerplate; `regex` supplies scan and redaction matching; `chrono` supplies RFC 3339 and week handling; `sha2` provides fingerprints and instruction hashes; `walkdir` replaces recursive directory traversal; `fs2` provides OS-released advisory writer locks; `clap` replaces argument parsing and usage validation. Test-only `tempfile` provides isolated fixture/database lifecycle cleanup.

## Evidence boundaries

Tool-call attempts and static shell sites are different counting units. A shell AST does not prove execution. Call results can be attributed to multiple static sites only with an explicit call-level label. Flags, text-derived outcomes, and missing evidence remain distinguishable. Native IDs deduplicate inherited events; equal text never establishes identity. Model metadata is event-local. Codex instruction hashes identify recorded text, not proof that the model followed it.
