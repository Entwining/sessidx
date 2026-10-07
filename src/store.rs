use crate::{
    adapters,
    discovery::{self, Root},
    model::State,
    normalize::hash,
    redaction::{redact, redact_metadata, spaced_cjk},
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const MAX_RECORD: usize = 16 * 1024 * 1024;
pub const SCHEMA_VERSION: i64 = 4;

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
    _file: File,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum FileCoverage {
    Ready,
    DeferredTail,
    BudgetExhausted,
}
impl FileCoverage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::DeferredTail => "deferred_tail",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }
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
            Ok(()) => Ok(Some(Self { _file: f })),
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
        self.db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-8192;",
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
            let tx = self.db.transaction()?;
            if version >= 4 {
                tx.execute("INSERT INTO index_changes(first_event_id) VALUES (0)", [])?;
            }
            tx.execute_batch("DROP VIEW call_outcomes; DROP VIEW canonical_events; DROP TABLE commands; DROP TABLE denials;")?;
            if version == 1 {
                tx.execute_batch("DROP TABLE events;")?;
            } else {
                tx.execute_batch("DROP VIEW events; DROP TABLE event_details; DROP TABLE locations; DROP TABLE strings;")?;
            }
            tx.execute_batch(
                "DROP TABLE sessions; DROP TABLE shapes; DROP TABLE files; DROP TABLE fts;",
            )?;
            tx.execute_batch(include_str!("schema.sql"))?;
            tx.commit()?;
            self.db.execute_batch("VACUUM;")?;
        }
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
        // A missing root that never held indexed files is a harness this user
        // does not have; one that did (moved, unmounted) leaves its rows unverified.
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
                root.harness,
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
        for (id, path) in existing {
            if roots
                .iter()
                .any(|r| r.path.exists() && Path::new(&path).starts_with(&r.path))
                && !discovered.contains(Path::new(&path))
            {
                let tx = self.db.transaction()?;
                delete_file(&tx, id)?;
                tx.commit()?;
            }
        }
        for (harness, path) in files {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                report.stale = true;
                report.continuation = Some(path.to_string_lossy().into_owned());
                break;
            }
            let result = self.index_file(&harness, &path, deadline)?;
            report.files_changed += usize::from(result.changed);
            report.records += result.records;
            if result.coverage == FileCoverage::DeferredTail {
                report.deferred_tails += 1;
            }
            if result.coverage == FileCoverage::BudgetExhausted {
                report.stale = true;
                report.continuation = Some(path.to_string_lossy().into_owned());
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    break;
                }
            }
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
        harness: &str,
        path: &Path,
        deadline: Option<Instant>,
    ) -> Result<IndexedFile> {
        let mut file = File::open(path).context("cannot open session file")?;
        let meta = file.metadata()?;
        let source_size = meta.len();
        let mtime = format!(
            "{}:{}:{}:{}",
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec()
        );
        let path_text = path.to_string_lossy();
        let old = self.db.query_row("SELECT id,dev,inode,size,mtime,prefix_hash,prefix_len,bytes_indexed,lines_indexed,state_json,parse_errors,index_status,harness FROM files WHERE path=?", [path_text.as_ref()], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, u64>(1)?, r.get::<_, u64>(2)?, r.get::<_, u64>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, u64>(6)?, r.get::<_, u64>(7)?, r.get::<_, u64>(8)?, r.get::<_, String>(9)?, r.get::<_, u64>(10)?,r.get::<_,String>(11)?, r.get::<_, String>(12)?))
        }).optional()?;
        // Another harness parses the same bytes differently, so neither the
        // cached rows nor an append to them can be reused.
        if let Some(ref o) = old
            && o.12 == harness
            && o.1 == meta.dev()
            && o.2 == meta.ino()
            && o.3 == source_size
            && o.4 == mtime
            && (o.7 == source_size || o.11 == "deferred_tail")
        {
            return Ok(IndexedFile {
                changed: false,
                records: 0,
                coverage: if o.11 == "deferred_tail" {
                    FileCoverage::DeferredTail
                } else {
                    FileCoverage::Ready
                },
            });
        }
        let prefix_len = source_size.min(4096);
        let mut prefix = vec![0; prefix_len as usize];
        file.read_exact(&mut prefix)?;
        let prefix_hash = hash(&prefix);
        let append = old.as_ref().is_some_and(|o| {
            o.12 == harness
                && o.1 == meta.dev()
                && o.2 == meta.ino()
                && source_size >= o.3
                && o.6 <= prefix_len
                && hash(&prefix[..o.6 as usize]) == o.5
                && (source_size > o.3 || o.4 == mtime)
        });
        let (mut offset, mut line, mut state, mut errors) = if append {
            let o = old.as_ref().unwrap();
            (o.7, o.8, serde_json::from_str::<State>(&o.9)?, o.10)
        } else {
            let state = State {
                session_id: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                kind: "unknown".into(),
                kind_source: "none".into(),
                ..State::default()
            };
            (0, 0, state, 0)
        };
        let tx = self.db.transaction()?;
        if !append && let Some(ref o) = old {
            delete_file(&tx, o.0)?;
        }
        tx.execute("INSERT INTO files(path,harness,dev,inode,size,mtime,prefix_hash,prefix_len,state_json) VALUES (?,?,?,?,?,?,?,?,?) ON CONFLICT(path) DO UPDATE SET dev=excluded.dev,inode=excluded.inode,size=excluded.size,mtime=excluded.mtime,prefix_hash=excluded.prefix_hash,prefix_len=excluded.prefix_len", params![path_text, harness, meta.dev(), meta.ino(), source_size, mtime, prefix_hash, prefix_len, serde_json::to_string(&state)?])?;
        let file_id: i64 = tx.query_row(
            "SELECT id FROM files WHERE path=?",
            [path_text.as_ref()],
            |r| r.get(0),
        )?;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file.take(source_size - offset));
        let mut buffer = Vec::new();
        let mut records = 0;
        let mut strings = HashMap::new();
        let mut coverage = FileCoverage::Ready;
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                coverage = FileCoverage::BudgetExhausted;
                break;
            }
            let (length, complete, oversized) = read_record(&mut reader, &mut buffer)?;
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
                crate::normalize::record_for_index(&buffer, harness).ok()
            };
            if let Some(v) = parsed {
                let normalized = adapters::parse(harness, &v, &mut state);
                let signature = shape(&v);
                let raw_hash = hash(&buffer);
                tx.prepare_cached("INSERT INTO shapes VALUES (?,?,?,1) ON CONFLICT(file_id,signature,known) DO UPDATE SET n=n+1")?.execute(params![file_id,signature,normalized.known])?;
                for (ordinal, event) in normalized.events.into_iter().enumerate() {
                    let session = intern(&tx, &mut strings, &state.session_id)?;
                    let model = event
                        .model
                        .as_deref()
                        .or(state.model.as_deref())
                        .map(|v| intern(&tx, &mut strings, v))
                        .transpose()?;
                    let model_source = intern(
                        &tx,
                        &mut strings,
                        if event.model.is_some() {
                            "message.model"
                        } else {
                            &state.model_source
                        },
                    )?;
                    tx.prepare_cached("INSERT INTO locations(file_id,session_ref,native_id,line_no,byte_off,byte_len,raw_hash,ordinal,ts,model_ref,model_source_ref) VALUES (?,?,unhex(?),?,?,?,unhex(?),?,?,?,?)")?.execute(params![file_id,session,event.native_id,line,offset,length,&raw_hash,ordinal,event.ts,model,model_source])?;
                    let event_id = tx.last_insert_rowid();
                    if event.kind != "context" {
                        let role = intern(&tx, &mut strings, &event.role)?;
                        let role_source = intern(&tx, &mut strings, &event.role_source)?;
                        let kind = intern(&tx, &mut strings, &event.kind)?;
                        let kind_source = intern(&tx, &mut strings, &event.kind_source)?;
                        let tool = event
                            .tool
                            .as_deref()
                            .map(|v| intern(&tx, &mut strings, v))
                            .transpose()?;
                        let ok_source = intern(&tx, &mut strings, &event.ok_source)?;
                        tx.prepare_cached("INSERT INTO event_details(event_id,session_ref,role_ref,role_source_ref,kind_ref,kind_source_ref,text,tool_ref,call_id,ok,ok_source_ref,exit_code,text_truncated) VALUES (?,?,?,?,?,?,?,?,unhex(?),?,?,?,?)")?.execute(params![event_id,session,role,role_source,kind,kind_source,event.text,tool,event.call_id,event.ok,ok_source,event.exit_code,event.text_truncated])?;
                    }
                    for (site, command) in event.sites.into_iter().enumerate() {
                        tx.prepare_cached("INSERT INTO commands(event_id,site,program,argv_json,parsed) VALUES (?,?,?,?,?)")?.execute(params![event_id,site,command.program,serde_json::to_string(&command.argv)?,command.parsed])?;
                    }
                    for (source, reason) in event.denials {
                        tx.prepare_cached(
                            "INSERT INTO denials(event_id,source,reason_id) VALUES (?,?,?)",
                        )?
                        .execute(params![event_id, source, reason])?;
                    }
                    if let Some(text) = event.text {
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
        tx.execute("INSERT INTO sessions(file_id,harness,session_id,cwd,parent_id,kind,kind_source,instruction_hash) VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET session_id=excluded.session_id,cwd=excluded.cwd,parent_id=excluded.parent_id,kind=excluded.kind,kind_source=excluded.kind_source,instruction_hash=excluded.instruction_hash", params![file_id,harness,state.session_id,state.cwd,state.parent_id,state.kind,state.kind_source,state.instruction_hash])?;
        tx.execute("UPDATE files SET bytes_indexed=?,lines_indexed=?,state_json=?,parse_errors=?,index_status=?,first_ts=? WHERE id=?", params![offset,line,serde_json::to_string(&state)?,errors,coverage.as_str(),state.first_ts,file_id])?;
        tx.commit()?;
        Ok(IndexedFile {
            changed: true,
            records,
            coverage,
        })
    }
}

fn delete_file(tx: &Transaction<'_>, id: i64) -> Result<()> {
    tx.execute("INSERT INTO index_changes(first_event_id) SELECT min(id) FROM locations WHERE file_id=? HAVING count(*)>0", [id])?;
    tx.execute("DELETE FROM files WHERE id=?", [id])?;
    Ok(())
}

fn intern(tx: &Transaction<'_>, values: &mut HashMap<String, i64>, value: &str) -> Result<i64> {
    if let Some(id) = values.get(value) {
        return Ok(*id);
    }
    tx.prepare_cached("INSERT INTO strings(value) VALUES (?) ON CONFLICT(value) DO NOTHING")?
        .execute([value])?;
    let id = tx
        .prepare_cached("SELECT id FROM strings WHERE value=?")?
        .query_row([value], |r| r.get(0))?;
    values.insert(value.into(), id);
    Ok(id)
}

fn read_record(reader: &mut impl BufRead, buffer: &mut Vec<u8>) -> Result<(u64, bool, bool)> {
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
        .map(|k| redact(k))
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
