use crate::{
    model::{Agent, SessionKind, name},
    redaction::{redact, spaced_cjk},
    store::{MAX_RECORD, compaction_due},
};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, SecondsFormat, Utc};
use clap::{Args, ValueEnum};
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params_from_iter, types::Value};
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
    /// Only this session, by its whole native ID, transcript path, or codex://threads/ID link
    #[arg(long, value_name = "ID|PATH")]
    pub session: Option<String>,
    #[arg(skip)]
    #[serde(skip)]
    pub high_water: Option<i64>,
    /// The transcripts of the frozen search ranking that a later page shows,
    /// in ranked order.
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

    /// Every filter, over events `e` joined to their sessions `s`.
    pub fn sql(&self, db: &Connection) -> Result<(String, Vec<Value>)> {
        let (sessions, mut args) = self.session_sql(db)?;
        let (events, event_args) = self.event_sql();
        args.extend(event_args);
        Ok((format!("{sessions} AND {events}"), args))
    }

    /// The filters on events `e` themselves: role, time, and high-water mark.
    pub fn event_sql(&self) -> (String, Vec<Value>) {
        let mut conditions = vec!["1=1".to_owned()];
        let mut args = Vec::new();
        if let Some(high_water) = self.high_water {
            conditions.push("e.id<=?".into());
            args.push(Value::Integer(high_water));
        }
        if let Some(role) = self.role {
            conditions.push("e.role=?".into());
            args.push(Value::Text(name(role)));
        }
        for (op, value) in [(">=", self.since()), ("<", self.until())] {
            if let Some(value) = value {
                conditions.push(format!("e.ts{op}?"));
                args.push(Value::Text(value));
            }
        }
        (conditions.join(" AND "), args)
    }

    /// `--since` in the stored timestamp format.
    pub fn since(&self) -> Option<String> {
        self.since
            .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
    }

    /// `--until` in the stored timestamp format.
    pub fn until(&self) -> Option<String> {
        self.until
            .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
    }

    /// The filters on sessions `s`: agent, kind, selector, and directory.
    pub fn session_sql(&self, db: &Connection) -> Result<(String, Vec<Value>)> {
        let mut conditions = vec!["1=1".to_owned()];
        let mut args = Vec::new();
        if !self.agent.is_empty() {
            conditions.push(format!(
                "s.agent IN ({})",
                vec!["?"; self.agent.len()].join(",")
            ));
            args.extend(self.agent.iter().map(|h| Value::Text(name(*h))));
        }
        if let Some(kind) = self.session_kind {
            conditions.push("s.session_kind=?".into());
            args.push(Value::Text(name(kind)));
        }
        if let Some(session) = &self.session {
            conditions.push("s.file_id=?".into());
            args.push(Value::Integer(session_file(db, session)?));
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
pub fn session_file(db: &Connection, selector: &str) -> Result<i64> {
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
        [] => Err(unmatched(selector)),
        [(file_id, _)] => Ok(*file_id),
        _ => anyhow::bail!(
            "session ID {} names {} transcripts; pass one of their paths: {}",
            selector,
            matches.len(),
            matches
                .iter()
                .map(|(_, path)| path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A selector that names no transcript fails rather than selecting nothing,
/// because an empty complete result reads as absence. Rule: docs/formats.md
/// `unmatched_selector`.
fn unmatched(selector: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "session selector {selector} names no indexed transcript; a selector is a whole session ID, transcript path, or codex://threads/ link, not a prefix; its transcript may not be indexed yet, which sessidx index finishes, or may have been deleted; find an indexed session's ID with sessidx search or sessidx sql"
    )
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<Cut>,
    #[serde(skip)]
    pub byte_off: u64,
    #[serde(skip)]
    pub byte_len: u64,
    #[serde(skip)]
    pub event_id: i64,
    #[serde(skip)]
    pub position: Position,
}

/// Where a grep or show page stopped: the file ID, line, and ordinal of the
/// last event it examined, or `i64::MAX` line and ordinal for a file read to
/// its end. Files are read in ID order and each file in line order, so a later
/// page resumes after this position.
pub type Position = (i64, i64, i64);

/// What a hit carries of its event: the indexed text for ranked lookup, the
/// whole source record, parsed and redacted, for grep and show, or part of
/// that record's JSON text when it does not fit a page alone.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Body {
    Text { text: String, truncated: bool },
    Record { record: Json },
    Part { part: String },
    Unavailable { unavailable: String },
}

impl Hit {
    /// Replaces a record that does not fit a page alone with the part of its
    /// JSON text from `offset` that does, and returns where the next part
    /// starts while any is left.
    fn fit(&mut self, kind: &str, offset: usize) -> Result<Option<usize>> {
        let Body::Record { record } = &self.body else {
            return Ok(None);
        };
        let text = record.to_string();
        anyhow::ensure!(text.is_char_boundary(offset), CURSOR_MISMATCH);
        self.body = Body::Part { part: text };
        let line = line_len(self, kind);
        if let Body::Part { part } = &mut self.body {
            self.cut = fit(line, vec![("/part".to_owned(), part, offset)]);
        }
        Ok(self
            .cut
            .iter()
            .find(|c| c.offset + c.kept < c.bytes)
            .map(|c| c.offset + c.kept))
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Coverage {
    pub incomplete: bool,
    pub continuation: Option<Position>,
    /// Where `show` resumes in the JSON text of the record at `continuation`,
    /// which the next page shows again from there.
    pub within: Option<usize>,
    pub unavailable_ranges: usize,
    pub records: usize,
    pub bytes: u64,
}

/// Data lines per page, in bytes. Rule: docs/formats.md `display_page`.
const PAGE_BYTES: usize = 24 * 1024;

/// A string shortened to fit a page: `field` is its JSON Pointer in the data
/// record, `bytes` the length of the whole string, and the page holds `kept`
/// bytes of it starting at `offset`.
#[derive(Debug, Serialize)]
pub struct Cut {
    pub field: String,
    pub bytes: usize,
    pub offset: usize,
    pub kept: usize,
}

/// Serializing fails only for a map with non-string keys, which no output has.
fn json_len(value: &impl Serialize) -> usize {
    serde_json::to_vec(value)
        .expect("output records serialize")
        .len()
}

fn line_len(record: &impl Serialize, kind: &str) -> usize {
    json_len(record) + r#","type":"""#.len() + kind.len() + 1
}

pub const CURSOR_MISMATCH: &str =
    "cursor belongs to another query or index version; restart without --cursor";

/// Shortens strings, each from its offset, to the largest common length that
/// fits a data line of `line` bytes into [`PAGE_BYTES`], so short ones stay
/// whole, and lists each string that was shortened or starts past its
/// beginning.
fn fit(line: usize, slots: Vec<(String, &mut String, usize)>) -> Vec<Cut> {
    let whole: usize = slots.iter().map(|(_, text, _)| json_len(text)).sum();
    let layout = |limit: usize| {
        let mut size = line - whole;
        let mut kept = Vec::new();
        let mut cuts = Vec::new();
        for (field, text, offset) in &slots {
            let rest = &text[*offset..];
            let k = rest.floor_char_boundary(limit);
            size += json_len(&&rest[..k]);
            if k < rest.len() || *offset > 0 {
                cuts.push(Cut {
                    field: field.clone(),
                    bytes: text.len(),
                    offset: *offset,
                    kept: k,
                });
            }
            kept.push(k);
        }
        if !cuts.is_empty() {
            size += r#","cut":"#.len() + json_len(&cuts);
        }
        (size, kept, cuts)
    };
    // Four bytes hold any character, so a continuing record always advances.
    let limits: Vec<usize> = (4..=PAGE_BYTES).collect();
    let fitting = limits.partition_point(|&limit| layout(limit).0 <= PAGE_BYTES);
    let (_, kept, cuts) = layout(limits[fitting.saturating_sub(1)]);
    for ((_, text, offset), k) in slots.into_iter().zip(kept) {
        *text = text[offset..offset + k].to_owned();
    }
    cuts
}

/// A hit's columns, from `event_rows e` and `sessions s`.
const COLUMNS: &str = "s.agent,s.session_id,s.path,e.line_no,e.ts,e.role,e.model,e.byte_off,e.byte_len,e.id,e.file_id,e.ordinal";

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
            truncated: false,
        },
        reference,
        cut: Vec::new(),
        byte_off: row.get(7)?,
        byte_len: row.get(8)?,
        event_id: row.get(9)?,
        position: (row.get(10)?, row.get(3)?, row.get(11)?),
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<Cut>,
    #[serde(skip)]
    pub file_id: i64,
}

impl Session {
    /// Cuts hit texts so the session's line fits a page.
    fn fit(&mut self) {
        let line = line_len(self, "session");
        let slots = self
            .hits
            .iter_mut()
            .enumerate()
            .filter_map(|(i, hit)| match &mut hit.body {
                Body::Text { text, .. } => Some((format!("/hits/{i}/text"), text, 0)),
                _ => None,
            })
            .collect();
        self.cut = fit(line, slots);
    }
}

pub struct SearchPage {
    pub sessions: Vec<Session>,
    /// Matching sessions at or below the high-water mark, ranked or not, among
    /// all sessions or those a frozen order lists.
    pub matched_sessions: usize,
    /// The file IDs of the first ranked sessions, which never name another
    /// transcript, when no order was supplied.
    pub order: Option<Vec<i64>>,
}

/// Ranks matching sessions and returns the first `limit` of them; with a
/// frozen order in `filters.search_order`, ranks only the transcripts it
/// lists, in that order. The query stops at `budget`.
pub fn search(
    db: &Connection,
    query: &str,
    filters: &Filters,
    limit: usize,
    budget: Duration,
) -> Result<SearchPage> {
    let deadline = Instant::now() + budget;
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    let result = rank(db, query, filters, limit);
    db.progress_handler(0, None::<fn() -> bool>)?;
    let error = match result {
        Ok(page) => return Ok(page),
        Err(e) => e,
    };
    if !matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ErrorCode::OperationInterrupted
    ) {
        return Err(error);
    }
    // Filters apply after each match is read, so they do not shorten the
    // search; fewer matches, a warm page cache, or a compacted index do. Rule:
    // docs/formats.md `search_deadline`.
    let recovery = if compaction_due(db)? {
        "most of the index was written since it was last compacted, which slows reads that are not cached; run `sessidx index` to compact it, then search again"
    } else {
        "search again, since a first read that is not cached is slower; if it exceeds the limit again, add distinctive terms"
    };
    anyhow::bail!("search exceeded {} seconds; {recovery}", budget.as_secs())
}

fn rank(db: &Connection, query: &str, filters: &Filters, limit: usize) -> Result<SearchPage> {
    let (clause, mut args) = filters.sql(db)?;
    let frozen = filters
        .search_order
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let (listed, order) = match &frozen {
        Some(order) => {
            args.push(Value::Text(order.clone()));
            (
                "AND e.file_id IN (SELECT value FROM json_each(?))",
                "(SELECT key FROM json_each(?) WHERE value=file_id)",
            )
        }
        None => ("", "0"),
    };
    args.push(Value::Text(fts_query(query)?));
    if let Some(order) = &frozen {
        args.push(Value::Text(order.clone()));
    }
    args.push(Value::Integer(limit as i64));
    args.push(Value::Integer(SEARCH_PAGE_SESSIONS as i64));
    // Materialize FTS scores before grouping: bm25 requires the original FTS cursor.
    // The final join starts from the ranked hits. Rules: docs/formats.md
    // `search_hit_lookup` and `cursor_search_order`.
    let sql = format!(
        "WITH matches AS MATERIALIZED (
          SELECT e.file_id,e.id,e.ts,bm25(fts) AS score
          FROM event_rows e JOIN sessions s ON s.file_id=e.file_id
          JOIN fts ON fts.rowid=e.id WHERE {clause} {listed} AND fts MATCH ?
        ), all_heads AS MATERIALIZED (
          SELECT file_id,min(score) AS score,max(ts) AS ts,count(*) AS n,{order} AS position
          FROM matches GROUP BY file_id
        ), heads AS MATERIALIZED (
          SELECT * FROM all_heads ORDER BY position,score,ts DESC,file_id LIMIT ?
        ), ranked AS (
          SELECT m.id,row_number() OVER (
            PARTITION BY m.file_id ORDER BY m.score,m.ts DESC,m.id DESC
          ) AS n FROM matches m JOIN heads h USING(file_id)
        )
        SELECT {COLUMNS},h.n,(SELECT count(*) FROM all_heads),
          (SELECT json_group_array(file_id)
            FROM (SELECT file_id FROM all_heads ORDER BY score,ts DESC,file_id LIMIT ?)),
          coalesce(t.text,''),coalesce(t.truncated,0),s.session_kind,s.parent_id,s.cwd,s.started_at
        FROM ranked r CROSS JOIN event_rows e ON e.id=r.id CROSS JOIN sessions s ON s.file_id=e.file_id
        LEFT JOIN event_texts t ON t.event_id=e.id JOIN heads h ON h.file_id=e.file_id
        WHERE r.n<=3 ORDER BY h.position,h.score,h.ts DESC,h.file_id,r.n"
    );
    let mut stmt = db.prepare(&sql)?;
    let mut matched_sessions = 0;
    let mut order = None;
    let mut sessions: Vec<Session> = Vec::new();
    let mut rows = stmt.query(params_from_iter(args))?;
    while let Some(r) = rows.next()? {
        matched_sessions = r.get(13)?;
        if order.is_none() {
            order = Some(r.get::<_, String>(14)?);
        }
        let mut hit = hit(r)?;
        hit.body = Body::Text {
            text: r.get(15)?,
            truncated: r.get(16)?,
        };
        let file_id = hit.position.0;
        match sessions.last_mut() {
            Some(s) if s.file_id == file_id => s.hits.push(hit),
            _ => sessions.push(Session {
                agent: hit.agent.clone(),
                session_id: hit.session_id.clone(),
                session_kind: r.get(17)?,
                parent_id: r.get(18)?,
                path: hit.path.clone(),
                cwd: r.get(19)?,
                started_at: r.get(20)?,
                matched_hits: r.get(12)?,
                hits: vec![hit],
                cut: Vec::new(),
                file_id,
            }),
        }
    }
    // A session that does not fit after others starts the next page.
    let mut used = 0;
    for (i, session) in sessions.iter_mut().enumerate() {
        let line = line_len(session, "session");
        if used + line > PAGE_BYTES {
            if i > 0 {
                sessions.truncate(i);
                break;
            }
            session.fit();
        }
        used += line_len(session, "session");
    }
    Ok(SearchPage {
        sessions,
        matched_sessions,
        order: match order {
            Some(order) if frozen.is_none() => Some(serde_json::from_str(&order)?),
            _ => None,
        },
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

/// Matches `pattern` against the source ranges of the selected sessions'
/// events after `after`, a transcript at a time in file ID order, until `limit`
/// hits or `budget`. Rule: docs/formats.md `grep_traversal`.
pub fn scan(
    db: &Connection,
    pattern: &str,
    filters: &Filters,
    limit: usize,
    after: Option<Position>,
    budget: Duration,
) -> Result<(Vec<Hit>, Coverage)> {
    anyhow::ensure!(
        filters.narrowed(),
        "grep requires a narrowing filter: add --since, --until, --cwd, --session, or --agent"
    );
    let deadline = Instant::now() + budget;
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    let mut hits = Vec::new();
    let mut coverage = Coverage::default();
    let mut cursor = after.unwrap_or((0, 0, 0));
    // Whether the page stopped before the end of the selection.
    let stopped = (|| -> Result<bool> {
        let pattern = Regex::new(pattern).context("invalid grep regex")?;
        let (sessions, mut args) = filters.session_sql(db)?;
        let mut conditions = vec![sessions, "s.file_id>=?".to_owned()];
        args.push(Value::Integer(cursor.0));
        // A transcript outside the window holds no event the time filter keeps.
        if let Some(since) = filters.since() {
            conditions.push("s.max_ts>=?".into());
            args.push(Value::Text(since));
        }
        if let Some(until) = filters.until() {
            conditions.push("s.min_ts<?".into());
            args.push(Value::Text(until));
        }
        let files = db
            .prepare(&format!(
                "SELECT s.file_id FROM sessions s WHERE {} ORDER BY s.file_id",
                conditions.join(" AND ")
            ))?
            .query_map(params_from_iter(args), |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let (events, event_args) = filters.event_sql();
        // Rule: docs/formats.md `line_role_record`.
        let mut stmt = db.prepare(&format!(
            "SELECT {COLUMNS} FROM event_rows e JOIN sessions s ON s.file_id=e.file_id
             WHERE e.file_id=? AND (e.line_no,e.ordinal)>(?,?) AND e.line_role_first=1 AND {events}
             ORDER BY e.line_no,e.ordinal"
        ))?;
        let mut source = None;
        let mut used = 0;
        for file in files {
            let (line, ordinal) = if file == cursor.0 {
                (cursor.1, cursor.2)
            } else {
                (0, 0)
            };
            let mut rows = stmt.query(params_from_iter(
                [file, line, ordinal]
                    .map(Value::Integer)
                    .into_iter()
                    .chain(event_args.iter().cloned()),
            ))?;
            loop {
                if Instant::now() >= deadline || hits.len() >= limit {
                    return Ok(true);
                }
                let Some(row) = rows.next()? else { break };
                let mut item = hit(row)?;
                let previous = cursor;
                cursor = item.position;
                coverage.records += 1;
                match read_range(db, &item, &mut source) {
                    Ok(bytes) => {
                        coverage.bytes += bytes.len() as u64;
                        if pattern.is_match(&String::from_utf8_lossy(&bytes)) {
                            item.body = Body::Record {
                                record: display_range(&bytes)?,
                            };
                            if used + line_len(&item, "hit") > PAGE_BYTES {
                                // The next page starts with this hit.
                                if !hits.is_empty() {
                                    coverage.records -= 1;
                                    coverage.bytes -= bytes.len() as u64;
                                    cursor = previous;
                                    return Ok(true);
                                }
                                item.fit("hit", 0)?;
                            }
                            used += line_len(&item, "hit");
                            hits.push(item);
                        }
                    }
                    Err(_) => {
                        coverage.unavailable_ranges += 1;
                        coverage.incomplete = true;
                    }
                }
            }
            cursor = (file, i64::MAX, i64::MAX);
        }
        Ok(false)
    })();
    db.progress_handler(0, None::<fn() -> bool>)?;
    let stopped = match stopped {
        Ok(stopped) => stopped,
        Err(e)
            if matches!(
                e.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(f, _))
                    if f.code == rusqlite::ErrorCode::OperationInterrupted
            ) && Instant::now() >= deadline =>
        {
            true
        }
        Err(e) => return Err(e),
    };
    if stopped {
        coverage.incomplete = true;
        coverage.continuation = Some(cursor);
    }
    Ok((hits, coverage))
}

pub fn show(
    db: &Connection,
    target: &str,
    around: usize,
    limit: usize,
    after: Option<Position>,
) -> Result<(Vec<Hit>, Coverage)> {
    show_snapshot(db, target, around, limit, after, None, i64::MAX)
}

/// The records of one transcript, or of the lines around `path:line`, after
/// `after`, among events up to `high_water`.
pub fn show_snapshot(
    db: &Connection,
    target: &str,
    around: usize,
    limit: usize,
    after: Option<Position>,
    mut within: Option<usize>,
    high_water: i64,
) -> Result<(Vec<Hit>, Coverage)> {
    let (file, lines) = match target
        .rsplit_once(':')
        .filter(|(_, l)| l.parse::<u64>().is_ok())
    {
        Some((path, line)) => {
            let line = line.parse::<u64>()?;
            let file = db
                .prepare_cached("SELECT file_id FROM sessions WHERE path=?")?
                .query_row([path], |r| r.get::<_, i64>(0))
                .optional()?
                .ok_or_else(|| unmatched(path))?;
            let first = line.saturating_sub(around as u64).max(1);
            let last = line.saturating_add(around as u64).min(i64::MAX as u64);
            (file, (first as i64, last as i64))
        }
        None => (session_file(db, target)?, (1, i64::MAX)),
    };
    let (line, ordinal) = match after {
        Some((after_file, line, ordinal)) => {
            anyhow::ensure!(after_file == file, CURSOR_MISMATCH);
            (line, ordinal)
        }
        None => (0, 0),
    };
    // A record continued inside its JSON text is shown again from `within`.
    let resume = if within.is_some() { ">=" } else { ">" };
    let rows = db
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM event_rows e JOIN sessions s ON s.file_id=e.file_id
             WHERE e.file_id=? AND e.line_no BETWEEN ? AND ? AND (e.line_no,e.ordinal){resume}(?,?)
             AND e.id<=? AND e.line_role_first=1 ORDER BY e.line_no,e.ordinal LIMIT ?"
        ))?
        .query_map(
            params_from_iter([
                file,
                lines.0,
                lines.1,
                line,
                ordinal,
                high_water,
                (limit + 1) as i64,
            ]),
            hit,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut hits: Vec<Hit> = Vec::new();
    let mut coverage = Coverage::default();
    let mut source = None;
    let mut used = 0;
    for mut item in rows {
        if hits.len() == limit {
            coverage.incomplete = true;
            coverage.continuation = hits.last().map(|h| h.position);
            break;
        }
        let mut read = 0;
        let unavailable = match read_range(db, &item, &mut source).and_then(|bytes| {
            read = bytes.len() as u64;
            display_range(&bytes)
        }) {
            Ok(record) => {
                item.body = Body::Record { record };
                false
            }
            Err(e) => {
                item.body = Body::Unavailable {
                    unavailable: redact(&format!("{e:#}")),
                };
                true
            }
        };
        let offset = within.take();
        if offset.is_some() || used + line_len(&item, "record") > PAGE_BYTES {
            // A record that does not fit after others starts the next page;
            // one that fills a page alone continues inside it.
            if !hits.is_empty() {
                coverage.incomplete = true;
                coverage.continuation = hits.last().map(|h| h.position);
                break;
            }
            coverage.within = item.fit("record", offset.unwrap_or(0))?;
        }
        coverage.records += 1;
        coverage.bytes += read;
        if unavailable {
            coverage.unavailable_ranges += 1;
            coverage.incomplete = true;
        }
        used += line_len(&item, "record");
        let position = item.position;
        hits.push(item);
        if coverage.within.is_some() {
            coverage.incomplete = true;
            coverage.continuation = Some(position);
            break;
        }
    }
    Ok((hits, coverage))
}
