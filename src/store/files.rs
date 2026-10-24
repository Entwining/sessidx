use super::{
    MAX_RECORD, PARSER_VERSION, Store,
    derived::{delete_file, mark, settle},
    rollups::add_counts,
};
use crate::{
    adapters,
    model::{Agent, Event, EventKind, State, name},
    normalize::{Digest, hash},
    redaction::{Label, spaced_cjk},
    source,
};
use anyhow::{Context, Result};
use rusqlite::{
    Connection, OptionalExtension, ToSql, Transaction, params,
    types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef},
};
use std::{
    fs,
    io::{self, BufRead, Read},
    os::unix::fs::MetadataExt,
    path::Path,
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum FileCoverage {
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
impl ToSql for Label {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok((&**self).into())
    }
}
impl ToSql for Digest {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok((&**self).into())
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
    parser: i64,
}
impl FileRow {
    fn read(db: &Connection, path: &str) -> Result<Option<Self>> {
        Ok(db
            .prepare_cached("SELECT * FROM files WHERE path=?")?
            .query_row([path], |r| {
                Ok(Self {
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
                    parser: r.get("parser")?,
                })
            })
            .optional()?)
    }

    /// The coverage of a file these rows still describe, or None when it must
    /// be read.
    fn unchanged(&self, agent: &str, path: &Path) -> Option<FileCoverage> {
        // Another agent parses the same bytes differently, so neither the
        // cached rows nor an append to them can be reused. Only a refresh that
        // ran out of budget leaves an unchanged file's records unread. Most
        // files a refresh visits are unchanged, so a stat decides before opening.
        if self.agent == agent
            && self.parser == PARSER_VERSION
            && self.index_status != FileCoverage::BudgetExhausted
            && let Ok(meta) = source::metadata(path)
            && self.dev == meta.dev()
            && self.inode == meta.ino()
            && self.size == meta.len()
            && self.mtime == change_stamp(&meta)
        {
            Some(if self.index_status == FileCoverage::DeferredTail {
                FileCoverage::DeferredTail
            } else {
                FileCoverage::Ready
            })
        } else {
            None
        }
    }

    /// Whether the file is the one these rows read, by its device, inode, and
    /// stored prefix, and only grew or did not change, so indexing resumes
    /// where they end: after an append, or after a refresh that ran out of
    /// budget in the file.
    fn appended(&self, agent: &str, meta: &fs::Metadata, prefix: &[u8], mtime: &str) -> bool {
        self.agent == agent
            && self.parser == PARSER_VERSION
            && self.dev == meta.dev()
            && self.inode == meta.ino()
            && meta.len() >= self.size
            && self.prefix_len <= prefix.len() as u64
            && *hash(&prefix[..self.prefix_len as usize]) == self.prefix_hash
            && (meta.len() > self.size || self.mtime == mtime)
    }

    fn cursor(&self) -> Result<Cursor> {
        Ok(Cursor {
            offset: self.bytes_indexed,
            line: self.lines_indexed,
            state: serde_json::from_str(&self.state_json)?,
            errors: self.parse_errors,
        })
    }
}
/// Where indexing stands in a file, as its `files` row keeps it between
/// transactions: only complete records advance it.
struct Cursor {
    offset: u64,
    line: u64,
    state: State,
    errors: u64,
}
impl Cursor {
    fn start(path: &Path) -> Self {
        Self {
            offset: 0,
            line: 0,
            state: State {
                session_id: Label::new(&path.file_stem().unwrap_or_default().to_string_lossy()),
                session_kind_source: "none".into(),
                ..State::default()
            },
            errors: 0,
        }
    }
}
/// What one transaction read from a file.
struct Batch {
    records: usize,
    coverage: FileCoverage,
    first_event: Option<i64>,
    bounds: Option<(String, String)>,
}
pub(super) struct IndexedFile {
    pub(super) changed: bool,
    pub(super) records: usize,
    pub(super) coverage: FileCoverage,
}

impl Store {
    pub(super) fn index_file(
        &mut self,
        agent: Agent,
        path: &Path,
        deadline: Option<Instant>,
    ) -> Result<IndexedFile> {
        let path_text = path.to_string_lossy();
        let agent_name = name(agent);
        let old = FileRow::read(&self.db, &path_text)?;
        if let Some(coverage) = old.as_ref().and_then(|o| o.unchanged(&agent_name, path)) {
            return Ok(IndexedFile {
                changed: false,
                records: 0,
                coverage,
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
        let appended = old
            .as_ref()
            .filter(|o| o.appended(&agent_name, &meta, &prefix, &mtime));
        let mut cursor = match appended {
            Some(o) => o.cursor()?,
            None => Cursor::start(path),
        };
        let tx = self.db.transaction()?;
        if appended.is_none()
            && let Some(ref o) = old
        {
            delete_file(&tx, o.id)?;
        }
        tx.execute("INSERT INTO files(path,agent,dev,inode,size,mtime,prefix_hash,prefix_len,state_json,parser) VALUES (?,?,?,?,?,?,?,?,?,?) ON CONFLICT(path) DO UPDATE SET dev=excluded.dev,inode=excluded.inode,size=excluded.size,mtime=excluded.mtime,prefix_hash=excluded.prefix_hash,prefix_len=excluded.prefix_len", params![path_text, agent_name, meta.dev(), meta.ino(), source_size, mtime, prefix_hash, prefix_len, serde_json::to_string(&cursor.state)?, PARSER_VERSION])?;
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
        let batch = read_records(&tx, file_id, agent, source, &mut cursor, deadline)?;
        let state = &cursor.state;
        let identity_changed = before
            .is_some_and(|(id, started)| id != *state.session_id || started != state.first_ts);
        if identity_changed {
            mark(&tx, "r.file_id=?", file_id)?;
        }
        let (min_ts, max_ts) = batch.bounds.unzip();
        // SQLite's two-argument min and max are NULL when either side is.
        tx.execute("INSERT INTO sessions(file_id,agent,session_id,parent_id,session_kind,session_kind_source,path,cwd,started_at,instruction_hash,min_ts,max_ts) VALUES (?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET session_id=excluded.session_id,parent_id=excluded.parent_id,session_kind=excluded.session_kind,session_kind_source=excluded.session_kind_source,cwd=excluded.cwd,started_at=excluded.started_at,instruction_hash=excluded.instruction_hash,min_ts=coalesce(min(min_ts,excluded.min_ts),min_ts,excluded.min_ts),max_ts=coalesce(max(max_ts,excluded.max_ts),max_ts,excluded.max_ts)", params![file_id,agent_name,state.session_id,state.parent_id,name(state.session_kind),state.session_kind_source,path_text,state.cwd,state.first_ts,state.instruction_hash,min_ts,max_ts])?;
        if identity_changed {
            mark(&tx, "r.file_id=?", file_id)?;
        } else if let Some(first) = batch.first_event {
            // Event IDs only grow, so this transaction's rows are those from
            // its first insert on.
            mark(&tx, "r.id>=?", first)?;
        }
        tx.execute("UPDATE files SET bytes_indexed=?,lines_indexed=?,state_json=?,parse_errors=?,index_status=? WHERE id=?", params![cursor.offset,cursor.line,serde_json::to_string(state)?,cursor.errors,batch.coverage,file_id])?;
        tx.execute(
            "UPDATE index_identity SET written=written+?",
            [batch.records as i64],
        )?;
        settle(&tx, batch.first_event)?;
        // This transaction's rows count once, with the values `settle` gave them.
        if let Some(first) = batch.first_event {
            add_counts(&tx, 1, "e.id>=?", [first])?;
        }
        tx.commit()?;
        Ok(IndexedFile {
            changed: true,
            records: batch.records,
            coverage: batch.coverage,
        })
    }
}

/// Reads `source`'s complete records from `cursor` until its end or the
/// deadline, writing each record's events and advancing `cursor` past it.
fn read_records(
    tx: &Transaction,
    file_id: i64,
    agent: Agent,
    source: source::Source,
    cursor: &mut Cursor,
    deadline: Option<Instant>,
) -> Result<Batch> {
    let mut batch = Batch {
        records: 0,
        coverage: FileCoverage::Ready,
        first_event: None,
        bounds: None,
    };
    let compressed = source.compressed;
    let mut reader = source.records_from(cursor.offset)?;
    let mut buffer = Vec::new();
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            batch.coverage = FileCoverage::BudgetExhausted;
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
                cursor.errors += 1;
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if length == 0 || !complete {
            if length > 0 {
                batch.coverage = FileCoverage::DeferredTail;
            }
            break;
        }
        cursor.line += 1;
        batch.records += 1;
        let parsed = if oversized {
            None
        } else {
            crate::normalize::record_for_index(&buffer, agent).ok()
        };
        if let Some(v) = parsed {
            let events = adapters::parse(agent, &v, &mut cursor.state);
            let state = &cursor.state;
            let raw_hash = hash(&buffer);
            let mut line_roles: Vec<Label> = Vec::new();
            for (ordinal, mut event) in events.into_iter().enumerate() {
                let line_role_first = !line_roles.contains(&event.role);
                if line_role_first {
                    line_roles.push(event.role.clone());
                }
                if let Some(ts) = &event.ts {
                    batch.bounds = Some(match batch.bounds {
                        Some((min, max)) => (min.min(ts.clone()), max.max(ts.clone())),
                        None => (ts.clone(), ts.clone()),
                    });
                }
                let event_model = event.model.take();
                let model = event_model.as_deref().or(state.model.as_deref());
                let model_source = if event_model.is_some() {
                    "message.model"
                } else if state.model.is_some() {
                    &state.model_source
                } else {
                    "none"
                };
                // Derived columns start as if the event had no copy and no
                // result; `settle` corrects every key these rows touch.
                let call_ok_source = (event.kind == EventKind::ToolCall).then_some("none");
                tx.prepare_cached("INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,native_id,ts,role,role_source,kind,kind_source,model,model_source,tool,call_id,ok,ok_source,exit_code,line_role_first,canonical,call_ok_source) VALUES (?,?,?,?,?,unhex(?),unhex(?),?,?,?,?,?,?,?,?,unhex(?),?,?,?,?,?,?)")?.execute(params![file_id,cursor.line,cursor.offset,length,ordinal,&raw_hash,event.native_id,event.ts,event.role,event.role_source,event.kind.as_str(),event.kind_source,model,model_source,event.tool,event.call_id,event.ok,event.ok_source,event.exit_code,line_role_first,true,call_ok_source])?;
                let event_id = tx.last_insert_rowid();
                batch.first_event.get_or_insert(event_id);
                insert_keyed_rows(tx, file_id, event_id, model, state, event)?;
            }
        } else {
            cursor.errors += 1;
        }
        cursor.offset += length;
    }
    Ok(batch)
}

/// Writes the rows keyed by an event: its request's usage, a request error, a
/// compaction, command sites, denials, and its text with the text's search entry.
/// `model` is the one its event row stored; `event.model` was taken for it.
fn insert_keyed_rows(
    tx: &Transaction,
    file_id: i64,
    event_id: i64,
    model: Option<&str>,
    state: &State,
    event: Event,
) -> Result<()> {
    if let Some(request) = event.request {
        let u = &request.usage;
        let route = request.route.as_ref();
        // A usage row starts as its request's only record; `settle` corrects
        // `selected`.
        tx.prepare_cached("INSERT INTO usage_rows(event_id,file_id,ts,model,route,route_source,finished,stop_reason,selected,input,output,cache_read,cache_write,cache_write_5m,cache_write_1h,reasoning) VALUES (?,?,?,?,?,?,?,?,1,?,?,?,?,?,?,?)")?.execute(params![event_id,file_id,event.ts,model,route.map(|r| &r.value),route.map(|r| r.source),request.finished,request.stop_reason,u.input,u.output,u.cache_read,u.cache_write,u.cache_write_5m,u.cache_write_1h,u.reasoning])?;
        for (pass, p) in request.passes.iter().enumerate() {
            let u = &p.usage;
            tx.prepare_cached("INSERT INTO usage_passes(event_id,pass,kind,model,input,output,cache_read,cache_write,cache_write_5m,cache_write_1h) VALUES (?,?,?,?,?,?,?,?,?,?)")?.execute(params![event_id,pass,p.kind,p.model,u.input,u.output,u.cache_read,u.cache_write,u.cache_write_5m,u.cache_write_1h])?;
        }
    }
    if let Some(error) = event.error {
        let route = error.route.as_ref();
        tx.prepare_cached("INSERT INTO error_rows(event_id,model,source,status,code,retry_attempt,route,route_source) VALUES (?,?,?,?,?,?,?,?)")?.execute(params![event_id,state.model,error.source,error.status,error.code,error.retry_attempt,route.map(|r| &r.value),route.map(|r| r.source)])?;
    }
    if let Some(c) = event.compaction {
        let s = c.summary.as_ref();
        tx.prepare_cached("INSERT INTO compaction_rows(event_id,trigger,pre_tokens,post_tokens,summary_input,summary_output,summary_cache_read,summary_cache_write,request_native_id) VALUES (?,?,?,?,?,?,?,?,unhex(?))")?.execute(params![event_id,c.trigger,c.pre_tokens,c.post_tokens,s.map(|s| s.input),s.map(|s| s.output),s.and_then(|s| s.cache_read),s.and_then(|s| s.cache_write),c.request_id])?;
    }
    for (site, command) in event.sites.into_iter().enumerate() {
        tx.prepare_cached(
            "INSERT INTO commands(event_id,site,program,argv_json,parsed) VALUES (?,?,?,?,?)",
        )?
        .execute(params![
            event_id,
            site,
            command.program,
            serde_json::to_string(&command.argv)?,
            command.parsed
        ])?;
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
        tx.prepare_cached("INSERT INTO event_texts(event_id,text,truncated) VALUES (?,?,?)")?
            .execute(params![event_id, &*text, event.text_truncated])?;
        tx.prepare_cached("INSERT INTO fts(rowid,text) VALUES (?,?)")?
            .execute(params![event_id, spaced_cjk(&text)])?;
    }
    Ok(())
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
