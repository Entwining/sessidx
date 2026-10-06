use crate::{
    redaction::{redact, spaced_cjk},
    store::MAX_RECORD,
};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use clap::Args;
use regex::Regex;
use rusqlite::{Connection, params_from_iter, types::Value};
use serde::Serialize;
use serde_json::Value as Json;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    time::{Duration, Instant},
};

#[derive(Args, Clone, Debug, Default)]
pub struct Filters {
    #[arg(long)]
    pub harness: Option<String>,
    #[arg(long)]
    pub role: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long)]
    pub cwd: Option<String>,
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long = "file")]
    pub files: Vec<String>,
}

impl Filters {
    pub fn narrowed(&self) -> bool {
        self.harness.is_some()
            || self.since.is_some()
            || self.until.is_some()
            || self.cwd.is_some()
            || self.session.is_some()
            || !self.files.is_empty()
    }

    pub fn sql(&self) -> Result<(String, Vec<Value>)> {
        let mut conditions = vec!["1=1".to_owned()];
        let mut args = Vec::new();
        for (field, value) in [
            ("f.harness", &self.harness),
            ("e.role", &self.role),
            ("e.session_id", &self.session),
        ] {
            if let Some(value) = value {
                conditions.push(format!("{field}=?"));
                args.push(Value::Text(value.clone()));
            }
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
        if !self.files.is_empty() {
            conditions.push(format!(
                "f.path IN ({})",
                vec!["?"; self.files.len()].join(",")
            ));
            args.extend(self.files.iter().cloned().map(Value::Text));
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
}

const SELECT: &str = "SELECT f.harness,e.session_id,f.path,e.line_no,e.ts,e.role,e.model,coalesce(e.text,''),e.byte_off,e.byte_len,e.id FROM events e JOIN files f ON f.id=e.file_id JOIN sessions s ON s.file_id=f.id";

fn hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
    let harness: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let path: String = row.get(2)?;
    let line_no: u64 = row.get(3)?;
    let reference = if harness == "codex" {
        format!("codex://threads/{session_id}")
    } else {
        format!("{path}:{line_no}")
    };
    Ok(Hit {
        harness,
        session_id,
        path,
        line_no,
        ts: row.get(4)?,
        role: row.get(5)?,
        model: row.get(6)?,
        snippet: redact(&row.get::<_, String>(7)?),
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

pub fn search(
    db: &Connection,
    query: &str,
    filters: &Filters,
    limit: usize,
    offset: usize,
) -> Result<Vec<Hit>> {
    let (clause, mut args) = filters.sql()?;
    args.push(Value::Text(fts_query(query)?));
    args.push(Value::Integer(limit as i64));
    args.push(Value::Integer(offset as i64));
    let sql = format!(
        "{SELECT} JOIN fts ON fts.rowid=e.id WHERE {clause} AND fts MATCH ? ORDER BY bm25(fts),e.ts DESC,e.id DESC LIMIT ? OFFSET ?"
    );
    Ok(db
        .prepare(&sql)?
        .query_map(params_from_iter(args), hit)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn raw_range(db: &Connection, item: &Hit) -> Result<String> {
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
    let mut value: Json =
        serde_json::from_slice(&bytes).context("source range changed; run sessidx index")?;
    redact_value(&mut value);
    Ok(serde_json::to_string(&value)?)
}

pub fn redact_value(v: &mut Json) {
    match v {
        Json::String(s) => *s = redact(s),
        Json::Array(a) => a.iter_mut().for_each(redact_value),
        Json::Object(m) => {
            let old = std::mem::take(m);
            for (key, mut value) in old {
                redact_value(&mut value);
                m.insert(redact(&key), value);
            }
        }
        _ => {}
    }
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
        "--scan requires a narrowing filter: add --since, --until, --cwd, --session, --harness, or --file"
    );
    let pattern = Regex::new(pattern).context("invalid scan regex")?;
    let (clause, mut args) = filters.sql()?;
    args.push(Value::Integer(after));
    let sql = format!(
        "{SELECT} WHERE {clause} AND e.id>? AND e.ordinal=(SELECT min(e2.ordinal) FROM events e2 WHERE e2.file_id=e.file_id AND e2.line_no=e.line_no AND e2.role=e.role) ORDER BY e.id"
    );
    let mut stmt = db.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), hit)?;
    let deadline = Instant::now() + budget;
    let mut hits = Vec::new();
    let mut coverage = Coverage::default();
    let mut cursor = after;
    for row in rows {
        if Instant::now() >= deadline || hits.len() >= limit {
            coverage.incomplete = true;
            coverage.continuation = Some(cursor);
            break;
        }
        let mut item = row?;
        cursor = item.event_id;
        match raw_range(db, &item) {
            Ok(raw) => {
                if pattern.is_match(&raw) {
                    item.snippet = raw;
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
    let sql = format!("{SELECT} WHERE {clause} AND e.id>? ORDER BY e.id LIMIT ?");
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
    for item in &mut hits {
        match raw_range(db, item) {
            Ok(raw) => item.snippet = raw,
            Err(_) => {
                item.snippet = "[source range unavailable; run sessidx index]".into();
                coverage.unavailable_ranges += 1;
                coverage.incomplete = true;
            }
        }
    }
    Ok((hits, coverage))
}

pub fn emit_hits(hits: &[Hit], json: bool) -> Result<()> {
    for item in hits {
        if json {
            println!("{}", serde_json::to_string(item)?);
        } else {
            println!(
                "{}:{} [{} {} {}] {}\n{}",
                redact(&item.path),
                item.line_no,
                item.harness,
                item.role,
                item.model.as_deref().unwrap_or("unknown"),
                redact(&item.snippet.chars().take(1200).collect::<String>()),
                redact(&item.reference)
            );
        }
    }
    Ok(())
}
