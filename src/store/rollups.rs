use anyhow::Result;
use rusqlite::Transaction;

/// A table of per-transcript sums by UTC day that the writer keeps equal to
/// the sums over its events: each transaction adds the contribution of the
/// rows it inserts once their derived columns are settled, and subtracts and
/// re-adds that of each earlier row whose derived columns change. Rule:
/// docs/formats.md `daily_counts`.
pub struct Rollup {
    pub table: &'static str,
    keys: &'static [&'static str],
    pub counters: &'static [&'static str],
    events: fn(&str) -> String,
}

pub const DAILY_COUNTS: Rollup = Rollup {
    table: "daily_counts",
    keys: &["file_id", "day", "kind", "role", "model", "canonical"],
    counters: &[
        "events",
        "failed",
        "succeeded",
        "denied",
        "denied_reason_unknown",
        "denial_rows",
        "denial_rows_reason_unknown",
        "sites",
        "sites_without_program",
        "shell_calls",
        "unparsed_calls",
        "unresolved_program",
    ],
    events: event_counts,
};

pub const PROGRAM_COUNTS: Rollup = Rollup {
    table: "daily_program_counts",
    keys: &["file_id", "day", "kind", "role", "model", "program"],
    counters: &[
        "events",
        "failed",
        "succeeded",
        "denied",
        "denied_reason_unknown",
        "unresolved_program",
        "matching_sites",
    ],
    events: program_counts,
};

/// The event whose shell sites hold a tool call's or tool result's program.
const SITES_OF: &str =
    "CASE e.kind WHEN 'tool_call' THEN e.id WHEN 'tool_result' THEN e.call_event_id END";

/// Each event's contribution to `daily_counts`, for the events `selection`
/// picks from `event_rows e`. Grouping by `+e.id` rather than `e.id` keeps
/// SQLite 3.53 from scanning every event in row-ID order to skip the sort,
/// instead of reaching the selection by its index.
fn event_counts(selection: &str) -> String {
    format!(
        "SELECT e.file_id,substr(e.ts,1,10) AS day,e.kind,e.role,e.model,e.canonical,1 AS events,
          e.call_ok IS 0 AS failed,e.call_ok IS 1 AS succeeded,
          EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id) AS denied,
          EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown') AS denied_reason_unknown,
          (SELECT count(*) FROM denials d WHERE d.event_id=e.id) AS denial_rows,
          (SELECT count(*) FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown') AS denial_rows_reason_unknown,
          coalesce(sum(c.parsed=1),0) AS sites,coalesce(sum(c.parsed=1 AND c.program IS NULL),0) AS sites_without_program,
          count(c.id)>0 AS shell_calls,coalesce(max(c.parsed=0),0) AS unparsed_calls,
          EXISTS(SELECT 1 FROM commands u WHERE u.event_id={SITES_OF} AND (u.parsed=0 OR u.program IS NULL)) AS unresolved_program
         FROM event_rows e LEFT JOIN commands c ON c.event_id=e.id WHERE {selection} GROUP BY +e.id"
    )
}

/// Each first-copy event's contribution to `daily_program_counts`, once per
/// program its call's sites name, for the events `selection` picks from
/// `event_rows e` joined to those sites as `commands c`.
fn program_counts(selection: &str) -> String {
    format!(
        "SELECT e.file_id,substr(e.ts,1,10) AS day,e.kind,e.role,e.model,c.program,1 AS events,
          e.call_ok IS 0 AS failed,e.call_ok IS 1 AS succeeded,
          EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id) AS denied,
          EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown') AS denied_reason_unknown,
          EXISTS(SELECT 1 FROM commands u WHERE u.event_id=c.event_id AND (u.parsed=0 OR u.program IS NULL)) AS unresolved_program,
          CASE WHEN e.kind='tool_call' THEN count(*) ELSE 0 END AS matching_sites
         FROM event_rows e JOIN commands c ON c.event_id={SITES_OF}
         WHERE {selection} AND e.canonical=1 AND c.program IS NOT NULL GROUP BY e.id,c.program"
    )
}

impl Rollup {
    /// The rollup's rows for the events `selection` picks, as the table holds them.
    pub fn grouped(&self, selection: &str) -> String {
        let keys = self.keys.join(",");
        format!(
            "SELECT {keys},{} FROM ({}) GROUP BY {keys}",
            self.counters
                .iter()
                .map(|c| format!("sum({c}) AS {c}"))
                .collect::<Vec<_>>()
                .join(","),
            (self.events)(selection)
        )
    }

    /// Adds `sign` times the contribution of the selected events, and drops
    /// the rows left empty.
    fn add(
        &self,
        tx: &Transaction<'_>,
        sign: i64,
        selection: &str,
        params: impl rusqlite::Params,
    ) -> Result<()> {
        let table = self.table;
        let keys = self.keys.join(",");
        // An empty blob, which no text equals, keys an absent day or model.
        let conflict = self
            .keys
            .iter()
            .map(|&k| match k {
                "day" | "model" => format!("ifnull({k},x'')"),
                _ => k.to_owned(),
            })
            .collect::<Vec<_>>()
            .join(",");
        tx.prepare_cached(&format!(
            "INSERT INTO {table} SELECT {keys},{} FROM ({}) WHERE true
             ON CONFLICT({conflict}) DO UPDATE SET {}",
            self.counters
                .iter()
                .map(|c| format!("{sign}*{c}"))
                .collect::<Vec<_>>()
                .join(","),
            self.grouped(selection),
            self.counters
                .iter()
                .map(|c| format!("{c}={c}+excluded.{c}"))
                .collect::<Vec<_>>()
                .join(","),
        ))?
        .execute(params)?;
        tx.prepare_cached(&format!("DELETE FROM {table} WHERE events=0"))?
            .execute([])?;
        Ok(())
    }
}

/// Adds `sign` times the contribution of the selected events to every rollup.
pub(super) fn add_counts<P: rusqlite::Params + Copy>(
    tx: &Transaction<'_>,
    sign: i64,
    selection: &str,
    params: P,
) -> Result<()> {
    DAILY_COUNTS.add(tx, sign, selection, params)?;
    PROGRAM_COUNTS.add(tx, sign, selection, params)
}
