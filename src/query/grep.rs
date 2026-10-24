use super::{
    Filters,
    page::{Body, COLUMNS, Coverage, Hit, PAGE_BYTES, Position, hit, line_len},
    ranges::{display_range, read_range},
    with_deadline,
};
use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::{Connection, params_from_iter, types::Value};
use std::time::{Duration, Instant};

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
    let mut hits = Vec::new();
    let mut coverage = Coverage::default();
    let mut cursor = after.unwrap_or((0, 0, 0));
    // Whether the page stopped before the end of the selection.
    let stopped = with_deadline(db, deadline, || -> Result<bool> {
        let pattern = Regex::new(pattern).context("invalid grep regex")?;
        let files = selected_files(db, filters, cursor.0)?;
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
    })?
    .unwrap_or(true);
    if stopped {
        coverage.incomplete = true;
        coverage.continuation = Some(cursor);
    }
    Ok((hits, coverage))
}

/// The file IDs of the transcripts `filters` selects, from `first` on, in order.
fn selected_files(db: &Connection, filters: &Filters, first: i64) -> Result<Vec<i64>> {
    let (sessions, mut args) = filters.session_sql(db)?;
    let mut conditions = vec![sessions, "s.file_id>=?".to_owned()];
    args.push(Value::Integer(first));
    // A transcript outside the window holds no event the time filter keeps.
    if let Some(since) = filters.since() {
        conditions.push("s.max_ts>=?".into());
        args.push(Value::Text(since));
    }
    if let Some(until) = filters.until() {
        conditions.push("s.min_ts<?".into());
        args.push(Value::Text(until));
    }
    Ok(db
        .prepare(&format!(
            "SELECT s.file_id FROM sessions s WHERE {} ORDER BY s.file_id",
            conditions.join(" AND ")
        ))?
        .query_map(params_from_iter(args), |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
