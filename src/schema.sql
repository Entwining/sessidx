-- `written` counts the records indexed since the database was last compacted. Rule:
-- docs/formats.md `index_compaction`.
CREATE TABLE IF NOT EXISTS index_identity (
 id INTEGER PRIMARY KEY CHECK(id=1), instance TEXT NOT NULL, written INTEGER NOT NULL DEFAULT 0
);
INSERT INTO index_identity(id,instance) SELECT 1,lower(hex(randomblob(16))) WHERE NOT EXISTS(SELECT 1 FROM index_identity);
-- A replaced or deleted file invalidates only the cursors that have yet to read it.
-- Rule: docs/formats.md `cursor_invalidation`.
CREATE TABLE IF NOT EXISTS index_changes (
 id INTEGER PRIMARY KEY AUTOINCREMENT, file_id INTEGER NOT NULL, first_event_id INTEGER NOT NULL
);
-- File IDs are never reused, so a cursor or frozen search order that names a file
-- cannot resolve to its replacement.
CREATE TABLE IF NOT EXISTS files (
 id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL UNIQUE, agent TEXT NOT NULL,
 dev INTEGER NOT NULL, inode INTEGER NOT NULL, size INTEGER NOT NULL, mtime TEXT NOT NULL,
 prefix_hash TEXT NOT NULL, prefix_len INTEGER NOT NULL,
 bytes_indexed INTEGER NOT NULL DEFAULT 0, lines_indexed INTEGER NOT NULL DEFAULT 0,
 state_json TEXT NOT NULL, parse_errors INTEGER NOT NULL DEFAULT 0,
 index_status TEXT NOT NULL DEFAULT 'ready', parser INTEGER NOT NULL
);
-- ready, deferred_tail, and budget_exhausted distinguish expected live tails from staleness.
-- `parser` is the PARSER_VERSION that produced the file's rows. Rule: docs/formats.md
-- `parser_version`.
-- Each transcript file is one session, identified by its own header or name rather than
-- by history copied into it. Rule: docs/formats.md `transcript_identity`.
CREATE TABLE IF NOT EXISTS sessions (
 file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
 agent TEXT NOT NULL, session_id TEXT NOT NULL, parent_id TEXT,
 session_kind TEXT NOT NULL, session_kind_source TEXT NOT NULL,
 path TEXT NOT NULL, cwd TEXT NOT NULL, started_at TEXT, instruction_hash TEXT,
 min_ts TEXT, max_ts TEXT
);
-- min_ts and max_ts bound the file's event timestamps, so that a time-filtered grep
-- skips transcripts outside its window. Rule: docs/formats.md `grep_traversal`.
CREATE INDEX IF NOT EXISTS sessions_id ON sessions(session_id);
CREATE UNIQUE INDEX IF NOT EXISTS sessions_path ON sessions(path);
-- Every indexed event needs a range for show/grep and identity for dedup. Source files are
-- the authority, and a range includes its record's newline. Bodies live in event_texts so
-- that scans over this metadata stay narrow.
CREATE TABLE IF NOT EXISTS event_rows (
 id INTEGER PRIMARY KEY AUTOINCREMENT, file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 line_no INTEGER NOT NULL, byte_off INTEGER NOT NULL, byte_len INTEGER NOT NULL, ordinal INTEGER NOT NULL,
 raw_hash BLOB NOT NULL CHECK(length(raw_hash)=32), native_id BLOB CHECK(native_id IS NULL OR length(native_id)=32),
 ts TEXT, role TEXT NOT NULL, role_source TEXT NOT NULL, kind TEXT NOT NULL, kind_source TEXT NOT NULL,
 model TEXT, model_source TEXT NOT NULL, tool TEXT, call_id BLOB CHECK(call_id IS NULL OR length(call_id)=32),
 ok INTEGER, ok_source TEXT NOT NULL, exit_code INTEGER,
 line_role_first INTEGER NOT NULL CHECK(line_role_first IN (0,1)),
 canonical INTEGER NOT NULL CHECK(canonical IN (0,1)),
 call_ok INTEGER CHECK(call_ok IS NULL OR (kind='tool_call' AND call_ok IN (0,1))),
 call_ok_source TEXT CHECK((kind='tool_call')=(call_ok_source IS NOT NULL) AND call_ok_source IN ('flag','text','none')),
 call_event_id INTEGER CHECK(call_event_id IS NULL OR kind='tool_result'),
 UNIQUE(file_id,line_no,ordinal)
);
-- `canonical` (first copy of a native event) and `call_ok`/`call_ok_source` (a tool call's
-- outcome from its session's results) depend on other rows. The writer derives them in the
-- transaction that changes their inputs, so queries read stored values instead of probing.
-- `call_event_id` names a tool result's call, whose shell sites decide the program counts
-- read for the result. Rules: docs/formats.md `canonical_first_copy`, `call_outcome_evidence`,
-- `call_pairing`, `derived_facts`.
-- `line_role_first` marks the first event of its role on its source line, the one that
-- grep and show display. Rule: docs/formats.md `line_role_record`.
CREATE INDEX IF NOT EXISTS events_time ON event_rows(ts);
CREATE INDEX IF NOT EXISTS events_native ON event_rows(native_id,kind) WHERE native_id IS NOT NULL;
-- Calls and results pair by call ID within a session.
CREATE INDEX IF NOT EXISTS events_call ON event_rows(call_id) WHERE call_id IS NOT NULL;
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
 e.file_id,e.byte_off,e.byte_len,e.ordinal,lower(hex(e.raw_hash)) AS raw_hash,e.canonical
 FROM event_rows e JOIN sessions s ON s.file_id=e.file_id;
-- Static sites are not execution counts, and a site has no outcome of its own: results
-- belong to the call.
CREATE TABLE IF NOT EXISTS commands (
 id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES event_rows(id) ON DELETE CASCADE,
 site INTEGER NOT NULL, program TEXT, argv_json TEXT, parsed INTEGER NOT NULL
);
-- Child FK indexes avoid scanning all commands/denials when a file is replaced.
CREATE INDEX IF NOT EXISTS commands_event ON commands(event_id);
CREATE TABLE IF NOT EXISTS denials (
 id INTEGER PRIMARY KEY, event_id INTEGER NOT NULL REFERENCES event_rows(id) ON DELETE CASCADE,
 source TEXT NOT NULL, class TEXT NOT NULL, producer TEXT, reason_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS denials_event ON denials(event_id);
-- What each record states about the tokens of the model request its event belongs to: a
-- Claude message written as several records, or copied into another transcript, has a row
-- per record. `finished` is whether the record states that the response ended; `selected`
-- (rule `derived_facts`) marks the one row per request that `requests` reports. `file_id`,
-- `ts` and `model` are the event row's, so that counts do not read `event_rows`. Rules:
-- docs/formats.md `usage_columns`, `usage_snapshot`, `request_route`, `request_stop_reason`.
CREATE TABLE IF NOT EXISTS usage_rows (
 event_id INTEGER PRIMARY KEY REFERENCES event_rows(id) ON DELETE CASCADE,
 file_id INTEGER NOT NULL, ts TEXT, model TEXT,
 route TEXT, route_source TEXT CHECK((route IS NULL)=(route_source IS NULL)),
 finished INTEGER NOT NULL CHECK(finished IN (0,1)), stop_reason TEXT,
 selected INTEGER NOT NULL CHECK(selected IN (0,1)),
 input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER, cache_write INTEGER,
 cache_write_5m INTEGER, cache_write_1h INTEGER, reasoning INTEGER
);
-- Rule: docs/formats.md `usage_count_work`.
CREATE INDEX IF NOT EXISTS usage_time ON usage_rows(ts);
CREATE INDEX IF NOT EXISTS usage_file ON usage_rows(file_id);
-- The model passes of a request whose record lists several, such as an advisor's. Rule:
-- docs/formats.md `claude_usage_passes`.
CREATE TABLE IF NOT EXISTS usage_passes (
 event_id INTEGER NOT NULL REFERENCES usage_rows(event_id) ON DELETE CASCADE,
 pass INTEGER NOT NULL, kind TEXT NOT NULL, model TEXT,
 input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER, cache_write INTEGER,
 cache_write_5m INTEGER, cache_write_1h INTEGER,
 PRIMARY KEY(event_id,pass)
) WITHOUT ROWID;
-- `request_native_id` is the hashed native ID of the request that wrote the compaction.
-- Rules: docs/formats.md `compaction_records`, `codex_compaction_request`.
CREATE TABLE IF NOT EXISTS compaction_rows (
 event_id INTEGER PRIMARY KEY REFERENCES event_rows(id) ON DELETE CASCADE,
 trigger TEXT NOT NULL CHECK(trigger IN ('auto','manual','unknown')), pre_tokens INTEGER, post_tokens INTEGER,
 summary_input INTEGER, summary_output INTEGER, summary_cache_read INTEGER, summary_cache_write INTEGER,
 request_native_id BLOB CHECK(request_native_id IS NULL OR length(request_native_id)=32)
);
-- Records in which the agent states that a model request failed. `model` is the session's
-- model when it failed, because a failure Claude Code reports names `<synthetic>`. Rule:
-- docs/formats.md `request_errors`.
CREATE TABLE IF NOT EXISTS error_rows (
 event_id INTEGER PRIMARY KEY REFERENCES event_rows(id) ON DELETE CASCADE,
 model TEXT, source TEXT NOT NULL CHECK(source IN ('api_error','api_error_message','stop_reason')),
 status INTEGER, code TEXT, retry_attempt INTEGER,
 route TEXT, route_source TEXT CHECK((route IS NULL)=(route_source IS NULL))
);
-- Message/input text already lives in event_texts; FTS retains tokens and positions only.
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(text, content='', contentless_delete=1, tokenize='unicode61');
CREATE TRIGGER IF NOT EXISTS events_delete AFTER DELETE ON event_rows BEGIN DELETE FROM fts WHERE rowid=old.id; END;
-- Native identity, never matching text, deduplicates inherited events for counts.
-- A message's text joins its fragments in the kept row's transcript, each repeated record
-- once. Rule: docs/formats.md `claude_fragment_text`.
CREATE VIEW IF NOT EXISTS canonical_events AS
 SELECT e.id,s.agent,s.session_id,s.path,e.line_no,e.ts,e.role,e.kind,e.model,e.tool,
 CASE WHEN e.call_id IS NULL THEN NULL ELSE lower(hex(e.call_id)) END AS call_id,e.ok,e.exit_code,
 CASE WHEN e.native_id IS NOT NULL AND e.kind='message' THEN coalesce(
  (SELECT group_concat(t.text,char(10) ORDER BY f.line_no,f.ordinal)
   FROM event_rows f JOIN event_texts t ON t.event_id=f.id
   WHERE f.native_id=e.native_id AND f.kind=e.kind AND f.file_id=e.file_id AND t.text<>''
   AND NOT EXISTS (SELECT 1 FROM event_rows d WHERE d.native_id=f.native_id AND d.kind=f.kind
    AND d.file_id=f.file_id AND d.raw_hash=f.raw_hash AND d.id<f.id)),
  (SELECT text FROM event_texts WHERE event_id=e.id))
 ELSE (SELECT text FROM event_texts WHERE event_id=e.id) END AS text,
 coalesce((SELECT truncated FROM event_texts WHERE event_id=e.id),0) AS text_truncated,
 e.role_source,e.kind_source,e.model_source,e.ok_source,
 CASE WHEN e.native_id IS NULL THEN NULL ELSE lower(hex(e.native_id)) END AS native_id,
 e.file_id,e.byte_off,e.byte_len,e.ordinal,lower(hex(e.raw_hash)) AS raw_hash,e.canonical
 FROM event_rows e JOIN sessions s ON s.file_id=e.file_id WHERE e.canonical=1;
-- One row per canonical tool call, with the outcome of its results in its session, which
-- can span transcripts.
CREATE VIEW IF NOT EXISTS call_outcomes AS
 SELECT e.id,s.agent,s.session_id,s.path,e.line_no,e.ts,e.role,e.model,e.tool,
 CASE WHEN e.call_id IS NULL THEN NULL ELSE lower(hex(e.call_id)) END AS call_id,e.file_id,
 e.call_ok AS ok,e.call_ok_source AS ok_source
 FROM event_rows e JOIN sessions s ON s.file_id=e.file_id WHERE e.kind='tool_call' AND e.canonical=1;
-- One row per model request, from its selected record.
CREATE VIEW IF NOT EXISTS requests AS
 SELECT u.event_id AS id,s.agent,s.session_id,s.path,(SELECT line_no FROM event_rows WHERE id=u.event_id) AS line_no,
 u.ts,u.model,u.route,u.route_source,u.stop_reason,u.input,u.cache_read,u.cache_write,u.cache_write_5m,u.cache_write_1h,u.output,u.reasoning,u.finished,
 (SELECT lower(hex(native_id)) FROM event_rows WHERE id=u.event_id AND native_id IS NOT NULL) AS native_id,u.file_id
 FROM usage_rows u JOIN sessions s ON s.file_id=u.file_id WHERE u.selected=1;
-- One row per model pass of a request: the passes its record lists, or the request itself
-- as pass 0 of no kind when it lists none. Rule: docs/formats.md `claude_usage_passes`.
CREATE VIEW IF NOT EXISTS request_passes AS
 SELECT u.event_id AS request_id,p.pass,p.kind,p.model,u.route,u.route_source,s.agent,s.session_id,s.path,u.ts,
 p.input,p.cache_read,p.cache_write,p.cache_write_5m,p.cache_write_1h,p.output,NULL AS reasoning,u.file_id
 FROM usage_rows u JOIN usage_passes p ON p.event_id=u.event_id JOIN sessions s ON s.file_id=u.file_id WHERE u.selected=1
 UNION ALL
 SELECT u.event_id,0,NULL,u.model,u.route,u.route_source,s.agent,s.session_id,s.path,u.ts,
 u.input,u.cache_read,u.cache_write,u.cache_write_5m,u.cache_write_1h,u.output,u.reasoning,u.file_id
 FROM usage_rows u JOIN sessions s ON s.file_id=u.file_id
 WHERE u.selected=1 AND NOT EXISTS (SELECT 1 FROM usage_passes p WHERE p.event_id=u.event_id);
-- One row per compaction, from its first copy, read from the compactions outward, with
-- the `requests` row of the request that wrote it. Rules: docs/formats.md `usage_count_work`,
-- `codex_compaction_request`.
CREATE VIEW IF NOT EXISTS compactions AS
 SELECT c.event_id AS id,s.agent,s.session_id,s.path,e.line_no,e.ts,e.model,c.trigger,c.pre_tokens,c.post_tokens,
 c.summary_input,c.summary_output,c.summary_cache_read,c.summary_cache_write,
 (SELECT u.event_id FROM event_rows r CROSS JOIN usage_rows u ON u.event_id=r.id
  WHERE r.native_id=c.request_native_id AND r.kind='context' AND u.selected=1) AS request_id,e.file_id
 FROM compaction_rows c CROSS JOIN event_rows e ON e.id=c.event_id CROSS JOIN sessions s ON s.file_id=e.file_id WHERE e.canonical=1;
-- One row per first copy of a record stating that a request failed, read from its rows
-- outward. Rule: docs/formats.md `request_errors`.
CREATE VIEW IF NOT EXISTS request_errors AS
 SELECT x.event_id AS id,s.agent,s.session_id,s.path,e.line_no,e.ts,x.model,x.source,x.status,x.code,x.retry_attempt,
 x.route,x.route_source,e.file_id
 FROM error_rows x CROSS JOIN event_rows e ON e.id=x.event_id CROSS JOIN sessions s ON s.file_id=e.file_id WHERE e.canonical=1;
-- What `count` and `doctor` sum per transcript, UTC day, and the event fields counts group
-- by, so that whole days are read here instead of event by event. The writer applies each
-- transaction's changes as deltas. Rule: docs/formats.md `daily_counts`.
CREATE TABLE IF NOT EXISTS daily_counts (
 file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 day TEXT, kind TEXT NOT NULL, role TEXT NOT NULL, model TEXT, canonical INTEGER NOT NULL,
 events INTEGER NOT NULL, failed INTEGER NOT NULL, succeeded INTEGER NOT NULL,
 denied INTEGER NOT NULL, denied_reason_unknown INTEGER NOT NULL,
 denial_rows INTEGER NOT NULL, denial_rows_reason_unknown INTEGER NOT NULL,
 sites INTEGER NOT NULL, sites_without_program INTEGER NOT NULL,
 shell_calls INTEGER NOT NULL, unparsed_calls INTEGER NOT NULL, unresolved_program INTEGER NOT NULL
);
-- An empty blob, which no text equals, keys an absent day or model.
CREATE UNIQUE INDEX IF NOT EXISTS daily_counts_key ON daily_counts(file_id,ifnull(day,x''),kind,role,ifnull(model,x''),canonical);
CREATE INDEX IF NOT EXISTS daily_counts_empty ON daily_counts(file_id) WHERE events=0;
-- The first-copy tool calls and results of `daily_counts` once per program their call's
-- sites name, for `count --program`. Rule: docs/formats.md `daily_counts`.
CREATE TABLE IF NOT EXISTS daily_program_counts (
 file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 day TEXT, kind TEXT NOT NULL, role TEXT NOT NULL, model TEXT, program TEXT NOT NULL,
 events INTEGER NOT NULL, failed INTEGER NOT NULL, succeeded INTEGER NOT NULL,
 denied INTEGER NOT NULL, denied_reason_unknown INTEGER NOT NULL,
 unresolved_program INTEGER NOT NULL, matching_sites INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS daily_program_counts_key ON daily_program_counts(file_id,ifnull(day,x''),kind,role,ifnull(model,x''),program);
CREATE INDEX IF NOT EXISTS daily_program_counts_program ON daily_program_counts(program,kind,day);
CREATE INDEX IF NOT EXISTS daily_program_counts_empty ON daily_program_counts(file_id) WHERE events=0;
PRAGMA user_version=14;
