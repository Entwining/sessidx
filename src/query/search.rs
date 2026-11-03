use super::{
    Filters, IN_FLIGHT_LOOKUP,
    page::{Body, COLUMNS, Cut, Hit, PAGE_BYTES, hit, line_len},
    with_deadline,
};
use crate::{redaction::spaced_cjk, store::compaction_due};
use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::{Connection, params, params_from_iter, types::Value};
use serde::Serialize;
use std::{
    ops::Range,
    sync::LazyLock,
    time::{Duration, Instant},
};

static SEARCH_TOKENS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""([^"]+)"|(\S+)"#).expect("static regex is valid"));
static TOKENIZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"tokenize='[^']*'").expect("static regex is valid"));

/// The most bytes of a hit's text that a search page shows, around its
/// matches: larger shows more context and fits fewer sessions on a page. Rule:
/// docs/formats.md `search_excerpt`.
pub const EXCERPT_BYTES: usize = 512;

/// The most bytes of a hit's text that one FTS5 `highlight` call reads. Its
/// time grows with the square of a text's matches, about 3 s for 100,000, so a
/// longer text is read in pieces: larger lets a text with more matches exceed
/// the deadline, smaller highlights more rows.
const PIECE_BYTES: usize = 8 * EXCERPT_BYTES;

/// The highlight markers around a match. A text that holds one shows its
/// prefix instead, since its matches cannot be told from it.
const OPEN: char = '\u{1}';
const CLOSE: char = '\u{2}';

/// The quoted FTS5 phrases of a search query, which a match holds all of.
fn fts_phrases(query: &str) -> Result<Vec<String>> {
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
        .collect())
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
    #[serde(serialize_with = "ranked_hits")]
    pub hits: Vec<Hit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<Cut>,
    #[serde(skip)]
    pub file_id: i64,
}

/// Writes each hit without the agent, session ID, and path its session
/// already carries, or the line its `ref` names, which would take a page's
/// room from hit text.
fn ranked_hits<S: serde::Serializer>(hits: &[Hit], serializer: S) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct Ranked<'a> {
        ts: &'a Option<String>,
        role: &'a str,
        model: &'a Option<String>,
        #[serde(flatten)]
        body: &'a Body,
        #[serde(rename = "ref")]
        reference: &'a str,
    }
    serializer.collect_seq(hits.iter().map(|hit| Ranked {
        ts: &hit.ts,
        role: &hit.role,
        model: &hit.model,
        body: &hit.body,
        reference: &hit.reference,
    }))
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
    if let Some(page) = with_deadline(db, deadline, || rank(db, query, filters, limit, deadline))? {
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

fn rank(
    db: &Connection,
    query: &str,
    filters: &Filters,
    limit: usize,
    deadline: Instant,
) -> Result<SearchPage> {
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
    let phrases = fts_phrases(query)?;
    args.push(Value::Text(phrases.join(" AND ")));
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
            AND NOT {IN_FLIGHT_LOOKUP}
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
    excerpt(db, &phrases, &mut sessions, deadline)?;
    paginate(&mut sessions);
    Ok(SearchPage {
        sessions,
        matched_sessions,
        order: match order {
            Some(order) if frozen.is_none() => Some(serde_json::from_str(&order)?),
            _ => None,
        },
    })
}

/// Keeps the sessions that fit a page; a session that does not fit after
/// others starts the next page. Excerpts keep a session's line to a few KiB, so
/// one that fills a page alone, which only a session field the log supplies,
/// such as a `cwd` of kilobytes, could make, is kept whole rather than cut.
fn paginate(sessions: &mut Vec<Session>) {
    let mut used = 0;
    let fitting = sessions
        .iter()
        .take_while(|session| {
            used += line_len(session, "session");
            used <= PAGE_BYTES
        })
        .count();
    sessions.truncate(fitting.max(1));
}

/// Replaces each hit text longer than [`EXCERPT_BYTES`] with the window
/// around its matches, listed in its session's `cut`. FTS5 finds each
/// phrase's matches, so they are the ones the ranking counted. Rule:
/// docs/formats.md `search_excerpt`.
fn excerpt(
    db: &Connection,
    phrases: &[String],
    sessions: &mut [Session],
    deadline: Instant,
) -> Result<()> {
    let long: Vec<(usize, usize)> = sessions
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.hits.iter().enumerate().map(move |(j, h)| (i, j, h)))
        .filter(
            |(_, _, h)| matches!(&h.body, Body::Text { text, .. } if text.len() > EXCERPT_BYTES),
        )
        .map(|(i, j, _)| (i, j))
        .collect();
    if long.is_empty() {
        return Ok(());
    }
    fn text(hit: &Hit) -> &str {
        match &hit.body {
            Body::Text { text, .. } => text,
            _ => unreachable!("search hits carry text"),
        }
    }
    // The live table's tokenizer, so the matches are the ones it indexed.
    let schema: String =
        db.query_row("SELECT sql FROM sqlite_schema WHERE name='fts'", [], |r| {
            r.get(0)
        })?;
    let tokenize = TOKENIZE
        .find(&schema)
        .context("the fts table names its tokenizer")?
        .as_str();
    let scratch = Connection::open_in_memory()?;
    scratch.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    scratch.execute_batch(&format!(
        "CREATE VIRTUAL TABLE excerpt USING fts5(text, {tokenize})"
    ))?;
    let mut insert = scratch.prepare("INSERT INTO excerpt(rowid,text) VALUES (?,?)")?;
    // Each piece's hit and the range of its text it reads.
    let mut read = Vec::new();
    // Each text's spans per phrase, or `None` once a phrase's could not be
    // aligned or the text holds a marker.
    let mut spans = vec![Some(vec![Vec::new(); phrases.len()]); long.len()];
    for (n, &(i, j)) in long.iter().enumerate() {
        let text = text(&sessions[i].hits[j]);
        if text.contains([OPEN, CLOSE]) {
            spans[n] = None;
            continue;
        }
        for range in pieces(text) {
            insert.execute(params![read.len() as i64, spaced_cjk(&text[range.clone()])])?;
            read.push((n, range));
        }
    }
    let mut stmt = scratch.prepare(
        "SELECT rowid,highlight(excerpt,0,char(1),char(2)) FROM excerpt WHERE excerpt MATCH ? ORDER BY rowid",
    )?;
    for (p, phrase) in phrases.iter().enumerate() {
        let mut rows = stmt.query([phrase])?;
        while let Some(row) = rows.next()? {
            let (n, range) = &read[usize::try_from(row.get::<_, i64>(0)?)?];
            let (i, j) = long[*n];
            let whole = text(&sessions[i].hits[j]);
            match (
                &mut spans[*n],
                align(&whole[range.clone()], &row.get::<_, String>(1)?),
            ) {
                // A piece that starts inside a token longer than a window
                // reads the token's end as a token of its own.
                (Some(found), Some(aligned)) => found[p].extend(
                    aligned
                        .into_iter()
                        .map(|s| s.start + range.start..s.end + range.start)
                        .filter(|s| !(s.start == range.start && splits_token(whole, s.start))),
                ),
                (slot, _) => *slot = None,
            }
        }
    }
    for (&(i, j), mut spans) in long.iter().zip(spans) {
        // Pieces overlap, so a match can arrive twice and after later ones.
        for found in spans.iter_mut().flatten() {
            found.sort_unstable_by_key(|s| s.start);
            found.dedup();
        }
        let session = &mut sessions[i];
        let Body::Text { text, .. } = &mut session.hits[j].body else {
            unreachable!("search hits carry text")
        };
        let window = window(text, spans.as_deref().unwrap_or_default(), deadline)?;
        session.cut.push(Cut {
            field: format!("/hits/{j}/text"),
            bytes: text.len(),
            offset: window.start,
            kept: window.len(),
        });
        *text = text[window].to_owned();
    }
    Ok(())
}

/// The ranges of `text` that [`excerpt`] highlights. Each reads a window past
/// where the next starts, so a match a window can hold lies whole in one.
fn pieces(text: &str) -> Vec<Range<usize>> {
    let mut starts = vec![0];
    while let Some(&last) = starts.last()
        && last + PIECE_BYTES < text.len()
    {
        starts.push(token_boundary(text, last + PIECE_BYTES));
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &start)| {
            let next = starts.get(k + 1).copied().unwrap_or(text.len());
            start..token_boundary(text, next + EXCERPT_BYTES)
        })
        .collect()
}

/// The last position at most a window before `at` that no token spans, so
/// cutting there leaves the tokens on both sides whole; `at` itself inside a
/// token longer than that, which no window can show whole anyway.
fn token_boundary(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    let at = text.floor_char_boundary(at);
    std::iter::once(at)
        .chain(text[..at].char_indices().rev().map(|(i, _)| i))
        .take_while(|&cut| at - cut <= EXCERPT_BYTES)
        .find(|&cut| !splits_token(text, cut))
        .unwrap_or(at)
}

/// Whether `at` lies inside one token of FTS5's `unicode61`, which joins
/// letters, digits, and the diacritics it removes, while [`spaced_cjk`] makes
/// each CJK ideograph a token of its own by spacing it.
fn splits_token(text: &str, at: usize) -> bool {
    // `spaced_cjk` collapses whitespace, so only a letter before shows the space.
    let alone = |c: char| spaced_cjk(&format!("a{c}")).contains(' ');
    let joined = |c: char| c.is_alphanumeric() && !alone(c) || ('\u{300}'..='\u{331}').contains(&c);
    text[..at].chars().next_back().is_some_and(joined)
        && text[at..].chars().next().is_some_and(joined)
}

/// The byte ranges of `text` that `highlighted` marks. `highlighted` is
/// `spaced_cjk(text)` with markers, so it differs from `text` only in
/// whitespace and the two align character by character over the rest; `None`
/// when they do not.
fn align(text: &str, highlighted: &str) -> Option<Vec<Range<usize>>> {
    let mut rest = text
        .char_indices()
        .filter(|(_, c)| !c.is_whitespace())
        .peekable();
    let mut spans = Vec::new();
    let (mut start, mut end) = (None, 0);
    for c in highlighted.chars() {
        match c {
            OPEN => start = Some(rest.peek()?.0),
            CLOSE => spans.push(start.take()?..end),
            c if c.is_whitespace() => {}
            c => {
                let (i, found) = rest.next()?;
                if found != c {
                    return None;
                }
                end = i + c.len_utf8();
            }
        }
    }
    Some(spans)
}

/// The range of at most [`EXCERPT_BYTES`] of `text` holding the most query
/// phrases, given each phrase's spans in order, starting at the line before
/// its first match when that line starts within an eighth of a window, and
/// otherwise an eighth of a window before it, or less when the match would not
/// fit after that. The earliest wins a tie, and the prefix is the window when
/// nothing matched. Fails as interrupted once `deadline` passes, as the
/// statements before it do.
fn window(
    text: &str,
    phrases: &[Vec<Range<usize>>],
    deadline: Instant,
) -> rusqlite::Result<Range<usize>> {
    // Each match start with the end of the longest match there.
    let mut firsts = phrases
        .iter()
        .flatten()
        .map(|s| (s.start, s.end))
        .collect::<Vec<_>>();
    firsts.sort_unstable_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
    firsts.dedup_by_key(|first| first.0);
    let mut best = (0, 0..text.floor_char_boundary(EXCERPT_BYTES));
    for (first, last) in firsts {
        if Instant::now() >= deadline {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
                None,
            ));
        }
        let room = EXCERPT_BYTES.saturating_sub(last - first);
        let lead = text.ceil_char_boundary(first.saturating_sub(room.min(EXCERPT_BYTES / 8)));
        let start = text[lead..first].rfind('\n').map_or(lead, |i| lead + i + 1);
        // A window that would pass the end shows the end's full width instead.
        let (start, end) = if start + EXCERPT_BYTES >= text.len() {
            (
                text.ceil_char_boundary(text.len() - EXCERPT_BYTES),
                text.len(),
            )
        } else {
            (start, text.floor_char_boundary(start + EXCERPT_BYTES))
        };
        // A phrase's spans end in the order they start, so its first from `start` ends first.
        let covered = phrases
            .iter()
            .filter(|spans| {
                spans
                    .get(spans.partition_point(|s| s.start < start))
                    .is_some_and(|s| s.end <= end)
            })
            .count();
        if covered > best.0 {
            best = (covered, start..end);
        }
    }
    Ok(best.1)
}
