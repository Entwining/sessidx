use crate::{
    adapters,
    discovery::{self, Root},
    model::{Agent, EventKind, State, name},
    normalize::hash,
    redaction::{redact_metadata, spaced_cjk},
    source,
};
use anyhow::{Context, Result};
use rusqlite::{
    Connection, OptionalExtension, ToSql, Transaction, params,
    types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef},
};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const MAX_RECORD: usize = 16 * 1024 * 1024;
pub const SCHEMA_VERSION: i64 = 11;

#[derive(Default, Debug, Serialize)]
pub struct Refresh {
    pub deferred_tails: usize,
    pub parse_errors: u64,
    pub unknown_records: u64,
    pub files_changed: usize,
    pub records: usize,
    pub stale: bool,
    pub continuation: Option<String>,
    pub missing_roots: Vec<String>,
    pub writer_busy: bool,
}

pub struct Store {
    pub db: Connection,
    pub path: PathBuf,
}

pub struct WriterLock {
    file: File,
}
impl Drop for WriterLock {
    /// A child process spawned while this lock is held keeps a copy of its
    /// descriptor until it execs, and `flock` releases on close only when every
    /// copy is closed; an explicit unlock releases it at once.
    fn drop(&mut self) {
        // Closing the file still releases the lock once every copy is gone.
        let _ = self.file.unlock();
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum FileCoverage {
    Ready,
    DeferredTail,
    BudgetExhausted,
}
impl ToSql for FileCoverage {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Self::Ready => "ready",
            Self::DeferredTail => "deferred_tail",
            Self::BudgetExhausted => "budget_exhausted",
        }
        .into())
    }
}
impl FromSql for FileCoverage {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "ready" => Ok(Self::Ready),
            "deferred_tail" => Ok(Self::DeferredTail),
            "budget_exhausted" => Ok(Self::BudgetExhausted),
            other => Err(FromSqlError::Other(
                format!("unknown index_status {other}").into(),
            )),
        }
    }
}
/// The files row of a previously indexed path.
struct FileRow {
    id: i64,
    dev: u64,
    inode: u64,
    size: u64,
    mtime: String,
    prefix_hash: String,
    prefix_len: u64,
    bytes_indexed: u64,
    lines_indexed: u64,
    state_json: String,
    parse_errors: u64,
    index_status: FileCoverage,
    agent: String,
}
struct IndexedFile {
    changed: bool,
    records: usize,
    coverage: FileCoverage,
}
impl WriterLock {
    fn acquire(path: &Path, shared: bool) -> Result<Option<Self>> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        match if shared {
            f.try_lock_shared()
        } else {
            f.try_lock()
        } {
            Ok(()) => Ok(Some(Self { file: f })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_mode(path, false)
    }

    pub fn open_for_rebuild(path: &Path) -> Result<Self> {
        Self::open_mode(path, true)
    }

    pub fn require_schema(db: &Connection) -> Result<()> {
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            version == SCHEMA_VERSION,
            "database schema changed; run sessidx index --full"
        );
        Ok(())
    }

    fn open_mode(path: &Path, rebuilding: bool) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(50))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            version == 0
                || version == SCHEMA_VERSION
                || rebuilding && (1..SCHEMA_VERSION).contains(&version),
            "database schema changed; run sessidx index --full"
        );
        Ok(Self {
            db,
            path: path.into(),
        })
    }

    pub fn lock(&self) -> Result<Option<WriterLock>> {
        WriterLock::acquire(&self.path, false)
    }

    pub fn read_lock(path: &Path) -> Result<Option<WriterLock>> {
        WriterLock::acquire(path, true)
    }

    fn initialize(&mut self, _lock: &WriterLock, full: bool) -> Result<()> {
        // Checkpointing every 10,000 pages instead of SQLite's default 1,000
        // built the maintainer's logs 7-13% faster, with the WAL peaking near
        // 100 MiB instead of 80 MiB.
        self.db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-8192; PRAGMA wal_autocheckpoint=10000;",
        )?;
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            version == 0
                || version == SCHEMA_VERSION
                || full && (1..SCHEMA_VERSION).contains(&version),
            "database schema changed; run sessidx index --full"
        );
        if version == 0 {
            self.db.execute_batch("PRAGMA journal_mode=WAL;")?;
            self.db.execute_batch(include_str!("schema.sql"))?;
        }
        if full && version != 0 {
            // With foreign keys on, each DROP TABLE first deletes its rows,
            // firing triggers on tables that may already be gone.
            self.db.execute_batch("PRAGMA foreign_keys=OFF;")?;
            let tx = self.db.transaction()?;
            // The schema recreates the index identity at random, so that
            // every earlier cursor is refused instead of resolving to new rows
            // whose file IDs restart.
            let objects: Vec<(String, String)> = tx
                .prepare("SELECT type,name FROM pragma_table_list WHERE schema='main' AND type IN ('view','table','virtual') AND name NOT LIKE 'sqlite_%' ORDER BY type='view' DESC")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (kind, name) in objects {
                let kind = if kind == "view" { "VIEW" } else { "TABLE" };
                tx.execute_batch(&format!("DROP {kind} IF EXISTS \"{name}\";"))?;
            }
            tx.execute_batch(include_str!("schema.sql"))?;
            tx.commit()?;
            self.db.execute_batch("PRAGMA foreign_keys=ON;")?;
            self.db.execute_batch("VACUUM;")?;
        }
        // Keys whose derived columns a transaction must recompute before it
        // commits, and the rows whose values change; see `settle`.
        self.db.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS dirty_groups(agent TEXT, native_id BLOB, kind TEXT, PRIMARY KEY(agent,native_id,kind)) WITHOUT ROWID;
             CREATE TEMP TABLE IF NOT EXISTS dirty_calls(agent TEXT, session_id TEXT, call_id BLOB, PRIMARY KEY(agent,session_id,call_id)) WITHOUT ROWID;
             CREATE TEMP TABLE IF NOT EXISTS derived_changes(id INTEGER PRIMARY KEY, canonical INTEGER, call_ok INTEGER, call_ok_source TEXT, call_event_id INTEGER);",
        )?;
        Ok(())
    }

    pub fn refresh(
        &mut self,
        roots: &[Root],
        full: bool,
        budget: Option<Duration>,
    ) -> Result<Refresh> {
        let deadline = budget.map(|d| Instant::now() + d);
        let Some(_lock) = self.lock()? else {
            return Ok(Refresh {
                stale: true,
                writer_busy: true,
                ..Refresh::default()
            });
        };
        let discovery::Discovered {
            files,
            missing_roots,
        } = discovery::files(roots)?;
        // Read before initialize, which drops these rows on a full rebuild.
        let indexed: Vec<(i64, String)> = if self.db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='files'",
            [],
            |r| r.get::<_, i64>(0),
        )? == 1
        {
            self.db
                .prepare("SELECT id,path FROM files")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        } else {
            Vec::new()
        };
        // A missing root that never held indexed files is one this user does not
        // have; one that did (moved, unmounted) leaves its rows unverified.
        let unverified = roots.iter().find(|r| {
            !r.path.exists()
                && indexed
                    .iter()
                    .any(|(_, path)| Path::new(path).starts_with(&r.path))
        });
        // A rebuild would drop the rows that keep later refreshes stale.
        if let (true, Some(root)) = (full, unverified) {
            anyhow::bail!(
                "{} root {} is missing but holds indexed sessions; restore it, or rebuild with --root for each root that remains",
                name(root.agent),
                root.path.display()
            );
        }
        self.initialize(&_lock, full)?;
        let mut report = Refresh {
            missing_roots,
            stale: unverified.is_some(),
            ..Refresh::default()
        };
        let existing = if full { Vec::new() } else { indexed };
        let discovered: HashSet<_> = files.iter().map(|(_, p)| p.as_path()).collect();
        let present: Vec<&Path> = roots
            .iter()
            .map(|r| r.path.as_path())
            .filter(|root| root.exists())
            .collect();
        let vanished: Vec<(i64, String)> = existing
            .into_iter()
            .filter(|(_, path)| {
                present.iter().any(|root| Path::new(path).starts_with(root))
                    && !discovered.contains(Path::new(path))
            })
            .collect();
        for (agent, path) in files {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                report.stale = true;
                report.continuation = Some(path.to_string_lossy().into_owned());
                break;
            }
            let result = self.index_file(agent, &path, deadline)?;
            report.files_changed += usize::from(result.changed);
            report.records += result.records;
            if result.coverage == FileCoverage::DeferredTail {
                report.deferred_tails += 1;
            }
            if result.coverage == FileCoverage::BudgetExhausted {
                report.stale = true;
                report.continuation = Some(path.to_string_lossy().into_owned());
                break;
            }
        }
        // Rows of vanished files go after new records, which lookups need
        // first, and within the same budget, because each deletion is a
        // transaction that cannot be interrupted. Rule: docs/formats.md
        // `refresh_budget`.
        for (id, path) in vanished {
            if report.continuation.is_some() {
                break;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                report.stale = true;
                report.continuation = Some(path);
                break;
            }
            let tx = self.db.transaction()?;
            delete_file(&tx, id)?;
            settle(&tx, None)?;
            tx.commit()?;
        }
        if full {
            // A build interleaves the pages of every table, one transaction per
            // file, so whole-table reads on a cold cache are bound by scattered
            // page reads; VACUUM rewrites each table into a run of its own.
            // Rule: docs/formats.md `rebuild_compaction`.
            self.db
                .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        }
        report.parse_errors =
            self.db
                .query_row("SELECT coalesce(sum(parse_errors),0) FROM files", [], |r| {
                    r.get(0)
                })?;
        report.unknown_records = self.db.query_row(
            "SELECT coalesce(sum(n),0) FROM shapes WHERE known=0",
            [],
            |r| r.get(0),
        )?;
        Ok(report)
    }

    fn index_file(
        &mut self,
        agent: Agent,
        path: &Path,
        deadline: Option<Instant>,
    ) -> Result<IndexedFile> {
        let path_text = path.to_string_lossy();
        let agent_name = name(agent);
        let old = self
            .db
            .prepare_cached("SELECT * FROM files WHERE path=?")?
            .query_row([path_text.as_ref()], |r| {
                Ok(FileRow {
                    id: r.get("id")?,
                    dev: r.get("dev")?,
                    inode: r.get("inode")?,
                    size: r.get("size")?,
                    mtime: r.get("mtime")?,
                    prefix_hash: r.get("prefix_hash")?,
                    prefix_len: r.get("prefix_len")?,
                    bytes_indexed: r.get("bytes_indexed")?,
                    lines_indexed: r.get("lines_indexed")?,
                    state_json: r.get("state_json")?,
                    parse_errors: r.get("parse_errors")?,
                    index_status: r.get("index_status")?,
                    agent: r.get("agent")?,
                })
            })
            .optional()?;
        // Another agent parses the same bytes differently, so neither the
        // cached rows nor an append to them can be reused. Only a refresh that
        // ran out of budget leaves an unchanged file's records unread. Most
        // files a refresh visits are unchanged, so a stat decides before opening.
        if let Some(ref o) = old
            && o.agent == agent_name
            && o.index_status != FileCoverage::BudgetExhausted
            && let Ok(meta) = source::metadata(path)
            && o.dev == meta.dev()
            && o.inode == meta.ino()
            && o.size == meta.len()
            && o.mtime == change_stamp(&meta)
        {
            return Ok(IndexedFile {
                changed: false,
                records: 0,
                coverage: if o.index_status == FileCoverage::DeferredTail {
                    FileCoverage::DeferredTail
                } else {
                    FileCoverage::Ready
                },
            });
        }
        // The agent deleted or archived the log after discovery listed it.
        let Some(mut source) = source::open(path).context("cannot open session file")? else {
            if let Some(ref o) = old {
                let tx = self.db.transaction()?;
                delete_file(&tx, o.id)?;
                settle(&tx, None)?;
                tx.commit()?;
            }
            return Ok(IndexedFile {
                changed: old.is_some(),
                records: 0,
                coverage: FileCoverage::Ready,
            });
        };
        let meta = source.meta.clone();
        let source_size = meta.len();
        let mtime = change_stamp(&meta);
        let prefix_len = source_size.min(4096);
        let mut prefix = vec![0; prefix_len as usize];
        source.file.read_exact(&mut prefix)?;
        let prefix_hash = hash(&prefix);
        let appended = old.as_ref().filter(|o| {
            o.agent == agent_name
                && o.dev == meta.dev()
                && o.inode == meta.ino()
                && source_size >= o.size
                && o.prefix_len <= prefix_len
                && hash(&prefix[..o.prefix_len as usize]) == o.prefix_hash
                && (source_size > o.size || o.mtime == mtime)
        });
        let (mut offset, mut line, mut state, mut errors) = if let Some(o) = appended {
            (
                o.bytes_indexed,
                o.lines_indexed,
                serde_json::from_str::<State>(&o.state_json)?,
                o.parse_errors,
            )
        } else {
            let state = State {
                session_id: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                session_kind_source: "none".into(),
                ..State::default()
            };
            (0, 0, state, 0)
        };
        let tx = self.db.transaction()?;
        if appended.is_none()
            && let Some(ref o) = old
        {
            delete_file(&tx, o.id)?;
        }
        tx.execute("INSERT INTO files(path,agent,dev,inode,size,mtime,prefix_hash,prefix_len,state_json) VALUES (?,?,?,?,?,?,?,?,?) ON CONFLICT(path) DO UPDATE SET dev=excluded.dev,inode=excluded.inode,size=excluded.size,mtime=excluded.mtime,prefix_hash=excluded.prefix_hash,prefix_len=excluded.prefix_len", params![path_text, agent_name, meta.dev(), meta.ino(), source_size, mtime, prefix_hash, prefix_len, serde_json::to_string(&state)?])?;
        let file_id: i64 = tx.query_row(
            "SELECT id FROM files WHERE path=?",
            [path_text.as_ref()],
            |r| r.get(0),
        )?;
        // The committed identity and rank of an appended file's session: when
        // the records read below change either, every row of the file derives
        // from new keys.
        let before: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT session_id,started_at FROM sessions WHERE file_id=?",
                [file_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let mut first_event = None;
        let mut bounds: Option<(String, String)> = None;
        let compressed = source.compressed;
        let mut reader = source.records_from(offset)?;
        let mut buffer = Vec::new();
        let mut records = 0;
        let mut coverage = FileCoverage::Ready;
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                coverage = FileCoverage::BudgetExhausted;
                break;
            }
            let (length, complete, oversized) = match read_record(&mut reader, &mut buffer) {
                Ok(record) => record,
                // zstd reports undecodable content as an error without an OS
                // code; records decoded before it stay indexed.
                Err(e) if compressed => {
                    if e.raw_os_error().is_some() {
                        return Err(e.into());
                    }
                    errors += 1;
                    tx.prepare_cached("INSERT INTO shapes VALUES (?,'invalid_zstd',0,1) ON CONFLICT(file_id,signature,known) DO UPDATE SET n=n+1")?.execute([file_id])?;
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            if length == 0 || !complete {
                if length > 0 {
                    coverage = FileCoverage::DeferredTail;
                }
                break;
            }
            line += 1;
            records += 1;
            let parsed = if oversized {
                None
            } else {
                crate::normalize::record_for_index(&buffer, agent).ok()
            };
            if let Some(v) = parsed {
                let normalized = adapters::parse(agent, &v, &mut state);
                let signature = shape(&v);
                let raw_hash = hash(&buffer);
                tx.prepare_cached("INSERT INTO shapes VALUES (?,?,?,1) ON CONFLICT(file_id,signature,known) DO UPDATE SET n=n+1")?.execute(params![file_id,signature,normalized.known])?;
                let mut line_roles: Vec<String> = Vec::new();
                for (ordinal, event) in normalized.events.into_iter().enumerate() {
                    let line_role_first = !line_roles.contains(&event.role);
                    if line_role_first {
                        line_roles.push(event.role.clone());
                    }
                    if let Some(ts) = &event.ts {
                        bounds = Some(match bounds {
                            Some((min, max)) => (min.min(ts.clone()), max.max(ts.clone())),
                            None => (ts.clone(), ts.clone()),
                        });
                    }
                    let model = event.model.as_deref().or(state.model.as_deref());
                    let model_source = if event.model.is_some() {
                        "message.model"
                    } else if state.model.is_some() {
                        &state.model_source
                    } else {
                        "none"
                    };
                    // Derived columns start as if the event had no copy and no
                    // result; `settle` corrects every key these rows touch.
                    let call_ok_source = (event.kind == EventKind::ToolCall).then_some("none");
                    tx.prepare_cached("INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,native_id,ts,role,role_source,kind,kind_source,model,model_source,tool,call_id,ok,ok_source,exit_code,line_role_first,canonical,call_ok_source) VALUES (?,?,?,?,?,unhex(?),unhex(?),?,?,?,?,?,?,?,?,unhex(?),?,?,?,?,?,?)")?.execute(params![file_id,line,offset,length,ordinal,&raw_hash,event.native_id,event.ts,event.role,event.role_source,event.kind.as_str(),event.kind_source,model,model_source,event.tool,event.call_id,event.ok,event.ok_source,event.exit_code,line_role_first,true,call_ok_source])?;
                    let event_id = tx.last_insert_rowid();
                    first_event.get_or_insert(event_id);
                    for (site, command) in event.sites.into_iter().enumerate() {
                        tx.prepare_cached("INSERT INTO commands(event_id,site,program,argv_json,parsed) VALUES (?,?,?,?,?)")?.execute(params![event_id,site,command.program,serde_json::to_string(&command.argv)?,command.parsed])?;
                    }
                    for denial in event.denials {
                        tx.prepare_cached(
                            "INSERT INTO denials(event_id,source,class,producer,reason_id) VALUES (?,?,?,?,?)",
                        )?
                        .execute(params![
                            event_id,
                            denial.source,
                            denial.class,
                            denial.producer,
                            denial.reason_id
                        ])?;
                    }
                    if let Some(text) = event.text {
                        tx.prepare_cached(
                            "INSERT INTO event_texts(event_id,text,truncated) VALUES (?,?,?)",
                        )?
                        .execute(params![
                            event_id,
                            &*text,
                            event.text_truncated
                        ])?;
                        tx.prepare_cached("INSERT INTO fts(rowid,text) VALUES (?,?)")?
                            .execute(params![event_id, spaced_cjk(&text)])?;
                    }
                }
            } else {
                errors += 1;
                tx.prepare_cached("INSERT INTO shapes VALUES (?,?,0,1) ON CONFLICT(file_id,signature,known) DO UPDATE SET n=n+1")?.execute(params![file_id,if oversized { "oversized_record" } else { "invalid_json" }])?;
            }
            offset += length;
        }
        let identity_changed =
            before.is_some_and(|(id, started)| id != state.session_id || started != state.first_ts);
        if identity_changed {
            mark_file(&tx, file_id)?;
        }
        let (min_ts, max_ts) = bounds.unzip();
        // SQLite's two-argument min and max are NULL when either side is.
        tx.execute("INSERT INTO sessions(file_id,agent,session_id,parent_id,session_kind,session_kind_source,path,cwd,started_at,instruction_hash,min_ts,max_ts) VALUES (?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET session_id=excluded.session_id,parent_id=excluded.parent_id,session_kind=excluded.session_kind,session_kind_source=excluded.session_kind_source,cwd=excluded.cwd,started_at=excluded.started_at,instruction_hash=excluded.instruction_hash,min_ts=coalesce(min(min_ts,excluded.min_ts),min_ts,excluded.min_ts),max_ts=coalesce(max(max_ts,excluded.max_ts),max_ts,excluded.max_ts)", params![file_id,agent_name,state.session_id,state.parent_id,name(state.session_kind),state.session_kind_source,path_text,state.cwd,state.first_ts,state.instruction_hash,min_ts,max_ts])?;
        if identity_changed {
            mark_file(&tx, file_id)?;
        } else if let Some(first) = first_event {
            // Event IDs only grow, so this transaction's rows are those from
            // its first insert on.
            tx.execute(MARK_GROUPS_FROM, [first])?;
            tx.execute(MARK_CALLS_FROM, [first])?;
        }
        tx.execute("UPDATE files SET bytes_indexed=?,lines_indexed=?,state_json=?,parse_errors=?,index_status=? WHERE id=?", params![offset,line,serde_json::to_string(&state)?,errors,coverage,file_id])?;
        settle(&tx, first_event)?;
        // This transaction's rows count once, with the values `settle` gave them.
        if let Some(first) = first_event {
            add_counts(&tx, 1, "e.id>=?", [first])?;
        }
        tx.commit()?;
        Ok(IndexedFile {
            changed: true,
            records,
            coverage,
        })
    }
}

/// The `files.mtime` value: modification and status-change times, so a rewrite
/// that restores the size and modification time still reads as changed.
fn change_stamp(meta: &fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}",
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec()
    )
}

fn delete_file(tx: &Transaction<'_>, id: i64) -> Result<()> {
    // By natural key, before the cascade removes the rows that name the keys.
    mark_file(tx, id)?;
    tx.execute("INSERT INTO index_changes(file_id,first_event_id) SELECT file_id,min(id) FROM event_rows WHERE file_id=? HAVING count(*)>0", [id])?;
    // A result elsewhere whose call is in this file contributes that call's
    // sites to the rollups, so it lets go of the call while the sites can still
    // be subtracted; `settle` pairs it again once the file is gone.
    tx.execute("INSERT INTO temp.derived_changes(id,canonical,call_ok,call_ok_source,call_event_id) SELECT r.id,r.canonical,r.call_ok,r.call_ok_source,NULL FROM event_rows c JOIN event_rows r ON r.call_id=c.call_id AND r.call_event_id=c.id WHERE c.file_id=?1 AND c.kind IN ('tool_call','server_tool_call') AND r.file_id<>?1", [id])?;
    apply_changes(tx, None)?;
    tx.execute("DELETE FROM files WHERE id=?", [id])?;
    Ok(())
}

// The native groups and call keys of a file's rows, or of rows from an event ID
// on, whose derived columns `settle` recomputes.
const MARK_GROUPS: &str = "INSERT OR IGNORE INTO temp.dirty_groups SELECT s.agent,r.native_id,r.kind FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE r.file_id=? AND r.native_id IS NOT NULL";
const MARK_CALLS: &str = "INSERT OR IGNORE INTO temp.dirty_calls SELECT s.agent,s.session_id,r.call_id FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE r.file_id=? AND r.call_id IS NOT NULL AND r.kind IN ('tool_call','server_tool_call','tool_result')";
const MARK_GROUPS_FROM: &str = "INSERT OR IGNORE INTO temp.dirty_groups SELECT s.agent,r.native_id,r.kind FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE r.id>=? AND r.native_id IS NOT NULL";
const MARK_CALLS_FROM: &str = "INSERT OR IGNORE INTO temp.dirty_calls SELECT s.agent,s.session_id,r.call_id FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE r.id>=? AND r.call_id IS NOT NULL AND r.kind IN ('tool_call','server_tool_call','tool_result')";

fn mark_file(tx: &Transaction<'_>, file_id: i64) -> Result<()> {
    tx.execute(MARK_GROUPS, [file_id])?;
    tx.execute(MARK_CALLS, [file_id])?;
    Ok(())
}

/// A table of per-transcript sums by UTC day that the writer keeps equal to
/// the sums over its events: each transaction adds the contribution of the
/// rows it inserts once their derived columns are settled, and subtracts and
/// re-adds that of each earlier row whose derived columns change. Rule:
/// docs/formats.md `daily_counts`.
pub struct Rollup {
    pub table: &'static str,
    keys: &'static [&'static str],
    pub counters: &'static [&'static str],
    events: fn(&str) -> String,
}

pub const DAILY_COUNTS: Rollup = Rollup {
    table: "daily_counts",
    keys: &["file_id", "day", "kind", "role", "model", "canonical"],
    counters: &[
        "events",
        "failed",
        "succeeded",
        "denied",
        "denied_reason_unknown",
        "denial_rows",
        "denial_rows_reason_unknown",
        "sites",
        "sites_without_program",
        "shell_calls",
        "unparsed_calls",
        "unresolved_program",
    ],
    events: event_counts,
};

pub const PROGRAM_COUNTS: Rollup = Rollup {
    table: "daily_program_counts",
    keys: &["file_id", "day", "kind", "role", "model", "program"],
    counters: &[
        "events",
        "failed",
        "succeeded",
        "denied",
        "denied_reason_unknown",
        "unresolved_program",
        "matching_sites",
    ],
    events: program_counts,
};

/// The event whose shell sites hold a tool call's or tool result's program.
const SITES_OF: &str =
    "CASE e.kind WHEN 'tool_call' THEN e.id WHEN 'tool_result' THEN e.call_event_id END";

/// Each event's contribution to `daily_counts`, for the events `selection`
/// picks from `event_rows e`. Grouping by `+e.id` rather than `e.id` keeps
/// SQLite 3.53 from scanning every event in row-ID order to skip the sort,
/// instead of reaching the selection by its index.
fn event_counts(selection: &str) -> String {
    format!(
        "SELECT e.file_id,substr(e.ts,1,10) AS day,e.kind,e.role,e.model,e.canonical,1 AS events,
          e.call_ok IS 0 AS failed,e.call_ok IS 1 AS succeeded,
          EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id) AS denied,
          EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown') AS denied_reason_unknown,
          (SELECT count(*) FROM denials d WHERE d.event_id=e.id) AS denial_rows,
          (SELECT count(*) FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown') AS denial_rows_reason_unknown,
          coalesce(sum(c.parsed=1),0) AS sites,coalesce(sum(c.parsed=1 AND c.program IS NULL),0) AS sites_without_program,
          count(c.id)>0 AS shell_calls,coalesce(max(c.parsed=0),0) AS unparsed_calls,
          EXISTS(SELECT 1 FROM commands u WHERE u.event_id={SITES_OF} AND (u.parsed=0 OR u.program IS NULL)) AS unresolved_program
         FROM event_rows e LEFT JOIN commands c ON c.event_id=e.id WHERE {selection} GROUP BY +e.id"
    )
}

/// Each first-copy event's contribution to `daily_program_counts`, once per
/// program its call's sites name, for the events `selection` picks from
/// `event_rows e` joined to those sites as `commands c`.
fn program_counts(selection: &str) -> String {
    format!(
        "SELECT e.file_id,substr(e.ts,1,10) AS day,e.kind,e.role,e.model,c.program,1 AS events,
          e.call_ok IS 0 AS failed,e.call_ok IS 1 AS succeeded,
          EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id) AS denied,
          EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown') AS denied_reason_unknown,
          EXISTS(SELECT 1 FROM commands u WHERE u.event_id=c.event_id AND (u.parsed=0 OR u.program IS NULL)) AS unresolved_program,
          CASE WHEN e.kind='tool_call' THEN count(*) ELSE 0 END AS matching_sites
         FROM event_rows e JOIN commands c ON c.event_id={SITES_OF}
         WHERE {selection} AND e.canonical=1 AND c.program IS NOT NULL GROUP BY e.id,c.program"
    )
}

impl Rollup {
    /// The rollup's rows for the events `selection` picks, as the table holds them.
    pub fn grouped(&self, selection: &str) -> String {
        let keys = self.keys.join(",");
        format!(
            "SELECT {keys},{} FROM ({}) GROUP BY {keys}",
            self.counters
                .iter()
                .map(|c| format!("sum({c}) AS {c}"))
                .collect::<Vec<_>>()
                .join(","),
            (self.events)(selection)
        )
    }

    /// Adds `sign` times the contribution of the selected events, and drops
    /// the rows left empty.
    fn add(
        &self,
        tx: &Transaction<'_>,
        sign: i64,
        selection: &str,
        params: impl rusqlite::Params,
    ) -> Result<()> {
        let table = self.table;
        let keys = self.keys.join(",");
        // An empty blob, which no text equals, keys an absent day or model.
        let conflict = self
            .keys
            .iter()
            .map(|&k| match k {
                "day" | "model" => format!("ifnull({k},x'')"),
                _ => k.to_owned(),
            })
            .collect::<Vec<_>>()
            .join(",");
        tx.prepare_cached(&format!(
            "INSERT INTO {table} SELECT {keys},{} FROM ({}) WHERE true
             ON CONFLICT({conflict}) DO UPDATE SET {}",
            self.counters
                .iter()
                .map(|c| format!("{sign}*{c}"))
                .collect::<Vec<_>>()
                .join(","),
            self.grouped(selection),
            self.counters
                .iter()
                .map(|c| format!("{c}={c}+excluded.{c}"))
                .collect::<Vec<_>>()
                .join(","),
        ))?
        .execute(params)?;
        tx.prepare_cached(&format!("DELETE FROM {table} WHERE events=0"))?
            .execute([])?;
        Ok(())
    }
}

/// Adds `sign` times the contribution of the selected events to every rollup.
fn add_counts<P: rusqlite::Params + Copy>(
    tx: &Transaction<'_>,
    sign: i64,
    selection: &str,
    params: P,
) -> Result<()> {
    DAILY_COUNTS.add(tx, sign, selection, params)?;
    PROGRAM_COUNTS.add(tx, sign, selection, params)
}

/// Recomputes the derived columns of every key a transaction marked from the
/// rows it is about to commit, and moves the rollup contribution of each row
/// before `first`, the transaction's first inserted event, whose values
/// change; the transaction adds its own rows afterwards. Rules: docs/formats.md `canonical_first_copy`,
/// `call_outcome_evidence`, `call_pairing`, `derived_facts`, and `daily_counts`.
fn settle(tx: &Transaction<'_>, first: Option<i64>) -> Result<()> {
    // Each statement starts at the marked keys and reaches rows by ID. Rule:
    // docs/formats.md `derived_work`.
    tx.execute_batch(
        "WITH w AS MATERIALIZED (SELECT d.agent,d.native_id,d.kind,(
           SELECT r2.id FROM event_rows r2 JOIN sessions s2 ON s2.file_id=r2.file_id
           WHERE r2.native_id=d.native_id AND r2.kind=d.kind AND s2.agent=d.agent
           ORDER BY coalesce(s2.started_at,'9999'),s2.path,r2.line_no,r2.ordinal LIMIT 1) AS winner
          FROM temp.dirty_groups d)
         INSERT INTO temp.derived_changes(id,canonical,call_ok,call_ok_source,call_event_id)
          SELECT r.id,NOT r.canonical,r.call_ok,r.call_ok_source,r.call_event_id FROM w CROSS JOIN event_rows r CROSS JOIN sessions s
          WHERE r.native_id=w.native_id AND r.kind=w.kind AND s.file_id=r.file_id AND s.agent=w.agent
           AND r.canonical IS NOT (r.id=w.winner);
         WITH o AS MATERIALIZED (SELECT d.agent,d.session_id,d.call_id,
           CASE WHEN sum(r.ok=0)>0 THEN 0 WHEN sum(r.ok=1)>0 THEN 1 END AS ok,
           CASE WHEN sum(r.ok=0 AND r.ok_source='flag')>0 THEN 'flag'
                WHEN sum(r.ok=0 AND r.ok_source='text')>0 THEN 'text'
                WHEN sum(r.ok_source='flag')>0 THEN 'flag'
                WHEN sum(r.ok_source='text')>0 THEN 'text' ELSE 'none' END AS source
          FROM temp.dirty_calls d LEFT JOIN event_rows r ON r.call_id=d.call_id AND r.kind='tool_result'
           AND r.file_id IN (SELECT file_id FROM sessions WHERE agent=d.agent AND session_id=d.session_id)
          GROUP BY d.agent,d.session_id,d.call_id)
         INSERT INTO temp.derived_changes(id,canonical,call_ok,call_ok_source,call_event_id)
          SELECT c.id,c.canonical,o.ok,o.source,NULL FROM o CROSS JOIN event_rows c CROSS JOIN sessions s
          WHERE c.call_id=o.call_id AND c.kind='tool_call' AND s.file_id=c.file_id
           AND s.agent=o.agent AND s.session_id=o.session_id
           AND (c.call_ok IS NOT o.ok OR c.call_ok_source IS NOT o.source)
          ON CONFLICT(id) DO UPDATE SET call_ok=excluded.call_ok,call_ok_source=excluded.call_ok_source;
         WITH p AS MATERIALIZED (SELECT d.agent,d.session_id,d.call_id,(
           SELECT c.id FROM event_rows c JOIN sessions cs ON cs.file_id=c.file_id
           WHERE c.call_id=d.call_id AND c.kind IN ('tool_call','server_tool_call')
            AND cs.agent=d.agent AND cs.session_id=d.session_id
           ORDER BY cs.path,c.line_no,c.ordinal LIMIT 1) AS call
          FROM temp.dirty_calls d)
         INSERT INTO temp.derived_changes(id,canonical,call_ok,call_ok_source,call_event_id)
          SELECT r.id,r.canonical,r.call_ok,r.call_ok_source,p.call FROM p CROSS JOIN event_rows r CROSS JOIN sessions s
          WHERE r.call_id=p.call_id AND r.kind='tool_result' AND s.file_id=r.file_id
           AND s.agent=p.agent AND s.session_id=p.session_id AND r.call_event_id IS NOT p.call
          ON CONFLICT(id) DO UPDATE SET call_event_id=excluded.call_event_id;",
    )?;
    apply_changes(tx, first)?;
    tx.execute_batch("DELETE FROM temp.dirty_groups; DELETE FROM temp.dirty_calls;")?;
    Ok(())
}

/// Stores the derived values in `temp.derived_changes`, moving the rollup
/// contribution of each changed row before `first` from its old values to its
/// new ones; rows from `first` on are not counted yet.
fn apply_changes(tx: &Transaction<'_>, first: Option<i64>) -> Result<()> {
    let changed = "e.id IN (SELECT id FROM temp.derived_changes WHERE id<?)";
    let before = [first.unwrap_or(i64::MAX)];
    add_counts(tx, -1, changed, before)?;
    tx.execute_batch(
        "UPDATE event_rows SET (canonical,call_ok,call_ok_source,call_event_id)=(
           SELECT canonical,call_ok,call_ok_source,call_event_id FROM temp.derived_changes c WHERE c.id=event_rows.id)
          WHERE id IN (SELECT id FROM temp.derived_changes);",
    )?;
    add_counts(tx, 1, changed, before)?;
    tx.execute_batch("DELETE FROM temp.derived_changes;")?;
    Ok(())
}

fn read_record(reader: &mut impl BufRead, buffer: &mut Vec<u8>) -> io::Result<(u64, bool, bool)> {
    buffer.clear();
    let mut length = 0;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((length, false, oversized));
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |n| n + 1);
        let complete = available[count - 1] == b'\n';
        length += count as u64;
        if !oversized && buffer.len() + count <= MAX_RECORD {
            buffer.extend_from_slice(&available[..count]);
        } else {
            oversized = true;
            buffer.clear();
        }
        reader.consume(count);
        if complete {
            return Ok((length, true, oversized));
        }
    }
}

fn shape(v: &serde_json::Value) -> String {
    let mut keys: Vec<_> = v
        .as_object()
        .into_iter()
        .flat_map(|o| o.keys())
        .map(|k| redact_metadata(k))
        .collect();
    keys.sort();
    let typ = redact_metadata(v.get("type").and_then(|s| s.as_str()).unwrap_or("unknown"));
    let subtype = v
        .pointer("/payload/type")
        .or_else(|| v.get("subtype"))
        .or_else(|| v.pointer("/message/role"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let subtype = redact_metadata(subtype);
    format!("{typ}/{subtype}:{}", keys.join(", "))
}
