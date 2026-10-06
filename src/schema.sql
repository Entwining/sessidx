-- Source files are the authority; byte ranges include the complete JSONL newline.
CREATE TABLE IF NOT EXISTS files (
 id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, harness TEXT NOT NULL,
 dev INTEGER NOT NULL, inode INTEGER NOT NULL, size INTEGER NOT NULL, mtime TEXT NOT NULL,
 prefix_hash TEXT NOT NULL, prefix_len INTEGER NOT NULL,
 bytes_indexed INTEGER NOT NULL DEFAULT 0, lines_indexed INTEGER NOT NULL DEFAULT 0,
 state_json TEXT NOT NULL, parse_errors INTEGER NOT NULL DEFAULT 0,
 index_status TEXT NOT NULL DEFAULT 'ready', first_ts TEXT
);
-- ready, deferred_tail, and budget_exhausted distinguish expected live tails from staleness.
-- Metadata is per source file, so replacements and model history cannot leave orphan sessions.
CREATE TABLE IF NOT EXISTS sessions (
 file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
 harness TEXT NOT NULL, session_id TEXT NOT NULL, cwd TEXT NOT NULL,
 parent_id TEXT, kind TEXT NOT NULL, kind_source TEXT NOT NULL, instruction_hash TEXT
);
CREATE INDEX IF NOT EXISTS sessions_id ON sessions(session_id);
-- Repeated metadata is interned; the events view keeps the public SQL vocabulary.
CREATE TABLE IF NOT EXISTS strings (
 id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE
);
-- Every indexed event needs a range for show/grep, identity for dedup and filter metadata.
-- Context records have only this row: their constant role/kind/outcome fields are derived.
CREATE TABLE IF NOT EXISTS locations (
 id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 session_ref INTEGER NOT NULL REFERENCES strings(id), native_id BLOB,
 line_no INTEGER NOT NULL, byte_off INTEGER NOT NULL, byte_len INTEGER NOT NULL,
 raw_hash BLOB NOT NULL CHECK(length(raw_hash)=32), ordinal INTEGER NOT NULL, ts TEXT,
 model_ref INTEGER REFERENCES strings(id), model_source_ref INTEGER NOT NULL REFERENCES strings(id),
 CHECK(native_id IS NULL OR length(native_id)=32),
 UNIQUE(file_id,line_no,ordinal)
);
CREATE INDEX IF NOT EXISTS events_time ON locations(ts);
CREATE INDEX IF NOT EXISTS events_session ON locations(session_ref);
CREATE INDEX IF NOT EXISTS events_native ON locations(native_id) WHERE native_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS event_details (
 event_id INTEGER PRIMARY KEY REFERENCES locations(id) ON DELETE CASCADE,
 session_ref INTEGER NOT NULL REFERENCES strings(id),
 role_ref INTEGER NOT NULL REFERENCES strings(id), role_source_ref INTEGER NOT NULL REFERENCES strings(id),
 kind_ref INTEGER NOT NULL REFERENCES strings(id), kind_source_ref INTEGER NOT NULL REFERENCES strings(id),
 text TEXT, tool_ref INTEGER REFERENCES strings(id), call_id BLOB,
 ok INTEGER, ok_source_ref INTEGER NOT NULL REFERENCES strings(id), exit_code INTEGER,
 CHECK(call_id IS NULL OR length(call_id)=32)
);
CREATE INDEX IF NOT EXISTS events_call ON event_details(session_ref,call_id,kind_ref) WHERE call_id IS NOT NULL;
CREATE VIEW IF NOT EXISTS events AS
 SELECT l.id,l.file_id,session.value AS session_id,
 CASE WHEN l.native_id IS NULL THEN NULL ELSE lower(hex(l.native_id)) END AS native_id,
 l.line_no,l.byte_off,l.byte_len,lower(hex(l.raw_hash)) AS raw_hash,l.ordinal,l.ts,
 coalesce(role.value,'unknown') AS role,coalesce(role_source.value,'none') AS role_source,
 coalesce(kind.value,'context') AS kind,coalesce(kind_source.value,'root.type') AS kind_source,
 model.value AS model,model_source.value AS model_source,d.text,tool.value AS tool,
 CASE WHEN d.call_id IS NULL THEN NULL ELSE lower(hex(d.call_id)) END AS call_id,
 d.ok,coalesce(ok_source.value,'none') AS ok_source,d.exit_code
 FROM locations l JOIN strings session ON session.id=l.session_ref
 JOIN strings model_source ON model_source.id=l.model_source_ref
 LEFT JOIN strings model ON model.id=l.model_ref
 LEFT JOIN event_details d ON d.event_id=l.id
 LEFT JOIN strings role ON role.id=d.role_ref LEFT JOIN strings role_source ON role_source.id=d.role_source_ref
 LEFT JOIN strings kind ON kind.id=d.kind_ref LEFT JOIN strings kind_source ON kind_source.id=d.kind_source_ref
 LEFT JOIN strings tool ON tool.id=d.tool_ref LEFT JOIN strings ok_source ON ok_source.id=d.ok_source_ref;
-- Static sites are not execution counts. All results are attributed at call level.
CREATE TABLE IF NOT EXISTS commands (
 id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES locations(id) ON DELETE CASCADE,
 site INTEGER NOT NULL, program TEXT, argv_json TEXT, parsed INTEGER NOT NULL,
 outcome_scope TEXT NOT NULL DEFAULT 'call'
);
CREATE INDEX IF NOT EXISTS commands_event ON commands(event_id);
CREATE TABLE IF NOT EXISTS denials (
 id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES locations(id) ON DELETE CASCADE,
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
-- Message/input text already lives in events; FTS retains tokens and positions only.
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(text, content='', contentless_delete=1, tokenize='unicode61');
CREATE TRIGGER IF NOT EXISTS events_delete AFTER DELETE ON locations BEGIN DELETE FROM fts WHERE rowid=old.id; END;
-- Native identity, never matching text, deduplicates inherited events for counts.
CREATE VIEW IF NOT EXISTS canonical_events AS
 SELECT * FROM (SELECT e.*, row_number() OVER (
 PARTITION BY f.harness, CASE WHEN e.native_id IS NULL THEN 'row:'||e.id ELSE 'native:'||e.native_id END, e.kind
 ORDER BY coalesce(f.first_ts,'9999'),f.path,e.line_no,e.ordinal) AS copy_rank FROM events e JOIN files f ON f.id=e.file_id)
 WHERE copy_rank=1;
CREATE VIEW IF NOT EXISTS call_outcomes AS
 SELECT f.harness,e.session_id,e.call_id,
 CASE WHEN sum(ok=0)>0 THEN 0 WHEN sum(ok=1)>0 THEN 1 END AS ok,
 CASE WHEN sum(ok=0 AND ok_source='flag')>0 THEN 'flag'
      WHEN sum(ok=0 AND ok_source='text')>0 THEN 'text'
      WHEN sum(ok_source='flag')>0 THEN 'flag'
      WHEN sum(ok_source='text')>0 THEN 'text' ELSE 'none' END AS ok_source
 FROM canonical_events e JOIN files f ON f.id=e.file_id WHERE e.kind='tool_result' AND e.call_id IS NOT NULL GROUP BY f.harness,e.session_id,e.call_id;
PRAGMA user_version=2;
