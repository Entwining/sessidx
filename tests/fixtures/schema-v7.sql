-- Source files are the authority; byte ranges include the complete JSONL newline.
CREATE TABLE IF NOT EXISTS index_identity (
 id INTEGER PRIMARY KEY CHECK(id=1), instance TEXT NOT NULL
);
INSERT INTO index_identity SELECT 1,lower(hex(randomblob(16))) WHERE NOT EXISTS(SELECT 1 FROM index_identity);
-- Replacement/deletion records invalidate only snapshots containing the changed events.
CREATE TABLE IF NOT EXISTS index_changes (
 id INTEGER PRIMARY KEY AUTOINCREMENT, first_event_id INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS files (
 id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, agent TEXT NOT NULL,
 dev INTEGER NOT NULL, inode INTEGER NOT NULL, size INTEGER NOT NULL, mtime TEXT NOT NULL,
 prefix_hash TEXT NOT NULL, prefix_len INTEGER NOT NULL,
 bytes_indexed INTEGER NOT NULL DEFAULT 0, lines_indexed INTEGER NOT NULL DEFAULT 0,
 state_json TEXT NOT NULL, parse_errors INTEGER NOT NULL DEFAULT 0,
 index_status TEXT NOT NULL DEFAULT 'ready'
);
-- ready, deferred_tail, and budget_exhausted distinguish expected live tails from staleness.
-- Each transcript file is one session, identified by its own header or name rather than
-- by history copied into it. Rule: docs/formats.md `transcript_identity`.
CREATE TABLE IF NOT EXISTS sessions (
 file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
 agent TEXT NOT NULL, session_id TEXT NOT NULL, parent_id TEXT,
 session_kind TEXT NOT NULL, session_kind_source TEXT NOT NULL,
 path TEXT NOT NULL, cwd TEXT NOT NULL, started_at TEXT, instruction_hash TEXT
);
CREATE INDEX IF NOT EXISTS sessions_id ON sessions(session_id);
-- Every indexed event needs a range for show/grep and identity for dedup. Bodies live in
-- event_texts so that scans over this metadata stay narrow.
CREATE TABLE IF NOT EXISTS event_rows (
 id INTEGER PRIMARY KEY AUTOINCREMENT, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 line_no INTEGER NOT NULL, byte_off INTEGER NOT NULL, byte_len INTEGER NOT NULL, ordinal INTEGER NOT NULL,
 raw_hash BLOB NOT NULL CHECK(length(raw_hash)=32), native_id BLOB CHECK(native_id IS NULL OR length(native_id)=32),
 ts TEXT, role TEXT NOT NULL, role_source TEXT NOT NULL, kind TEXT NOT NULL, kind_source TEXT NOT NULL,
 model TEXT, model_source TEXT NOT NULL, tool TEXT, call_id BLOB CHECK(call_id IS NULL OR length(call_id)=32),
 ok INTEGER, ok_source TEXT NOT NULL, exit_code INTEGER,
 UNIQUE(file_id,line_no,ordinal)
);
CREATE INDEX IF NOT EXISTS events_time ON event_rows(ts);
CREATE INDEX IF NOT EXISTS events_native ON event_rows(native_id,kind) WHERE native_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS events_call ON event_rows(file_id,call_id) WHERE call_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS event_texts (
 event_id INTEGER PRIMARY KEY REFERENCES event_rows(id) ON DELETE CASCADE,
 text TEXT NOT NULL, truncated INTEGER NOT NULL
);
-- The body columns are subqueries so that a query that does not name them never reads them.
CREATE VIEW IF NOT EXISTS events AS
 SELECT e.id,s.agent,s.session_id,s.path,e.line_no,e.ts,e.role,e.kind,e.model,e.tool,
 CASE WHEN e.call_id IS NULL THEN NULL ELSE lower(hex(e.call_id)) END AS call_id,e.ok,e.exit_code,
 (SELECT text FROM event_texts WHERE event_id=e.id) AS text,
 coalesce((SELECT truncated FROM event_texts WHERE event_id=e.id),0) AS text_truncated,
 e.role_source,e.kind_source,e.model_source,e.ok_source,
 CASE WHEN e.native_id IS NULL THEN NULL ELSE lower(hex(e.native_id)) END AS native_id,
 e.file_id,e.byte_off,e.byte_len,e.ordinal,lower(hex(e.raw_hash)) AS raw_hash
 FROM event_rows e JOIN sessions s ON s.file_id=e.file_id;
-- Static sites are not execution counts. All results are attributed at call level.
CREATE TABLE IF NOT EXISTS commands (
 id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES event_rows(id) ON DELETE CASCADE,
 site INTEGER NOT NULL, program TEXT, argv_json TEXT, parsed INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS commands_event ON commands(event_id);
CREATE TABLE IF NOT EXISTS denials (
 id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES event_rows(id) ON DELETE CASCADE,
 source TEXT NOT NULL, reason_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS denials_event ON denials(event_id);
-- Child FK indexes avoid scanning all commands/denials when a file is replaced.
-- Only structural signatures are retained; raw unknown values are never stored.
CREATE TABLE IF NOT EXISTS shapes (
 file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 signature TEXT NOT NULL, known INTEGER NOT NULL, n INTEGER NOT NULL,
 PRIMARY KEY(file_id,signature,known)
);
-- Message/input text already lives in event_texts; FTS retains tokens and positions only.
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(text, content='', contentless_delete=1, tokenize='unicode61');
CREATE TRIGGER IF NOT EXISTS events_delete AFTER DELETE ON event_rows BEGIN DELETE FROM fts WHERE rowid=old.id; END;
-- Native identity, never matching text, deduplicates inherited events for counts.
-- Rule: docs/formats.md `canonical_first_copy`.
CREATE VIEW IF NOT EXISTS canonical_events AS
 SELECT e.* FROM events e JOIN event_rows r ON r.id=e.id JOIN sessions s ON s.file_id=r.file_id
 WHERE r.native_id IS NULL OR NOT EXISTS (
  SELECT 1 FROM event_rows r2 JOIN sessions s2 ON s2.file_id=r2.file_id
  WHERE r2.native_id=r.native_id AND r2.kind=r.kind AND s2.agent=s.agent
  AND (coalesce(s2.started_at,'9999'),s2.path,r2.line_no,r2.ordinal) < (coalesce(s.started_at,'9999'),s.path,r.line_no,r.ordinal));
-- Calls pair with results inside one session, which can span transcripts.
CREATE VIEW IF NOT EXISTS call_outcomes AS
 SELECT e.agent,e.session_id,e.call_id,
 CASE WHEN sum(ok=0)>0 THEN 0 WHEN sum(ok=1)>0 THEN 1 END AS ok,
 CASE WHEN sum(ok=0 AND ok_source='flag')>0 THEN 'flag'
      WHEN sum(ok=0 AND ok_source='text')>0 THEN 'text'
      WHEN sum(ok_source='flag')>0 THEN 'flag'
      WHEN sum(ok_source='text')>0 THEN 'text' ELSE 'none' END AS ok_source
 FROM canonical_events e WHERE e.kind='tool_result' AND e.call_id IS NOT NULL GROUP BY e.agent,e.session_id,e.call_id;
PRAGMA user_version=7;
