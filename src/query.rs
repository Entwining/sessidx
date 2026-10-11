use crate::{
    model::{Agent, SessionKind, name},
    redaction::{redact, spaced_cjk},
    store::MAX_RECORD,
};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, SecondsFormat, Utc};
use clap::{Args, ValueEnum};
use regex::Regex;
use rusqlite::{Connection, params_from_iter, types::Value};
use serde::Serialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::MetadataExt,
    path::Path,
    sync::LazyLock,
    time::{Duration, Instant},
};

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

#[derive(Args, Clone, Debug, Default, Serialize)]
pub struct Filters {
    /// Only this agent; repeat for a union
    #[arg(long, value_enum)]
    pub agent: Vec<Agent>,
    /// Only events from this speaker
    #[arg(long, value_enum)]
    pub role: Option<Role>,
    /// Only sessions started on their own (main) or spawned by another session (subagent)
    #[arg(long, value_enum)]
    pub session_kind: Option<SessionKind>,
    /// Only events at or after this RFC 3339 time or UTC YYYY-MM-DD date
    #[arg(long, value_name = "TIME", value_parser = date)]
    pub since: Option<DateTime<Utc>>,
    /// Only events before this RFC 3339 time or UTC YYYY-MM-DD date
    #[arg(long, value_name = "TIME", value_parser = date)]
    pub until: Option<DateTime<Utc>>,
    /// Only sessions whose working directory is DIR or below it
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<String>,
    /// Only this session, by native ID or transcript path
    #[arg(long, value_name = "ID|PATH")]
    pub session: Option<String>,
    #[arg(skip)]
    #[serde(skip)]
    pub high_water: Option<i64>,
    /// Anchors of the frozen search ranking, from the first page's cursor.
    #[arg(skip)]
    #[serde(skip)]
    pub search_order: Option<Vec<i64>>,
}

impl Filters {
    pub fn narrowed(&self) -> bool {
        !self.agent.is_empty()
            || self.since.is_some()
            || self.until.is_some()
            || self.cwd.is_some()
            || self.session.is_some()
    }

    pub fn sql(&self, db: &Connection) -> Result<(String, Vec<Value>)> {
        let mut conditions = vec!["1=1".to_owned()];
        let mut args = Vec::new();
        if let Some(high_water) = self.high_water {
            conditions.push("e.id<=?".into());
            args.push(Value::Integer(high_water));
        }
        if !self.agent.is_empty() {
            conditions.push(format!(
                "s.agent IN ({})",
                vec!["?"; self.agent.len()].join(",")
            ));
            args.extend(self.agent.iter().map(|h| Value::Text(name(*h))));
        }
        for (field, value) in [
            ("e.role", self.role.map(name)),
            ("s.session_kind", self.session_kind.map(name)),
        ] {
            if let Some(value) = value {
                conditions.push(format!("{field}=?"));
                args.push(Value::Text(value));
            }
        }
        if let Some(session) = &self.session {
            match session_file(db, session)? {
                Some(file_id) => {
                    conditions.push("s.file_id=?".into());
                    args.push(Value::Integer(file_id));
                }
                None => conditions.push("0".into()),
            }
        }
        for (op, value) in [(">=", &self.since), ("<", &self.until)] {
            if let Some(value) = value {
                conditions.push(format!("e.ts{op}?"));
                args.push(Value::Text(
                    value.to_rfc3339_opts(SecondsFormat::Millis, true),
                ));
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

/// The transcript a session selector names. Claude subagent IDs are unique
/// only within their parent session, so an ID shared by several transcripts is
/// refused with their paths rather than merged.
pub fn session_file(db: &Connection, selector: &str) -> Result<Option<i64>> {
    let id = selector
        .strip_prefix("codex://threads/")
        .unwrap_or(selector);
    let matches: Vec<(i64, String)> = db
        .prepare_cached(
            "SELECT file_id,path FROM sessions WHERE session_id=? OR path=? ORDER BY path",
        )?
        .query_map([id, selector], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    match matches.as_slice() {
        [] => Ok(None),
        [(file_id, _)] => Ok(Some(*file_id)),
        _ => anyhow::bail!(
            "session ID {} names {} transcripts; pass one of their paths: {}",
            redact(selector),
            matches.len(),
            matches
                .iter()
                .map(|(_, path)| redact(path))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn date(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    Ok(NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .context("date must be RFC 3339 or YYYY-MM-DD")?
        .and_time(NaiveTime::MIN)
        .and_utc())
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub agent: String,
    pub session_id: String,
    pub path: String,
    pub line_no: u64,
    pub ts: Option<String>,
    pub role: String,
    pub model: Option<String>,
    #[serde(flatten)]
    pub body: Body,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(skip)]
    pub byte_off: u64,
    #[serde(skip)]
    pub byte_len: u64,
    #[serde(skip)]
    pub event_id: i64,
}

/// What a hit carries of its event: the indexed text for ranked lookup, or the
/// whole source record, parsed and redacted, for grep and show.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Body {
    Text { text: String, truncated: bool },
    Record { record: Json },
    Unavailable { unavailable: String },
}

#[derive(Debug, Default, Serialize)]
pub struct Coverage {
    pub incomplete: bool,
    pub continuation: Option<i64>,
    pub unavailable_ranges: usize,
    pub records: usize,
    pub bytes: u64,
}

const COLUMNS: &str = "e.agent,e.session_id,e.path,e.line_no,e.ts,e.role,e.model,e.byte_off,e.byte_len,e.id,e.text_truncated";
const FROM: &str = "FROM events e JOIN sessions s ON s.file_id=e.file_id";
// Rule: docs/formats.md `line_role_record`.
const FIRST_OF_LINE_ROLE: &str = "e.ordinal=(SELECT min(r.ordinal) FROM event_rows r WHERE r.file_id=e.file_id AND r.line_no=e.line_no AND r.role=e.role)";

fn hit(row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
    let agent: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let path: String = row.get(2)?;
    let line_no: u64 = row.get(3)?;
    let reference = format!("{path}:{line_no}");
    Ok(Hit {
        agent,
        session_id,
        path,
        line_no,
        ts: row.get(4)?,
        role: row.get(5)?,
        model: row.get(6)?,
        body: Body::Text {
            text: String::new(),
            truncated: row.get(10)?,
        },
        reference,
        byte_off: row.get(7)?,
        byte_len: row.get(8)?,
        event_id: row.get(9)?,
    })
}

static SEARCH_TOKENS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""([^"]+)"|(\S+)"#).expect("static regex is valid"));

pub fn fts_query(query: &str) -> Result<String> {
    anyhow::ensure!(!query.trim().is_empty(), "search query is empty");
    Ok(SEARCH_TOKENS
        .captures_iter(query)
        .map(|c| {
            let term = c
                .get(1)
                .or_else(|| c.get(2))
                .expect("one token alternative matched")
                .as_str();
            format!("\"{}\"", spaced_cjk(term).replace('"', "\"\""))
        })
        .collect::<Vec<_>>()
        .join(" AND "))
}

/// Sessions reachable through search pagination: larger allows deeper paging
/// and lengthens the cursor by about one integer per session.
pub const SEARCH_PAGE_SESSIONS: usize = 100;

#[derive(Debug, Serialize)]
pub struct Session {
    pub agent: String,
    pub session_id: String,
    pub session_kind: String,
    pub parent_id: Option<String>,
    pub path: String,
    pub cwd: String,
    pub started_at: Option<String>,
    pub matched_hits: usize,
    pub hits: Vec<Hit>,
    #[serde(skip)]
    file_id: i64,
}

pub struct SearchPage {
    pub sessions: Vec<Session>,
    /// Matching sessions at or below the high-water mark, ranked or not.
    pub matched_sessions: usize,
    /// Each session is anchored by its lowest matching event ID, which stays
    /// fixed below the high-water mark. Set only when no order was supplied.
    pub order: Option<Vec<i64>>,
}

pub fn search(
    db: &Connection,
    query: &str,
    filters: &Filters,
    limit: usize,
    offset: usize,
) -> Result<SearchPage> {
    let (clause, mut args) = filters.sql(db)?;
    args.push(Value::Text(fts_query(query)?));
    args.push(match &filters.search_order {
        Some(order) => Value::Text(serde_json::to_string(order)?),
        None => Value::Null,
    });
    args.push(Value::Integer(limit as i64));
    args.push(Value::Integer(offset as i64));
    args.push(Value::Integer(SEARCH_PAGE_SESSIONS as i64));
    // Materialize FTS scores before grouping: bm25 requires the original FTS cursor.
    // The final join starts from the ranked hits. Rule: docs/formats.md `search_hit_lookup`.
    let sql = format!(
        "WITH matches AS MATERIALIZED (
          SELECT e.file_id,e.id,e.ts,bm25(fts) AS score {FROM}
          JOIN fts ON fts.rowid=e.id WHERE {clause} AND fts MATCH ?
        ), ordering AS MATERIALIZED (SELECT ? AS ranks), all_heads AS MATERIALIZED (
          SELECT file_id,min(score) AS score,max(ts) AS ts,count(*) AS n,min(id) AS anchor
          FROM matches GROUP BY file_id
        ), ordered_heads AS MATERIALIZED (
          SELECT h.*,j.key AS position FROM ordering CROSS JOIN json_each(ordering.ranks) j
          JOIN all_heads h ON j.value=h.anchor
          UNION ALL SELECT h.*,0 AS position FROM all_heads h CROSS JOIN ordering WHERE ordering.ranks IS NULL
        ), heads AS MATERIALIZED (
          SELECT * FROM ordered_heads ORDER BY position,score,ts DESC,file_id LIMIT ? OFFSET ?
        ), ranked AS (
          SELECT m.id,row_number() OVER (
            PARTITION BY m.file_id ORDER BY m.score,m.ts DESC,m.id DESC
          ) AS n FROM matches m JOIN heads h USING(file_id)
        )
        SELECT {COLUMNS},h.n,(SELECT count(*) FROM all_heads),
          CASE WHEN ordering.ranks IS NULL THEN (SELECT json_group_array(anchor)
            FROM (SELECT anchor FROM all_heads ORDER BY score,ts DESC,file_id LIMIT ?)) END,
          coalesce(e.text,''),s.file_id,s.session_kind,s.parent_id,s.cwd,s.started_at
        FROM ranked r CROSS JOIN events e ON e.id=r.id JOIN sessions s ON s.file_id=e.file_id
        CROSS JOIN ordering JOIN heads h ON h.file_id=e.file_id
        WHERE r.n<=3 ORDER BY h.position,h.score,h.ts DESC,h.file_id,r.n"
    );
    let mut stmt = db.prepare(&sql)?;
    let mut matched_sessions = 0;
    let mut order = None;
    let mut sessions: Vec<Session> = Vec::new();
    let mut rows = stmt.query(params_from_iter(args))?;
    while let Some(r) = rows.next()? {
        matched_sessions = r.get(12)?;
        if order.is_none() {
            order = r.get::<_, Option<String>>(13)?;
        }
        let mut hit = hit(r)?;
        if let Body::Text { text, .. } = &mut hit.body {
            *text = redact(&r.get::<_, String>(14)?);
        }
        let file_id: i64 = r.get(15)?;
        match sessions.last_mut() {
            Some(s) if s.file_id == file_id => s.hits.push(hit),
            _ => sessions.push(Session {
                agent: hit.agent.clone(),
                session_id: hit.session_id.clone(),
                session_kind: r.get(16)?,
                parent_id: r.get(17)?,
                path: hit.path.clone(),
                cwd: r.get(18)?,
                started_at: r.get(19)?,
                matched_hits: r.get(11)?,
                hits: vec![hit],
                file_id,
            }),
        }
    }
    Ok(SearchPage {
        sessions,
        matched_sessions,
        order: order.map(|o| serde_json::from_str(&o)).transpose()?,
    })
}

/// `source` keeps the last opened file with its device and inode: consecutive
/// rows usually share it. It is reused only while the path still names that
/// file, so a source replaced mid-query is read as reopening it would.
fn read_range(
    db: &Connection,
    item: &Hit,
    source: &mut Option<((u64, u64), crate::source::Ranges)>,
) -> Result<Vec<u8>> {
    anyhow::ensure!(
        item.byte_len <= MAX_RECORD as u64,
        "source record exceeds display limit"
    );
    let expected: (u64, u64, [u8; 32]) = db
        .prepare_cached(
            "SELECT f.dev,f.inode,r.raw_hash FROM event_rows r JOIN files f ON f.id=r.file_id WHERE r.id=?",
        )?
        .query_row([item.event_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let named = crate::source::metadata(Path::new(&item.path))
        .context("source unreadable; run sessidx index")?;
    let (identity, ranges) = match source {
        Some((identity, ranges)) if *identity == (named.dev(), named.ino()) => (*identity, ranges),
        _ => {
            let opened = crate::source::open(Path::new(&item.path))
                .context("source unreadable; run sessidx index")?
                .context("source removed; run sessidx index")?;
            let identity = (opened.meta.dev(), opened.meta.ino());
            (identity, &mut source.insert((identity, opened.ranges())).1)
        }
    };
    anyhow::ensure!(
        identity == (expected.0, expected.1),
        "source replaced; run sessidx index"
    );
    let mut bytes = vec![0; item.byte_len as usize];
    ranges
        .read_exact_at(&mut bytes, item.byte_off)
        .context("source truncated; run sessidx index")?;
    anyhow::ensure!(
        Sha256::digest(&bytes)[..] == expected.2,
        "source range changed; run sessidx index"
    );
    Ok(bytes)
}

fn display_range(bytes: &[u8]) -> Result<Json> {
    let mut value: Json =
        serde_json::from_slice(bytes).context("source range changed; run sessidx index")?;
    crate::redaction::redact_value(&mut value);
    Ok(value)
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
        "grep requires a narrowing filter: add --since, --until, --cwd, --session, or --agent"
    );
    let deadline = Instant::now() + budget;
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    let result: Result<(Vec<Hit>, Coverage)> = (|| {
        let pattern = Regex::new(pattern).context("invalid grep regex")?;
        let (clause, mut args) = filters.sql(db)?;
        args.push(Value::Integer(after));
        let sql = format!(
            "SELECT {COLUMNS} {FROM} WHERE {clause} AND e.id>? AND {FIRST_OF_LINE_ROLE} ORDER BY e.id"
        );
        let mut stmt = db.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(args))?;
        let mut hits = Vec::new();
        let mut coverage = Coverage::default();
        let mut cursor = after;
        let mut source = None;
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
            match read_range(db, &item, &mut source) {
                Ok(bytes) => {
                    coverage.bytes += bytes.len() as u64;
                    if pattern.is_match(&String::from_utf8_lossy(&bytes)) {
                        item.body = Body::Record {
                            record: display_range(&bytes)?,
                        };
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
    db.progress_handler(0, None::<fn() -> bool>)?;
    result
}

pub fn show(
    db: &Connection,
    target: &str,
    around: usize,
    limit: usize,
    after: i64,
) -> Result<(Vec<Hit>, Coverage)> {
    show_snapshot(db, target, around, limit, after, i64::MAX)
}

pub fn show_snapshot(
    db: &Connection,
    target: &str,
    around: usize,
    limit: usize,
    after: i64,
    high_water: i64,
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
        "e.path=? AND e.line_no BETWEEN ? AND ?"
    } else {
        args.push(Value::Integer(session_file(db, target)?.unwrap_or(0)));
        "e.file_id=?"
    };
    args.push(Value::Integer(after));
    args.push(Value::Integer(high_water));
    args.push(Value::Integer((limit + 1) as i64));
    let sql = format!(
        "SELECT {COLUMNS} {FROM} WHERE {clause} AND e.id>? AND e.id<=? AND {FIRST_OF_LINE_ROLE} ORDER BY e.id LIMIT ?"
    );
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
    let mut source = None;
    for item in &mut hits {
        match read_range(db, item, &mut source).and_then(|bytes| {
            coverage.bytes += bytes.len() as u64;
            display_range(&bytes)
        }) {
            Ok(record) => item.body = Body::Record { record },
            Err(e) => {
                item.body = Body::Unavailable {
                    unavailable: redact(&format!("{e:#}")),
                };
                coverage.unavailable_ranges += 1;
                coverage.incomplete = true;
            }
        }
    }
    Ok((hits, coverage))
}
