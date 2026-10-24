mod grep;
mod page;
mod ranges;
mod search;
mod show;

use crate::model::{Agent, SessionKind, name};
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, SecondsFormat, Utc};
use clap::{Args, ValueEnum};
pub use grep::scan;
pub use page::{Body, CURSOR_MISMATCH, Coverage, Cut, Hit, Position};
use rusqlite::{Connection, types::Value};
pub use search::{SEARCH_PAGE_SESSIONS, SearchPage, Session, fts_query, search};
use serde::Serialize;
pub use show::{show, show_snapshot};
use std::time::Instant;

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
pub(super) fn unmatched(selector: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "session selector {selector} names no indexed transcript; a selector is a whole session ID, transcript path, or codex://threads/ link, not a prefix; its transcript may not be indexed yet, which sessidx index finishes, or may have been deleted; find an indexed session's ID with sessidx search or sessidx sql"
    )
}

/// Parses `--since` and `--until`: RFC 3339, or a UTC date.
pub fn date(s: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    Ok(NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .context("date must be RFC 3339 or YYYY-MM-DD")?
        .and_time(NaiveTime::MIN)
        .and_utc())
}

/// Runs `work` with SQLite interrupting any statement still running at
/// `deadline`, and returns `None` when it interrupted one. The handler is
/// removed after an error too, because a caller may query the connection past
/// the deadline, as `search` does to choose its recovery.
pub(crate) fn with_deadline<T>(
    db: &Connection,
    deadline: Instant,
    work: impl FnOnce() -> Result<T>,
) -> Result<Option<T>> {
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    let result = work();
    db.progress_handler(0, None::<fn() -> bool>)?;
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e)
            if matches!(
                e.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(f, _))
                    if f.code == rusqlite::ErrorCode::OperationInterrupted
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
