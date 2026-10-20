use crate::{
    model::name,
    query::Filters,
    store::{DAILY_COUNTS, PROGRAM_COUNTS, Rollup},
};
use anyhow::Result;
use chrono::{NaiveDate, NaiveTime};
use clap::ValueEnum;
use rusqlite::{
    Connection,
    fallible_iterator::FallibleIterator,
    types::{Value, ValueRef},
};
use serde_json::{Value as Json, json};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
pub enum Metric {
    Commands,
    Failures,
    Denials,
    Usage,
    Compactions,
}

impl Metric {
    /// Model requests and compactions belong to no speaker and run no
    /// program.
    fn of_events(self) -> bool {
        !matches!(self, Self::Usage | Self::Compactions)
    }

    /// The grouping `--by` takes when it is not given.
    pub fn default_by(self) -> Vec<By> {
        if self.of_events() {
            vec![By::Agent, By::Model, By::Role, By::Week]
        } else {
            vec![By::Agent, By::Model, By::Week]
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
pub enum By {
    Agent,
    Model,
    Role,
    Week,
    #[value(name = "session_kind")]
    SessionKind,
}

/// Refuses options that do not apply to a metric, before any refresh.
pub fn check(metric: Metric, by: &[By], program: Option<&str>, filters: &Filters) -> Result<()> {
    if let Some(repeated) = by
        .iter()
        .enumerate()
        .find_map(|(i, key)| by[..i].contains(key).then_some(*key))
    {
        anyhow::bail!("--by repeats {}; name each grouping once", name(repeated));
    }
    if !metric.of_events() {
        let option = if by.contains(&By::Role) {
            Some("--by role")
        } else if filters.role.is_some() {
            Some("--role")
        } else if program.is_some() {
            Some("--program")
        } else {
            None
        };
        if let Some(option) = option {
            anyhow::bail!(
                "{option} applies to count commands, failures, and denials, not {}",
                name(metric)
            );
        }
    }
    Ok(())
}

/// One row per group. The counts of a metric partition its population, and
/// what lies outside the population is named apart from it. Rule:
/// docs/formats.md `count_partitions`.
///
/// Counts come from `daily_counts`, and with a program from
/// `daily_program_counts` too, for the whole UTC days inside the window, and
/// from the events themselves for the part of a day at either edge. Rule:
/// docs/formats.md `daily_counts`.
pub fn count(
    db: &Connection,
    metric: Metric,
    by: &[By],
    program: Option<&str>,
    filters: &Filters,
) -> Result<Vec<Json>> {
    check(metric, by, program, filters)?;
    // Columns named `program_*` are the program's rows; the others, every
    // event's.
    let (kind, mut fields): (&str, Vec<(&str, &str)>) = match (metric, program.is_some()) {
        (Metric::Commands, _) => (
            "tool_call",
            vec![
                ("sites", "sum(x.sites)"),
                ("sites_without_program", "sum(x.sites_without_program)"),
                ("unparsed_calls", "sum(x.unparsed_calls)"),
            ],
        ),
        (Metric::Failures, false) => (
            "tool_call",
            vec![
                ("calls", "sum(x.events)"),
                ("failed", "sum(x.failed)"),
                ("succeeded", "sum(x.succeeded)"),
                ("outcome_unknown", "sum(x.events-x.failed-x.succeeded)"),
            ],
        ),
        (Metric::Failures, true) => (
            "tool_call",
            vec![
                ("calls", "sum(x.program_events)"),
                ("failed", "sum(x.program_failed)"),
                ("succeeded", "sum(x.program_succeeded)"),
                (
                    "outcome_unknown",
                    "sum(x.program_events-x.program_failed-x.program_succeeded)",
                ),
            ],
        ),
        (Metric::Denials, false) => (
            "tool_result",
            vec![
                ("results", "sum(x.events)"),
                ("denied", "sum(x.denied)"),
                ("denied_reason_unknown", "sum(x.denied_reason_unknown)"),
            ],
        ),
        (Metric::Denials, true) => (
            "tool_result",
            vec![
                ("results", "sum(x.program_events)"),
                ("denied", "sum(x.program_denied)"),
                (
                    "denied_reason_unknown",
                    "sum(x.program_denied_reason_unknown)",
                ),
            ],
        ),
        (Metric::Usage | Metric::Compactions, _) => return count_records(db, metric, by, filters),
    };
    // Events whose program is unknown, less those that also name the program.
    let unresolved = "sum(x.unresolved_program-x.program_unresolved_program)";
    if program.is_some() {
        fields.push(match metric {
            Metric::Commands => ("matching_sites", "sum(x.program_matching_sites)"),
            Metric::Failures => ("calls_with_unresolved_program", unresolved),
            Metric::Denials => ("results_with_unresolved_program", unresolved),
            Metric::Usage | Metric::Compactions => unreachable!("check refuses --program"),
        });
    }
    // Whole days run from the first midnight at or after --since to the last
    // at or before --until; a bound already at midnight leaves an empty edge.
    let whole_from = filters.since.map(|t| {
        let day = t.date_naive();
        if t.time() == NaiveTime::MIN {
            day
        } else {
            day + chrono::Days::new(1)
        }
    });
    let whole_to = filters.until.map(|t| t.date_naive());
    let stamp = |day: NaiveDate| day.format("%Y-%m-%dT00:00:00.000Z").to_string();
    let mut whole_days = None;
    let mut edges = Vec::new();
    if let (Some(from), Some(to)) = (whole_from, whole_to)
        && from >= to
    {
        edges.push((filters.since(), filters.until()));
    } else {
        whole_days = Some((whole_from, whole_to));
        if let (Some(since), Some(from)) = (filters.since(), whole_from) {
            edges.push((Some(since), Some(stamp(from))));
        }
        if let (Some(until), Some(to)) = (filters.until(), whole_to) {
            edges.push((Some(stamp(to)), Some(until)));
        }
    }
    // The program's rollup follows every event's, and its columns are
    // prefixed `program_`.
    let rollups: Vec<(&Rollup, Option<&str>)> = std::iter::once((&DAILY_COUNTS, None))
        .chain(program.map(|p| (&PROGRAM_COUNTS, Some(p))))
        .collect();
    let mut parts = Vec::new();
    let mut args = Vec::new();
    for (i, &(rollup, program)) in rollups.iter().enumerate() {
        // Each part has every rollup's counters, zero for the others'.
        let columns = rollups
            .iter()
            .enumerate()
            .flat_map(|(j, (other, _))| {
                other.counters.iter().map(move |c| {
                    let prefix = if j == 0 { "" } else { "program_" };
                    let value = if i == j { c } else { "0" };
                    format!("{value} AS {prefix}{c}")
                })
            })
            .collect::<Vec<_>>()
            .join(",");
        if let Some((from, to)) = whole_days {
            let mut days = vec!["kind=?".to_owned()];
            args.push(Value::Text(kind.into()));
            if program.is_none() {
                days.push("canonical=1".into());
            }
            if let Some(program) = program {
                days.push("program=?".into());
                args.push(Value::Text(program.into()));
            }
            for (op, day) in [(">=", from), ("<", to)] {
                if let Some(day) = day {
                    days.push(format!("day{op}?"));
                    args.push(Value::Text(day.format("%Y-%m-%d").to_string()));
                }
            }
            parts.push(format!(
                "SELECT file_id,day,role,model,{columns} FROM {} WHERE {}",
                rollup.table,
                days.join(" AND ")
            ));
        }
        for (since, until) in &edges {
            let mut selection = vec!["e.kind=?".to_owned(), "e.canonical=1".to_owned()];
            args.push(Value::Text(kind.into()));
            if let Some(program) = program {
                selection.push("c.program=?".into());
                args.push(Value::Text(program.into()));
            }
            for (op, value) in [(">=", since), ("<", until)] {
                if let Some(value) = value {
                    selection.push(format!("e.ts{op}?"));
                    args.push(Value::Text(value.clone()));
                }
            }
            parts.push(format!(
                "SELECT file_id,day,role,model,{columns} FROM ({})",
                rollup.grouped(&selection.join(" AND "))
            ));
        }
    }
    let (sessions, session_args) = filters.session_sql(db)?;
    args.extend(session_args);
    let mut conditions = vec![sessions];
    if let Some(role) = filters.role {
        conditions.push("x.role=?".into());
        args.push(Value::Text(name(role)));
    }
    if let Metric::Commands = metric {
        // A group exists where a shell call does, as in a join with
        // `commands`, and every row of a program is one.
        conditions.push(if program.is_some() {
            "(x.shell_calls>0 OR x.program_events>0)".into()
        } else {
            "x.shell_calls>0".into()
        });
    }
    let from = format!(
        "({}) x JOIN sessions s ON s.file_id=x.file_id WHERE {}",
        parts.join(" UNION ALL "),
        conditions.join(" AND ")
    );
    grouped(db, metric, by, program, &fields, &from, args)
}

/// `count usage` and `count compactions`, summed over the public views. A
/// request counts once, under its own model; its tokens count per pass, under
/// the pass's model. Rule: docs/formats.md `count_usage`.
fn count_records(
    db: &Connection,
    metric: Metric,
    by: &[By],
    filters: &Filters,
) -> Result<Vec<Json>> {
    let (window, window_args) = filters.event_sql();
    let tokens = [
        "input",
        "cache_read",
        "cache_write",
        "cache_write_5m",
        "cache_write_1h",
        "output",
        "reasoning",
    ];
    let (source, fields, mut args): (String, Vec<(String, String)>, Vec<Value>) = match metric {
        Metric::Usage => {
            // A request counts on the pass row that is the request itself, or
            // on a row of its own when its record lists passes, so that the
            // sort reads one row per pass rather than one more per request.
            let nulls = tokens.map(|t| format!("NULL AS {t}")).join(",");
            let source = format!(
                "SELECT file_id,substr(ts,1,10) AS day,model,kind IS NULL AS requests,1 AS passes,{} FROM request_passes e WHERE {window}
                 UNION ALL SELECT file_id,substr(ts,1,10),model,1,0,{nulls} FROM requests e WHERE {window}
                  AND EXISTS(SELECT 1 FROM usage_passes p WHERE p.event_id=e.id)",
                tokens.join(",")
            );
            let mut fields = vec![
                ("requests".to_owned(), "sum(x.requests)".to_owned()),
                ("passes".to_owned(), "sum(x.passes)".to_owned()),
            ];
            fields.extend(tokens.map(|t| (t.to_owned(), format!("sum(x.{t})"))));
            // Input and output are recorded by every pass.
            fields.extend(
                tokens
                    .iter()
                    .filter(|t| !matches!(**t, "input" | "output"))
                    .map(|t| (format!("{t}_recorded"), format!("count(x.{t})"))),
            );
            let args = window_args.iter().chain(&window_args).cloned().collect();
            (source, fields, args)
        }
        Metric::Compactions => (
            format!(
                "SELECT file_id,substr(ts,1,10) AS day,model,trigger,pre_tokens,post_tokens FROM compactions e WHERE {window}"
            ),
            [
                ("compactions", "count(*)"),
                ("auto", "sum(x.trigger='auto')"),
                ("manual", "sum(x.trigger='manual')"),
                ("trigger_unknown", "sum(x.trigger='unknown')"),
                ("pre_tokens", "sum(x.pre_tokens)"),
                ("pre_tokens_recorded", "count(x.pre_tokens)"),
                ("post_tokens", "sum(x.post_tokens)"),
                ("post_tokens_recorded", "count(x.post_tokens)"),
            ]
            .map(|(field, sql)| (field.to_owned(), sql.to_owned()))
            .into(),
            window_args,
        ),
        Metric::Commands | Metric::Failures | Metric::Denials => {
            unreachable!("counted from the rollups")
        }
    };
    let (sessions, session_args) = filters.session_sql(db)?;
    args.extend(session_args);
    // The records drive the join: joined freely, SQLite indexed the whole
    // union to probe it per session. Rule: docs/formats.md `usage_count_work`.
    let from =
        format!("({source}) x CROSS JOIN sessions s ON s.file_id=x.file_id WHERE {sessions}");
    grouped(db, metric, by, None, &fields, &from, args)
}

/// One record per group of the rows `x` and their sessions `s` that `from`
/// selects, with each field summed over the group.
fn grouped<F: AsRef<str>, S: AsRef<str>>(
    db: &Connection,
    metric: Metric,
    by: &[By],
    program: Option<&str>,
    fields: &[(F, S)],
    from: &str,
    args: Vec<Value>,
) -> Result<Vec<Json>> {
    let keys: Vec<String> = by.iter().map(|&key| name(key)).collect();
    let expressions: Vec<&str> = by
        .iter()
        .map(|key| match key {
            By::Agent => "s.agent",
            By::Model => "coalesce(x.model,'unknown')",
            By::Role => "x.role",
            By::Week => "coalesce(strftime('%G-W%V',x.day),'unknown')",
            By::SessionKind => "s.session_kind",
        })
        .collect();
    let group = if expressions.is_empty() {
        String::new()
    } else {
        format!(
            " GROUP BY {} ORDER BY {}",
            expressions.join(","),
            expressions.join(",")
        )
    };
    let columns = expressions
        .iter()
        .map(|e| (*e).to_owned())
        .chain(
            fields
                .iter()
                .map(|(_, sql)| format!("coalesce({},0)", sql.as_ref())),
        )
        .collect::<Vec<_>>()
        .join(",");
    let values = db
        .prepare(&format!("SELECT {columns} FROM {from}{group}"))?
        .query_map(rusqlite::params_from_iter(args), |r| {
            let mut obj = serde_json::Map::new();
            for (i, key) in keys.iter().enumerate() {
                obj.insert(key.clone(), json!(r.get::<_, String>(i)?));
            }
            obj.insert("metric".into(), json!(name(metric)));
            if let Some(program) = program {
                obj.insert("program".into(), json!(program));
            }
            for (i, (field, _)) in fields.iter().enumerate() {
                obj.insert(
                    field.as_ref().into(),
                    json!(r.get::<_, i64>(keys.len() + i)?),
                );
            }
            Ok(Json::Object(obj))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
}

/// Each count is named after the population it counts, so that a reader can
/// compare it with the population it belongs to. Event counts come from
/// `daily_counts`.
pub fn doctor(db: &Connection) -> Result<Json> {
    let scalar = |sql: &str| -> Result<i64> { Ok(db.query_row(sql, [], |r| r.get(0))?) };
    Ok(json!({
        "files": scalar("SELECT count(*) FROM files")?,
        "incomplete_files": scalar("SELECT count(*) FROM files WHERE index_status!='ready'")?,
        "parse_errors": scalar("SELECT coalesce(sum(parse_errors),0) FROM files")?,
        "records": scalar("SELECT coalesce(sum(lines_indexed),0) FROM files")?,
        "sessions": scalar("SELECT count(*) FROM sessions")?,
        "sessions_with_unknown_kind": scalar("SELECT count(*) FROM sessions WHERE session_kind='unknown'")?,
        "events": scalar("SELECT coalesce(sum(events),0) FROM daily_counts")?,
        "canonical_events": scalar("SELECT coalesce(sum(events),0) FROM daily_counts WHERE canonical=1")?,
        "tool_events": scalar("SELECT coalesce(sum(events),0) FROM daily_counts WHERE kind IN ('tool_call','server_tool_call','tool_result')")?,
        "tool_events_without_model": scalar("SELECT coalesce(sum(events),0) FROM daily_counts WHERE kind IN ('tool_call','server_tool_call','tool_result') AND model IS NULL")?,
        "shell_calls": scalar("SELECT coalesce(sum(shell_calls),0) FROM daily_counts")?,
        "shell_calls_unparsed": scalar("SELECT coalesce(sum(unparsed_calls),0) FROM daily_counts")?,
        "denials": scalar("SELECT coalesce(sum(denial_rows),0) FROM daily_counts")?,
        "denials_with_unknown_reason": scalar("SELECT coalesce(sum(denial_rows_reason_unknown),0) FROM daily_counts")?,
    }))
}

pub fn sql(db: &Connection, sql: &str) -> Result<Vec<Json>> {
    db.execute_batch("PRAGMA query_only=ON;")?;
    let deadline = Instant::now() + crate::BUDGET;
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
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            names.iter().collect::<std::collections::HashSet<_>>().len() == names.len(),
            "duplicate SQL column names; alias each column uniquely"
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
                    ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
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
