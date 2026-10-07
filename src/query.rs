use crate::{
    redaction::{redact, spaced_cjk},
    store::MAX_RECORD,
};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use clap::{Args, ValueEnum};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params_from_iter, types::Value};
use serde::Serialize;
use serde_json::Value as Json;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Codex,
    Pi,
}

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    Tool,
    System,
    Developer,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Unknown,
    Delegated,
}

#[derive(Args, Clone, Debug, Default, Serialize)]
pub struct Filters {
    #[arg(long, value_enum)]
    pub harness: Vec<Harness>,
    #[arg(long, value_enum)]
    pub role: Option<Role>,
    #[arg(long, value_enum)]
    pub kind: Option<Kind>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long)]
    pub cwd: Option<String>,
    /// Native session ID or session file path
    #[arg(long)]
    pub session: Option<String>,
}

impl Filters {
    pub fn narrowed(&self) -> bool {
        !self.harness.is_empty()
            || self.since.is_some()
            || self.until.is_some()
            || self.cwd.is_some()
            || self.session.is_some()
    }

    pub fn sql(&self, db: &Connection) -> Result<(String, Vec<Value>)> {
        let mut conditions = vec!["1=1".to_owned()];
        let mut args = Vec::new();
        if !self.harness.is_empty() {
            conditions.push(format!(
                "f.harness IN ({})",
                vec!["?"; self.harness.len()].join(",")
            ));
            args.extend(
                self.harness
                    .iter()
                    .map(|h| Value::Text(h.to_possible_value().unwrap().get_name().into())),
            );
        }
        for (field, value) in [
            (
                "e.role",
                self.role
                    .map(|r| r.to_possible_value().unwrap().get_name().to_owned()),
            ),
            (
                "s.kind",
                self.kind
                    .map(|k| k.to_possible_value().unwrap().get_name().to_owned()),
            ),
        ] {
            if let Some(value) = value {
                conditions.push(format!("{field}=?"));
                args.push(Value::Text(value));
            }
        }
        if let Some(session) = &self.session {
            let session_ref: Option<i64> = db
                .query_row("SELECT id FROM strings WHERE value=?", [session], |r| {
                    r.get(0)
                })
                .optional()?;
            let file_id: Option<i64> = db
                .query_row("SELECT id FROM files WHERE path=?", [session], |r| r.get(0))
                .optional()?;
            let mut selectors = Vec::new();
            for (column, id) in [("session_ref", session_ref), ("file_id", file_id)] {
                if let Some(id) = id {
                    selectors.push(format!("SELECT id FROM locations WHERE {column}=?"));
                    args.push(Value::Integer(id));
                }
            }
            conditions.push(if selectors.is_empty() {
                "0".into()
            } else {
                format!("e.id IN ({})", selectors.join(" UNION "))
            });
        }
        for (op, value) in [(">=", &self.since), ("<", &self.until)] {
            if let Some(value) = value {
                conditions.push(format!("e.ts{op}?"));
                args.push(Value::Text(date(value)?));
            }
        }
        if let Some(cwd) = &self.cwd {
            conditions.push("(s.cwd=? OR substr(s.cwd,1,length(?)+1)=?||'/')".into());
            for _ in 0..3 {
                args.push(Value::Text(cwd.trim_end_matches('/').to_owned()));
            }
        }
        Ok((conditions.join(" AND "), args))
    }
}

pub fn date(s: &str) -> Result<String> {
    let dt = if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        dt.with_timezone(&Utc)
    } else {
        NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .context("date must be RFC 3339 or YYYY-MM-DD")?
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
    };
    Ok(dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub harness: String,
    pub session_id: String,
    pub path: String,
    pub line_no: u64,
    pub ts: Option<String>,
    pub role: String,
    pub model: Option<String>,
    pub snippet: String,
    pub truncated: bool,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(skip)]
    pub byte_off: u64,
    #[serde(skip)]
    pub byte_len: u64,
    #[serde(skip)]
    pub event_id: i64,
}

#[derive(Debug, Default, Serialize)]
pub struct Coverage {
    pub incomplete: bool,
    pub continuation: Option<i64>,
    pub unavailable_ranges: usize,
    pub records: usize,
    pub bytes: u64,
}

const COLUMNS: &str = "f.harness,e.session_id,f.path,e.line_no,e.ts,e.role,e.model,coalesce(e.text,''),e.byte_off,e.byte_len,e.id,e.text_truncated";
const FROM: &str = "FROM events e JOIN files f ON f.id=e.file_id JOIN sessions s ON s.file_id=f.id";

fn hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
    let harness: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let path: String = row.get(2)?;
    let line_no: u64 = row.get(3)?;
    let reference = format!("{path}:{line_no}");
    Ok(Hit {
        harness,
        session_id,
        path,
        line_no,
        ts: row.get(4)?,
        role: row.get(5)?,
        model: row.get(6)?,
        snippet: redact(&row.get::<_, String>(7)?),
        truncated: row.get(11)?,
        reference,
        byte_off: row.get(8)?,
        byte_len: row.get(9)?,
        event_id: row.get(10)?,
    })
}

pub fn fts_query(query: &str) -> Result<String> {
    anyhow::ensure!(!query.trim().is_empty(), "search query is empty");
    let tokens = Regex::new(r#""([^"]+)"|(\S+)"#).unwrap();
    Ok(tokens
        .captures_iter(query)
        .map(|c| {
            let term = c.get(1).or_else(|| c.get(2)).unwrap().as_str();
            format!("\"{}\"", spaced_cjk(term).replace('"', "\"\""))
        })
        .collect::<Vec<_>>()
        .join(" AND "))
}

#[derive(Debug, Serialize)]
pub struct Session {
    pub harness: String,
    pub session_id: String,
    pub path: String,
    pub ts: Option<String>,
    pub matched_hits: usize,
    pub hits: Vec<Hit>,
}

pub fn search(
    db: &Connection,
    query: &str,
    filters: &Filters,
    limit: usize,
    offset: usize,
) -> Result<Vec<Session>> {
    let (clause, mut args) = filters.sql(db)?;
    args.push(Value::Text(fts_query(query)?));
    args.push(Value::Integer(limit as i64));
    args.push(Value::Integer(offset as i64));
    // Materialize FTS scores before grouping: bm25 requires the original FTS cursor.
    let sql = format!(
        "WITH matches AS MATERIALIZED (
          SELECT f.harness,e.session_id,e.id,e.ts,bm25(fts) AS score {FROM}
          JOIN fts ON fts.rowid=e.id WHERE {clause} AND fts MATCH ?
        ), heads AS MATERIALIZED (
          SELECT harness,session_id,min(score) AS score,max(ts) AS ts,count(*) AS n
          FROM matches GROUP BY harness,session_id
          ORDER BY score,ts DESC,harness,session_id LIMIT ? OFFSET ?
        ), ranked AS (
          SELECT m.id,row_number() OVER (
            PARTITION BY m.harness,m.session_id ORDER BY m.score,m.ts DESC,m.id DESC
          ) AS n FROM matches m JOIN heads h USING(harness,session_id)
        )
        SELECT {COLUMNS},h.ts,h.n {FROM}
        JOIN ranked r ON r.id=e.id JOIN heads h ON h.harness=f.harness AND h.session_id=e.session_id
        WHERE r.n<=3 ORDER BY h.score,h.ts DESC,h.harness,h.session_id,r.n"
    );
    let mut stmt = db.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), |r| {
        Ok((
            hit(r)?,
            r.get::<_, Option<String>>(12)?,
            r.get::<_, usize>(13)?,
        ))
    })?;
    let mut sessions: Vec<Session> = Vec::new();
    for row in rows {
        let (hit, ts, matched_hits) = row?;
        if !sessions
            .last()
            .is_some_and(|s| s.harness == hit.harness && s.session_id == hit.session_id)
        {
            sessions.push(Session {
                harness: hit.harness.clone(),
                session_id: hit.session_id.clone(),
                path: hit.path.clone(),
                ts,
                matched_hits,
                hits: Vec::new(),
            });
        }
        sessions.last_mut().unwrap().hits.push(hit);
    }
    Ok(sessions)
}

fn read_range(db: &Connection, item: &Hit) -> Result<Vec<u8>> {
    anyhow::ensure!(
        item.byte_len <= MAX_RECORD as u64,
        "source record exceeds display limit"
    );
    let expected: (u64, u64, String) = db.query_row(
        "SELECT f.dev,f.inode,e.raw_hash FROM files f JOIN events e ON e.file_id=f.id WHERE e.id=?",
        [item.event_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut file = File::open(&item.path)?;
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    anyhow::ensure!(
        meta.dev() == expected.0
            && meta.ino() == expected.1
            && meta.len() >= item.byte_off + item.byte_len,
        "source replaced or truncated; run sessidx index"
    );
    file.seek(SeekFrom::Start(item.byte_off))?;
    let mut bytes = vec![0; item.byte_len as usize];
    file.read_exact(&mut bytes)?;
    anyhow::ensure!(
        crate::normalize::hash(&bytes) == expected.2,
        "source range changed; run sessidx index"
    );
    Ok(bytes)
}

fn display_range(bytes: &[u8]) -> Result<String> {
    let mut value: Json =
        serde_json::from_slice(bytes).context("source range changed; run sessidx index")?;
    crate::redaction::redact_value(&mut value);
    Ok(serde_json::to_string(&value)?)
}

pub fn scan(
    db: &Connection,
    pattern: &str,
    filters: &Filters,
    limit: usize,
    after: i64,
    budget: Duration,
) -> Result<(Vec<Hit>, Coverage)> {
    anyhow::ensure!(
        filters.narrowed(),
        "grep requires a narrowing filter: add --since, --until, --cwd, --session, or --harness"
    );
    let deadline = Instant::now() + budget;
    db.progress_handler(1000, Some(move || Instant::now() >= deadline));
    let result: Result<(Vec<Hit>, Coverage)> = (|| {
        let pattern = Regex::new(pattern).context("invalid grep regex")?;
        let (clause, mut args) = filters.sql(db)?;
        args.push(Value::Integer(after));
        let sql = format!(
            "SELECT {COLUMNS} {FROM} WHERE {clause} AND e.id>? AND e.ordinal=(SELECT min(e2.ordinal) FROM events e2 WHERE e2.file_id=e.file_id AND e2.line_no=e.line_no AND e2.role=e.role) ORDER BY e.id"
        );
        let mut stmt = db.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(args))?;
        let mut hits = Vec::new();
        let mut coverage = Coverage::default();
        let mut cursor = after;
        loop {
            if Instant::now() >= deadline || hits.len() >= limit {
                coverage.incomplete = true;
                coverage.continuation = Some(cursor);
                break;
            }
            let row = match rows.next() {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::OperationInterrupted
                        && Instant::now() >= deadline =>
                {
                    coverage.incomplete = true;
                    coverage.continuation = Some(cursor);
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            let mut item = hit(row)?;
            cursor = item.event_id;
            coverage.records += 1;
            match read_range(db, &item) {
                Ok(bytes) => {
                    coverage.bytes += bytes.len() as u64;
                    if pattern.is_match(&String::from_utf8_lossy(&bytes)) {
                        item.snippet = display_range(&bytes)?;
                        item.truncated = false;
                        hits.push(item);
                    }
                }
                Err(_) => {
                    coverage.unavailable_ranges += 1;
                    coverage.incomplete = true;
                }
            }
        }
        Ok((hits, coverage))
    })();
    db.progress_handler(0, None::<fn() -> bool>);
    match result {
        Err(e) if Instant::now() >= deadline && e.downcast_ref::<rusqlite::Error>().is_some_and(|e| matches!(e, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::OperationInterrupted)) => Ok((Vec::new(), Coverage { incomplete: true, continuation: Some(after), ..Coverage::default() })),
        other => other,
    }
}

pub fn show(
    db: &Connection,
    target: &str,
    around: usize,
    limit: usize,
    after: i64,
) -> Result<(Vec<Hit>, Coverage)> {
    let mut args = Vec::new();
    let clause = if let Some((path, line)) = target
        .rsplit_once(':')
        .filter(|(_, l)| l.parse::<u64>().is_ok())
    {
        let line = line.parse::<u64>()?;
        args.push(Value::Text(path.into()));
        args.push(Value::Integer(
            line.saturating_sub(around as u64).max(1) as i64
        ));
        args.push(Value::Integer(line.saturating_add(around as u64) as i64));
        "f.path=? AND e.line_no BETWEEN ? AND ?"
    } else {
        args.push(Value::Text(
            target
                .strip_prefix("codex://threads/")
                .unwrap_or(target)
                .into(),
        ));
        "e.session_id=?"
    };
    args.push(Value::Integer(after));
    args.push(Value::Integer((limit + 1) as i64));
    let sql = format!("SELECT {COLUMNS} {FROM} WHERE {clause} AND e.id>? ORDER BY e.id LIMIT ?");
    let mut hits = db
        .prepare(&sql)?
        .query_map(params_from_iter(args), hit)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = hits.len() > limit;
    hits.truncate(limit);
    let mut coverage = Coverage {
        incomplete: more,
        continuation: if more {
            hits.last().map(|h| h.event_id)
        } else {
            None
        },
        ..Coverage::default()
    };
    coverage.records = hits.len();
    for item in &mut hits {
        match read_range(db, item).and_then(|bytes| {
            coverage.bytes += bytes.len() as u64;
            display_range(&bytes)
        }) {
            Ok(raw) => {
                item.snippet = raw;
                item.truncated = false;
            }
            Err(_) => {
                item.snippet = "[source range unavailable; run sessidx index]".into();
                coverage.unavailable_ranges += 1;
                coverage.incomplete = true;
            }
        }
    }
    Ok((hits, coverage))
}
