use super::{
    page::{Body, COLUMNS, CURSOR_MISMATCH, Coverage, Hit, PAGE_BYTES, Position, hit, line_len},
    ranges::{display_range, read_range},
    session_file, unmatched,
};
use crate::redaction::redact;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params_from_iter};

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
