-- Source files are the authority; byte ranges include the complete JSONL newline.
CREATE TABLE IF NOT EXISTS files (
 id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, harness TEXT NOT NULL,
 dev INTEGER NOT NULL, inode INTEGER NOT NULL, size INTEGER NOT NULL, mtime TEXT NOT NULL,
 prefix_hash TEXT NOT NULL, prefix_len INTEGER NOT NULL,
 bytes_indexed INTEGER NOT NULL DEFAULT 0, lines_indexed INTEGER NOT NULL DEFAULT 0,
 state_json TEXT NOT NULL, parse_errors INTEGER NOT NULL DEFAULT 0,
 complete INTEGER NOT NULL DEFAULT 0
);
-- Metadata is per source file, so replacements and model history cannot leave orphan sessions.
CREATE TABLE IF NOT EXISTS sessions (
 file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
 harness TEXT NOT NULL, session_id TEXT NOT NULL, cwd TEXT NOT NULL,
 parent_id TEXT, kind TEXT NOT NULL, kind_source TEXT NOT NULL, instruction_hash TEXT
);
CREATE INDEX IF NOT EXISTS sessions_id ON sessions(session_id);
-- Output events contain no output text; outcome evidence stays separate from absence.
CREATE TABLE IF NOT EXISTS events (
 id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 session_id TEXT NOT NULL, native_id TEXT, line_no INTEGER NOT NULL, byte_off INTEGER NOT NULL,
 byte_len INTEGER NOT NULL, ordinal INTEGER NOT NULL, ts TEXT, role TEXT NOT NULL,
 role_source TEXT NOT NULL, kind TEXT NOT NULL, kind_source TEXT NOT NULL,
 model TEXT, model_source TEXT NOT NULL, text TEXT, tool TEXT, call_id TEXT,
 ok INTEGER, ok_source TEXT NOT NULL, exit_code INTEGER,
 UNIQUE(file_id,line_no,ordinal)
);
CREATE INDEX IF NOT EXISTS events_time ON events(ts);
CREATE INDEX IF NOT EXISTS events_call ON events(session_id,call_id,kind);
CREATE INDEX IF NOT EXISTS events_native ON events(native_id);
-- Static sites are not execution counts. All results are attributed at call level.
CREATE TABLE IF NOT EXISTS commands (
 id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
 site INTEGER NOT NULL, program TEXT, argv_json TEXT, parsed INTEGER NOT NULL,
 outcome_scope TEXT NOT NULL DEFAULT 'call'
);
CREATE TABLE IF NOT EXISTS denials (
 id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
 source TEXT NOT NULL, reason_id TEXT NOT NULL, UNIQUE(event_id,source,reason_id)
);
-- Only structural signatures are retained; raw unknown values are never stored.
CREATE TABLE IF NOT EXISTS shapes (
 file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 signature TEXT NOT NULL, known INTEGER NOT NULL, n INTEGER NOT NULL,
 PRIMARY KEY(file_id,signature,known)
);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(text, tokenize='unicode61');
CREATE TRIGGER IF NOT EXISTS events_delete AFTER DELETE ON events BEGIN DELETE FROM fts WHERE rowid=old.id; END;
-- Native identity, never matching text, deduplicates inherited events for counts.
CREATE VIEW IF NOT EXISTS canonical_events AS
 SELECT * FROM (SELECT e.*, row_number() OVER (
 PARTITION BY f.harness, CASE WHEN e.native_id IS NULL THEN 'row:'||e.id ELSE 'native:'||e.native_id END, e.kind
 ORDER BY f.path,e.line_no,e.ordinal) AS copy_rank FROM events e JOIN files f ON f.id=e.file_id)
 WHERE copy_rank=1;
PRAGMA user_version=1;
