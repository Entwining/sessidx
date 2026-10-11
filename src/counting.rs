use crate::{
    model::name,
    query::Filters,
    redaction::{redact, redact_metadata},
};
use anyhow::{Context, Result};
use clap::ValueEnum;
use rusqlite::{
    Connection,
    fallible_iterator::FallibleIterator,
    types::{Value, ValueRef},
};
use serde_json::{Value as Json, json};
use std::time::{Duration, Instant};

/// Calls whose canonical event holds a site of `:program`, joined to results
/// instead of probed per result: SQLite 3.53 plans that probe as a scan of the
/// result's whole transcript. Rule: docs/formats.md `count_view_parity`.
const PROGRAM_CALLS: &str = "WITH program_calls AS MATERIALIZED (
 SELECT DISTINCT ce.agent,ce.session_id,ce.call_id FROM commands cp JOIN canonical_events ce ON ce.id=cp.event_id
 WHERE cp.program=:program AND ce.call_id IS NOT NULL) ";

/// A shell site whose program is unknown: unparsed syntax, or a parsed command
/// whose program is not a literal word.
const UNRESOLVED_SITE: &str = "(cp.parsed=0 OR cp.program IS NULL)";

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Metric {
    Commands,
    Failures,
    Denials,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
pub enum By {
    Agent,
    Model,
    Role,
    Week,
    #[value(name = "session_kind")]
    SessionKind,
}

/// One row per group. The counts of a metric partition its population, and
/// what lies outside the population is named apart from it. Rule:
/// docs/formats.md `count_partitions`.
pub fn count(
    db: &Connection,
    metric: Metric,
    by: &[By],
    program: Option<&str>,
    filters: &Filters,
) -> Result<Vec<Json>> {
    let mut keys = Vec::new();
    let mut expressions: Vec<String> = Vec::new();
    for &key in by {
        keys.push(name(key));
        expressions.push(
            match key {
                By::Agent => "s.agent",
                By::Model => "coalesce(e.model,'unknown')",
                By::Role => "e.role",
                By::Week => "coalesce(strftime('%G-W%V',e.ts),'unknown')",
                By::SessionKind => "s.session_kind",
            }
            .into(),
        );
    }
    let (clause, args) = filters.sql(db)?;
    let selected = match (program, metric) {
        (None, _) => "1",
        (Some(_), Metric::Commands) => "c.program=:program",
        (Some(_), Metric::Failures) => {
            "EXISTS(SELECT 1 FROM commands cp WHERE cp.event_id=e.id AND cp.program=:program)"
        }
        (Some(_), Metric::Denials) => "pc.call_id IS NOT NULL",
    };
    let unresolved = match metric {
        Metric::Denials => format!(
            "EXISTS(SELECT 1 FROM sessions cs JOIN event_rows cr ON cr.file_id=cs.file_id JOIN commands cp ON cp.event_id=cr.id WHERE cs.agent=e.agent AND cs.session_id=e.session_id AND cr.call_id=unhex(e.call_id) AND cr.kind!='tool_result' AND {UNRESOLVED_SITE})"
        ),
        _ => format!(
            "EXISTS(SELECT 1 FROM commands cp WHERE cp.event_id=e.id AND {UNRESOLVED_SITE})"
        ),
    };
    let sum = |condition: &str| format!("sum(CASE WHEN {condition} THEN 1 ELSE 0 END)");
    let mut fields: Vec<(&str, String)> = match metric {
        Metric::Commands => vec![
            ("sites", sum("c.parsed=1")),
            (
                "sites_without_program",
                sum("c.parsed=1 AND c.program IS NULL"),
            ),
            (
                "unparsed_calls",
                "count(DISTINCT CASE WHEN c.parsed=0 THEN e.id END)".into(),
            ),
        ],
        // Columns of the `calls` CTE below.
        Metric::Failures => vec![
            ("calls", sum("selected")),
            ("failed", sum("selected AND ok=0")),
            ("succeeded", sum("selected AND ok=1")),
            ("outcome_unknown", sum("selected AND ok IS NULL")),
        ],
        Metric::Denials => vec![
            ("results", sum(selected)),
            (
                "denied",
                sum(&format!(
                    "{selected} AND EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id)"
                )),
            ),
            (
                "denied_reason_unknown",
                sum(&format!(
                    "{selected} AND EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown')"
                )),
            ),
        ],
    };
    if program.is_some() {
        fields.push(match metric {
            Metric::Commands => ("matching_sites", sum("c.parsed=1 AND c.program=:program")),
            Metric::Failures => (
                "calls_with_unresolved_program",
                sum("NOT selected AND unresolved"),
            ),
            Metric::Denials => (
                "results_with_unresolved_program",
                sum(&format!("NOT {selected} AND {unresolved}")),
            ),
        });
    }
    let events = "FROM canonical_events e JOIN sessions s ON s.file_id=e.file_id";
    let (with, source, group_keys) = match metric {
        Metric::Commands => (
            String::new(),
            format!(
                "{events} JOIN commands c ON c.event_id=e.id WHERE {clause} AND e.kind='tool_call'"
            ),
            expressions,
        ),
        // Each selected call's outcome is evaluated once, in this CTE.
        // Rule: docs/formats.md `call_outcome_probe`.
        Metric::Failures => {
            let aliases: Vec<String> = (0..expressions.len()).map(|i| format!("k{i}")).collect();
            let mut evaluated: Vec<String> = expressions
                .iter()
                .zip(&aliases)
                .map(|(e, k)| format!("{e} AS {k}"))
                .collect();
            evaluated.push(format!("{selected} AS selected"));
            if program.is_some() {
                evaluated.push(format!("{unresolved} AS unresolved"));
            }
            evaluated.push(format!("CASE WHEN {selected} THEN e.ok END AS ok"));
            (
                format!(
                    "WITH calls AS MATERIALIZED (SELECT {} FROM call_outcomes e JOIN sessions s ON s.file_id=e.file_id WHERE {clause}) ",
                    evaluated.join(",")
                ),
                "FROM calls".to_owned(),
                aliases,
            )
        }
        Metric::Denials if program.is_some() => (
            PROGRAM_CALLS.to_owned(),
            format!(
                "{events} LEFT JOIN program_calls pc ON pc.agent=e.agent AND pc.session_id=e.session_id AND pc.call_id=e.call_id WHERE {clause} AND e.kind='tool_result'"
            ),
            expressions,
        ),
        Metric::Denials => (
            String::new(),
            format!("{events} WHERE {clause} AND e.kind='tool_result'"),
            expressions,
        ),
    };
    let group = if group_keys.is_empty() {
        String::new()
    } else {
        format!(
            " GROUP BY {} ORDER BY {}",
            group_keys.join(","),
            group_keys.join(",")
        )
    };
    let columns = group_keys
        .iter()
        .cloned()
        .chain(fields.iter().map(|(_, sql)| format!("coalesce({sql},0)")))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("{with}SELECT {columns} {source}{group}");
    let mut stmt = db.prepare(&sql)?;
    let mut args = args.into_iter();
    for i in 1..=stmt.parameter_count() {
        let value = if stmt.parameter_name(i) == Some(":program") {
            Value::Text(program.context("program parameter missing")?.into())
        } else {
            args.next().context("filter parameter missing")?
        };
        stmt.raw_bind_parameter(i, value)?;
    }
    let values = stmt
        .raw_query()
        .mapped(|r| {
            let mut obj = serde_json::Map::new();
            for (i, key) in keys.iter().enumerate() {
                obj.insert(key.clone(), json!(redact_metadata(&r.get::<_, String>(i)?)));
            }
            obj.insert("metric".into(), json!(name(metric)));
            if let Some(program) = program {
                obj.insert("program".into(), json!(redact(program)));
            }
            for (i, (field, _)) in fields.iter().enumerate() {
                obj.insert((*field).into(), json!(r.get::<_, i64>(keys.len() + i)?));
            }
            Ok(Json::Object(obj))
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
}

/// Each count is named after the population it counts, so that a reader can
/// compare it with the population it belongs to.
pub fn doctor(db: &Connection) -> Result<Json> {
    let scalar = |sql: &str| -> Result<i64> { Ok(db.query_row(sql, [], |r| r.get(0))?) };
    let signatures: Vec<Json> = db
        .prepare("SELECT signature,sum(n) FROM shapes WHERE known=0 GROUP BY signature ORDER BY sum(n) DESC LIMIT 100")?
        .query_map([], |r| {
            Ok(json!({"signature": redact(&r.get::<_, String>(0)?), "records": r.get::<_, i64>(1)?}))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(json!({
        "files": scalar("SELECT count(*) FROM files")?,
        "incomplete_files": scalar("SELECT count(*) FROM files WHERE index_status!='ready'")?,
        "parse_errors": scalar("SELECT coalesce(sum(parse_errors),0) FROM files")?,
        "records": scalar("SELECT coalesce(sum(n),0) FROM shapes")?,
        "records_with_unknown_shape": scalar("SELECT coalesce(sum(n),0) FROM shapes WHERE known=0")?,
        "unknown_shape_signatures": signatures,
        "sessions": scalar("SELECT count(*) FROM sessions")?,
        "sessions_with_unknown_kind": scalar("SELECT count(*) FROM sessions WHERE session_kind='unknown'")?,
        "events": scalar("SELECT count(*) FROM event_rows")?,
        "canonical_events": scalar("SELECT count(*) FROM canonical_events")?,
        "tool_events": scalar("SELECT count(*) FROM event_rows WHERE kind IN ('tool_call','server_tool_call','tool_result')")?,
        "tool_events_without_model": scalar("SELECT count(*) FROM event_rows WHERE kind IN ('tool_call','server_tool_call','tool_result') AND model IS NULL")?,
        "shell_calls": scalar("SELECT count(DISTINCT event_id) FROM commands")?,
        "shell_calls_unparsed": scalar("SELECT count(DISTINCT event_id) FROM commands WHERE parsed=0")?,
        "denials": scalar("SELECT count(*) FROM denials")?,
        "denials_with_unknown_reason": scalar("SELECT count(*) FROM denials WHERE reason_id='unknown'")?,
    }))
}

pub fn sql(db: &Connection, sql: &str) -> Result<Vec<Json>> {
    db.execute_batch("PRAGMA query_only=ON;")?;
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))?;
    let result = (|| {
        let mut batch = rusqlite::Batch::new(db, sql);
        let mut stmt = batch.next()?.ok_or_else(|| anyhow::anyhow!("empty SQL"))?;
        anyhow::ensure!(batch.next()?.is_none(), "sql accepts exactly one statement");
        anyhow::ensure!(stmt.readonly(), "sql accepts one read-only SELECT");
        let first = sql
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        anyhow::ensure!(
            first == "SELECT" || first == "WITH",
            "sql accepts one read-only SELECT"
        );
        let names = stmt
            .column_names()
            .iter()
            .map(|s| redact(s))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            names.iter().collect::<std::collections::HashSet<_>>().len() == names.len(),
            "duplicate SQL column names after redaction; alias each column uniquely"
        );
        let mut rows = stmt.query([])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            anyhow::ensure!(
                result.len() < 10_000 && Instant::now() < deadline,
                "SQL result exceeded 10000 rows or two seconds; add LIMIT or narrower predicates"
            );
            let mut obj = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let value = match row.get_ref(i)? {
                    ValueRef::Null => Json::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(v) => json!(redact(&String::from_utf8_lossy(v))),
                    ValueRef::Blob(_) => json!("[blob omitted]"),
                };
                obj.insert(name.clone(), value);
            }
            result.push(Json::Object(obj));
        }
        Ok(result)
    })();
    db.progress_handler(0, None::<fn() -> bool>)?;
    db.execute_batch("PRAGMA query_only=OFF;")?;
    result.map_err(|e| match e.downcast_ref::<rusqlite::Error>() {
        Some(rusqlite::Error::SqliteFailure(f, _))
            if f.code == rusqlite::ErrorCode::OperationInterrupted =>
        {
            anyhow::anyhow!(
                "SQL exceeded two seconds; narrow it by session_id or ts, or aggregate fewer rows"
            )
        }
        _ => e,
    })
}
