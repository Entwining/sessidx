use super::rollups::add_counts;
use anyhow::Result;
use rusqlite::{Connection, Transaction};

/// Creates this connection's tables of the keys whose derived columns a
/// transaction must recompute before it commits, and of the rows whose values
/// change.
pub(super) fn create_work_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS dirty_groups(agent TEXT, native_id BLOB, kind TEXT, PRIMARY KEY(agent,native_id,kind)) WITHOUT ROWID;
         CREATE TEMP TABLE IF NOT EXISTS dirty_calls(agent TEXT, session_id TEXT, call_id BLOB, PRIMARY KEY(agent,session_id,call_id)) WITHOUT ROWID;
         CREATE TEMP TABLE IF NOT EXISTS derived_changes(id INTEGER PRIMARY KEY, canonical INTEGER, call_ok INTEGER, call_ok_source TEXT, call_event_id INTEGER);",
    )?;
    Ok(())
}

pub(super) fn delete_file(tx: &Transaction<'_>, id: i64) -> Result<()> {
    // By natural key, before the cascade removes the rows that name the keys.
    mark(tx, "r.file_id=?", id)?;
    tx.execute("INSERT INTO index_changes(file_id,first_event_id) SELECT file_id,min(id) FROM event_rows WHERE file_id=? HAVING count(*)>0", [id])?;
    // A result elsewhere whose call is in this file contributes that call's
    // sites to the rollups, so it lets go of the call while the sites can still
    // be subtracted; `settle` pairs it again once the file is gone.
    tx.execute("INSERT INTO temp.derived_changes(id,canonical,call_ok,call_ok_source,call_event_id) SELECT r.id,r.canonical,r.call_ok,r.call_ok_source,NULL FROM event_rows c JOIN event_rows r ON r.call_id=c.call_id AND r.call_event_id=c.id WHERE c.file_id=?1 AND c.kind IN ('tool_call','server_tool_call') AND r.file_id<>?1", [id])?;
    apply_changes(tx, None)?;
    tx.execute("DELETE FROM files WHERE id=?", [id])?;
    Ok(())
}

/// Marks the native groups and call keys of the rows `rows` selects from
/// `event_rows r`, whose derived columns `settle` recomputes.
pub(super) fn mark(tx: &Transaction<'_>, rows: &str, key: i64) -> Result<()> {
    tx.execute(&format!("INSERT OR IGNORE INTO temp.dirty_groups SELECT s.agent,r.native_id,r.kind FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE {rows} AND r.native_id IS NOT NULL"), [key])?;
    tx.execute(&format!("INSERT OR IGNORE INTO temp.dirty_calls SELECT s.agent,s.session_id,r.call_id FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE {rows} AND r.call_id IS NOT NULL AND r.kind IN ('tool_call','server_tool_call','tool_result')"), [key])?;
    Ok(())
}

/// Recomputes the derived columns of every key a transaction marked from the
/// rows it is about to commit, and moves the rollup contribution of each row
/// before `first`, the transaction's first inserted event, whose values
/// change; the transaction adds its own rows afterwards. Which usage record a
/// request reports contributes to no rollup. Rules: docs/formats.md `canonical_first_copy`,
/// `call_outcome_evidence`, `call_pairing`, `usage_snapshot`, `derived_facts`, and `daily_counts`.
pub(super) fn settle(tx: &Transaction<'_>, first: Option<i64>) -> Result<()> {
    // Each statement starts at the marked keys and reaches rows by ID. Rule:
    // docs/formats.md `derived_work`.
    tx.execute_batch(
        "WITH w AS MATERIALIZED (SELECT d.agent,d.native_id,d.kind,(
           SELECT r2.id FROM event_rows r2 JOIN sessions s2 ON s2.file_id=r2.file_id
           WHERE r2.native_id=d.native_id AND r2.kind=d.kind AND s2.agent=d.agent
           ORDER BY coalesce(s2.started_at,'9999'),s2.path,r2.line_no,r2.ordinal LIMIT 1) AS winner
          FROM temp.dirty_groups d)
         INSERT INTO temp.derived_changes(id,canonical,call_ok,call_ok_source,call_event_id)
          SELECT r.id,NOT r.canonical,r.call_ok,r.call_ok_source,r.call_event_id FROM w CROSS JOIN event_rows r CROSS JOIN sessions s
          WHERE r.native_id=w.native_id AND r.kind=w.kind AND s.file_id=r.file_id AND s.agent=w.agent
           AND r.canonical IS NOT (r.id=w.winner);
         WITH o AS MATERIALIZED (SELECT d.agent,d.session_id,d.call_id,
           CASE WHEN sum(r.ok=0)>0 THEN 0 WHEN sum(r.ok=1)>0 THEN 1 END AS ok,
           CASE WHEN sum(r.ok=0 AND r.ok_source='flag')>0 THEN 'flag'
                WHEN sum(r.ok=0 AND r.ok_source='text')>0 THEN 'text'
                WHEN sum(r.ok_source='flag')>0 THEN 'flag'
                WHEN sum(r.ok_source='text')>0 THEN 'text' ELSE 'none' END AS source
          FROM temp.dirty_calls d LEFT JOIN event_rows r ON r.call_id=d.call_id AND r.kind='tool_result'
           AND r.file_id IN (SELECT file_id FROM sessions WHERE agent=d.agent AND session_id=d.session_id)
          GROUP BY d.agent,d.session_id,d.call_id)
         INSERT INTO temp.derived_changes(id,canonical,call_ok,call_ok_source,call_event_id)
          SELECT c.id,c.canonical,o.ok,o.source,NULL FROM o CROSS JOIN event_rows c CROSS JOIN sessions s
          WHERE c.call_id=o.call_id AND c.kind='tool_call' AND s.file_id=c.file_id
           AND s.agent=o.agent AND s.session_id=o.session_id
           AND (c.call_ok IS NOT o.ok OR c.call_ok_source IS NOT o.source)
          ON CONFLICT(id) DO UPDATE SET call_ok=excluded.call_ok,call_ok_source=excluded.call_ok_source;
         WITH p AS MATERIALIZED (SELECT d.agent,d.session_id,d.call_id,(
           SELECT c.id FROM event_rows c JOIN sessions cs ON cs.file_id=c.file_id
           WHERE c.call_id=d.call_id AND c.kind IN ('tool_call','server_tool_call')
            AND cs.agent=d.agent AND cs.session_id=d.session_id
           ORDER BY cs.path,c.line_no,c.ordinal LIMIT 1) AS call
          FROM temp.dirty_calls d)
         INSERT INTO temp.derived_changes(id,canonical,call_ok,call_ok_source,call_event_id)
          SELECT r.id,r.canonical,r.call_ok,r.call_ok_source,p.call FROM p CROSS JOIN event_rows r CROSS JOIN sessions s
          WHERE r.call_id=p.call_id AND r.kind='tool_result' AND s.file_id=r.file_id
           AND s.agent=p.agent AND s.session_id=p.session_id AND r.call_event_id IS NOT p.call
          ON CONFLICT(id) DO UPDATE SET call_event_id=excluded.call_event_id;
         WITH u AS MATERIALIZED (SELECT d.agent,d.native_id,d.kind,(
           SELECT r2.id FROM event_rows r2 JOIN sessions s2 ON s2.file_id=r2.file_id JOIN usage_rows u2 ON u2.event_id=r2.id
           WHERE r2.native_id=d.native_id AND r2.kind=d.kind AND s2.agent=d.agent
           ORDER BY u2.finished DESC,coalesce(s2.started_at,'9999'),s2.path,r2.line_no DESC,r2.ordinal DESC LIMIT 1) AS winner
          FROM temp.dirty_groups d)
         UPDATE usage_rows SET selected=NOT selected WHERE event_id IN (
          SELECT r.id FROM u CROSS JOIN event_rows r CROSS JOIN usage_rows x CROSS JOIN sessions s
          WHERE u.winner IS NOT NULL AND r.native_id=u.native_id AND r.kind=u.kind AND x.event_id=r.id
           AND s.file_id=r.file_id AND s.agent=u.agent AND x.selected IS NOT (r.id=u.winner));",
    )?;
    apply_changes(tx, first)?;
    tx.execute_batch("DELETE FROM temp.dirty_groups; DELETE FROM temp.dirty_calls;")?;
    Ok(())
}

/// Stores the derived values in `temp.derived_changes`, moving the rollup
/// contribution of each changed row before `first` from its old values to its
/// new ones; rows from `first` on are not counted yet.
fn apply_changes(tx: &Transaction<'_>, first: Option<i64>) -> Result<()> {
    let changed = "e.id IN (SELECT id FROM temp.derived_changes WHERE id<?)";
    let before = [first.unwrap_or(i64::MAX)];
    add_counts(tx, -1, changed, before)?;
    tx.execute_batch(
        "UPDATE event_rows SET (canonical,call_ok,call_ok_source,call_event_id)=(
           SELECT canonical,call_ok,call_ok_source,call_event_id FROM temp.derived_changes c WHERE c.id=event_rows.id)
          WHERE id IN (SELECT id FROM temp.derived_changes);",
    )?;
    add_counts(tx, 1, changed, before)?;
    tx.execute_batch("DELETE FROM temp.derived_changes;")?;
    Ok(())
}
