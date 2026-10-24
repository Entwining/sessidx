use super::{
    Filters,
    page::{Body, COLUMNS, Cut, Hit, PAGE_BYTES, fit, hit, line_len},
    with_deadline,
};
use crate::{redaction::spaced_cjk, store::compaction_due};
use anyhow::Result;
use regex::Regex;
use rusqlite::{Connection, params_from_iter, types::Value};
use serde::Serialize;
use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

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
    if let Some(page) = with_deadline(db, deadline, || rank(db, query, filters, limit))? {
        return Ok(page);
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
