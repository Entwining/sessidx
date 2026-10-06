use crate::{
    adapters,
    discovery::{self, Root},
    model::State,
    normalize::hash,
    redaction::{redact, spaced_cjk},
};
use anyhow::{Context, Result};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const MAX_RECORD: usize = 16 * 1024 * 1024;

#[derive(Default, Debug, Serialize)]
pub struct Refresh {
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

pub struct WriterLock(File);
impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(50))?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-8192;")?;
        db.execute_batch(include_str!("schema.sql"))?;
        Ok(Self {
            db,
            path: path.into(),
        })
    }

    pub fn lock(&self) -> Result<Option<WriterLock>> {
        let path = self.path.with_extension("lock");
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        match f.try_lock_exclusive() {
            Ok(()) => Ok(Some(WriterLock(f))),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.into()),
        }
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
        let (files, missing_roots) = discovery::files(roots)?;
        let mut report = Refresh {
            missing_roots,
            ..Refresh::default()
        };
        report.stale = !report.missing_roots.is_empty();
        if full {
            self.db.execute("DELETE FROM files", [])?;
        }
        let existing: Vec<(i64, String)> = self
            .db
            .prepare("SELECT id,path FROM files")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, path) in existing {
            if !files
                .iter()
                .any(|(_, p)| p.as_os_str() == Path::new(&path).as_os_str())
            {
                self.db.execute("DELETE FROM files WHERE id=?", [id])?;
            }
        }
        for (harness, path) in files {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                report.stale = true;
                report.continuation = Some(redact(&path.to_string_lossy()));
                break;
            }
            let result = self.index_file(&harness, &path, deadline)?;
            report.files_changed += usize::from(result.0);
            report.records += result.1;
            if result.2 {
                report.stale = true;
                report.continuation = Some(redact(&path.to_string_lossy()));
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    break;
                }
            }
        }
        Ok(report)
    }

    fn index_file(
        &mut self,
        harness: &str,
        path: &Path,
        deadline: Option<Instant>,
    ) -> Result<(bool, usize, bool)> {
        let mut file = File::open(path).context("cannot open session file")?;
        let meta = file.metadata()?;
        let mtime = format!("{}:{}", meta.mtime(), meta.mtime_nsec());
        let path_text = path.to_string_lossy();
        let old = self.db.query_row("SELECT id,dev,inode,size,mtime,prefix_hash,prefix_len,bytes_indexed,lines_indexed,state_json,parse_errors FROM files WHERE path=?", [path_text.as_ref()], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, u64>(1)?, r.get::<_, u64>(2)?, r.get::<_, u64>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?, r.get::<_, u64>(6)?, r.get::<_, u64>(7)?, r.get::<_, u64>(8)?, r.get::<_, String>(9)?, r.get::<_, u64>(10)?))
        }).optional()?;
        if let Some(ref o) = old {
            if o.1 == meta.dev()
                && o.2 == meta.ino()
                && o.3 == meta.len()
                && o.4 == mtime
                && o.7 == meta.len()
            {
                return Ok((false, 0, false));
            }
        }
        let prefix_len = meta.len().min(4096);
        let mut prefix = vec![0; prefix_len as usize];
        file.read_exact(&mut prefix)?;
        let prefix_hash = hash(&prefix);
        let append = old.as_ref().is_some_and(|o| {
            o.1 == meta.dev()
                && o.2 == meta.ino()
                && meta.len() >= o.3
                && o.6 <= prefix_len
                && hash(&prefix[..o.6 as usize]) == o.5
                && (meta.len() > o.3 || o.4 == mtime)
        });
        let (mut offset, mut line, mut state, mut errors) = if append {
            let o = old.as_ref().unwrap();
            (o.7, o.8, serde_json::from_str::<State>(&o.9)?, o.10)
        } else {
            let mut state = State::default();
            state.session_id = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            state.kind = "unknown".into();
            state.kind_source = "none".into();
            (0, 0, state, 0)
        };
        let tx = self.db.transaction()?;
        if !append {
            if let Some(ref o) = old {
                tx.execute("DELETE FROM files WHERE id=?", [o.0])?;
            }
        }
        tx.execute("INSERT INTO files(path,harness,dev,inode,size,mtime,prefix_hash,prefix_len,state_json) VALUES (?,?,?,?,?,?,?,?,?) ON CONFLICT(path) DO UPDATE SET dev=excluded.dev,inode=excluded.inode,size=excluded.size,mtime=excluded.mtime,prefix_hash=excluded.prefix_hash,prefix_len=excluded.prefix_len", params![path_text, harness, meta.dev(), meta.ino(), meta.len(), mtime, prefix_hash, prefix_len, serde_json::to_string(&state)?])?;
        let file_id: i64 = tx.query_row(
            "SELECT id FROM files WHERE path=?",
            [path_text.as_ref()],
            |r| r.get(0),
        )?;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file.take(meta.len() - offset));
        let mut buffer = Vec::new();
        let mut records = 0;
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            let (length, complete, oversized) = read_record(&mut reader, &mut buffer)?;
            if length == 0 || !complete {
                break;
            }
            line += 1;
            records += 1;
            let parsed = if oversized {
                None
            } else {
                serde_json::from_slice::<serde_json::Value>(&buffer).ok()
            };
            if let Some(v) = parsed {
                let normalized = adapters::parse(harness, &v, &mut state);
                let signature = shape(&v);
                tx.execute("INSERT INTO shapes VALUES (?,?,?,1) ON CONFLICT(file_id,signature,known) DO UPDATE SET n=n+1", params![file_id,signature,normalized.known])?;
                for (ordinal, event) in normalized.events.into_iter().enumerate() {
                    tx.execute("INSERT INTO events(file_id,session_id,native_id,line_no,byte_off,byte_len,ordinal,ts,role,role_source,kind,kind_source,model,model_source,text,tool,call_id,ok,ok_source,exit_code) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)", params![file_id,state.session_id,event.native_id,line,offset,length,ordinal,event.ts,event.role,event.role_source,event.kind,"adapter",state.model,state.model_source,event.text,event.tool,event.call_id,event.ok,event.ok_source,event.exit_code])?;
                    let event_id = tx.last_insert_rowid();
                    if let Some(text) = event.text {
                        tx.execute(
                            "INSERT INTO fts(rowid,text) VALUES (?,?)",
                            params![event_id, spaced_cjk(&text)],
                        )?;
                    }
                }
            } else {
                errors += 1;
                tx.execute("INSERT INTO shapes VALUES (?,?,0,1) ON CONFLICT(file_id,signature,known) DO UPDATE SET n=n+1", params![file_id,if oversized { "oversized_record" } else { "invalid_json" }])?;
            }
            offset += length;
        }
        tx.execute("INSERT INTO sessions(file_id,harness,session_id,cwd,parent_id,kind,kind_source,instruction_hash) VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET session_id=excluded.session_id,cwd=excluded.cwd,parent_id=excluded.parent_id,kind=excluded.kind,kind_source=excluded.kind_source,instruction_hash=excluded.instruction_hash", params![file_id,harness,state.session_id,state.cwd,state.parent_id,state.kind,state.kind_source,state.instruction_hash])?;
        tx.execute("UPDATE files SET bytes_indexed=?,lines_indexed=?,state_json=?,parse_errors=?,complete=? WHERE id=?", params![offset,line,serde_json::to_string(&state)?,errors,offset==meta.len(),file_id])?;
        tx.commit()?;
        Ok((true, records, offset < meta.len()))
    }
}

pub fn read_record(reader: &mut impl BufRead, buffer: &mut Vec<u8>) -> Result<(u64, bool, bool)> {
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
    let typ = v.get("type").and_then(|s| s.as_str()).unwrap_or("unknown");
    let subtype = v
        .pointer("/payload/type")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    redact(&format!("{typ}/{subtype}:{}", keys.join(",")))
}
