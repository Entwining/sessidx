use crate::{
    model::name,
    query::Filters,
    redaction::{redact, redact_metadata},
};
use anyhow::{Context, Result};
use clap::ValueEnum;
use rusqlite::{
    Connection,
    types::{Value, ValueRef},
};
use serde_json::{Value as Json, json};
use std::time::{Duration, Instant};

/// `canonical_events` and `call_outcomes` with the views' columns and rules,
/// limited to tool calls and results before ranking copies. Through the views,
/// every count joined the strings of all events before the kind filter applied.
/// Adapters attach shell sites only to local and server tool calls, so
/// `--program` lookups see every site.
/// `session_ref` and `call_ref` let call lookups use the `events_call` index;
/// matching them through the views scanned every event of the session.
const TOOL_EVENTS: &str = "WITH canonical_events AS MATERIALIZED (
 SELECT c.id,c.file_id,c.session_ref,session.value AS session_id,c.ts,model.value AS model,coalesce(role.value,'unknown') AS role,kind.value AS kind,
 CASE WHEN c.call_id IS NULL THEN NULL ELSE lower(hex(c.call_id)) END AS call_id,c.call_id AS call_ref,c.ok
 FROM (SELECT l.id,l.file_id,l.session_ref,l.ts,l.model_ref,d.role_ref,d.kind_ref,d.call_id,d.ok,row_number() OVER (
 PARTITION BY f.harness,coalesce(l.native_id,l.id),d.kind_ref ORDER BY coalesce(f.first_ts,'9999'),f.path,l.line_no,l.ordinal) AS copy_rank
 FROM event_details d CROSS JOIN locations l ON l.id=d.event_id JOIN files f ON f.id=l.file_id
 WHERE d.kind_ref IN (SELECT id FROM strings WHERE value IN ('tool_call','server_tool_call','tool_result'))) c
 JOIN strings session ON session.id=c.session_ref LEFT JOIN strings model ON model.id=c.model_ref
 LEFT JOIN strings role ON role.id=c.role_ref JOIN strings kind ON kind.id=c.kind_ref
 WHERE c.copy_rank=1),
call_outcomes AS MATERIALIZED (
 SELECT f.harness,e.session_id,e.call_id,CASE WHEN sum(ok=0)>0 THEN 0 WHEN sum(ok=1)>0 THEN 1 END AS ok
 FROM canonical_events e JOIN files f ON f.id=e.file_id WHERE e.kind='tool_result' AND e.call_id IS NOT NULL GROUP BY f.harness,e.session_id,e.call_id)";

/// Calls whose canonical event holds a site of `:program`, joined to results
/// instead of probed per result: with a GROUP BY, SQLite 3.46 plans that probe
/// as a scan of `canonical_events` for every result.
const PROGRAM_CALLS: &str = ",
program_calls AS MATERIALIZED (
 SELECT DISTINCT cf.harness,ce.session_ref,ce.call_ref FROM commands cp JOIN canonical_events ce ON ce.id=cp.event_id JOIN files cf ON cf.id=ce.file_id
 WHERE cp.program=:program AND ce.call_ref IS NOT NULL)";

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Metric {
    Commands,
    Failures,
    Denials,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
pub enum By {
    Harness,
    Model,
    Role,
    Week,
    Kind,
}

pub fn count(
    db: &Connection,
    metric: Metric,
    by: &[By],
    program: Option<&str>,
    filters: &Filters,
) -> Result<Vec<Json>> {
    let mut keys = Vec::new();
    let mut expressions = Vec::new();
    for &key in by {
        keys.push(name(key));
        expressions.push(match key {
            By::Harness => "f.harness",
            By::Model => "coalesce(e.model,'unknown')",
            By::Role => "e.role",
            By::Week => "coalesce(strftime('%G-W%V',e.ts),'unknown')",
            By::Kind => "s.kind",
        });
    }
    let (clause, args) = filters.sql(db)?;
    let denials_by_program = program.is_some() && matches!(metric, Metric::Denials);
    let selected = if program.is_some() {
        match metric {
            Metric::Commands => "c.program=:program",
            Metric::Failures => {
                "EXISTS(SELECT 1 FROM commands cp WHERE cp.event_id=e.id AND cp.program=:program)"
            }
            Metric::Denials => "pc.call_ref IS NOT NULL",
        }
    } else {
        "1=1"
    };
    let unknown_program = if program.is_none() {
        "0"
    } else if denials_by_program {
        "EXISTS(SELECT 1 FROM event_details cd JOIN commands cp ON cp.event_id=cd.event_id JOIN locations cl ON cl.id=cd.event_id JOIN files cf ON cf.id=cl.file_id WHERE cd.session_ref=e.session_ref AND cd.call_id=e.call_ref AND cf.harness=f.harness AND (cp.parsed=0 OR cp.program IS NULL))"
    } else {
        "EXISTS(SELECT 1 FROM commands cp WHERE cp.event_id=e.id AND (cp.parsed=0 OR cp.program IS NULL))"
    };
    let selected_denominator = format!("sum(CASE WHEN {selected} THEN 1 ELSE 0 END)");
    let unknown_outcomes = format!(
        "sum(CASE WHEN ({selected} AND o.ok IS NULL) OR {unknown_program} THEN 1 ELSE 0 END)"
    );
    let unknown_denials = format!(
        "sum(CASE WHEN ({selected} AND EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown')) OR {unknown_program} THEN 1 ELSE 0 END)"
    );
    let (unit, numerator, denominator, unclassified, unclassified_unit, join, kind) = match metric {
        Metric::Commands => (
            "static_shell_command_sites",
            format!("sum(CASE WHEN c.parsed=1 AND {selected} THEN 1 ELSE 0 END)"),
            "sum(c.parsed=1)",
            "sum(c.parsed=0 OR c.program IS NULL)",
            "shell_calls_or_sites_without_classification",
            "JOIN commands c ON c.event_id=e.id",
            "tool_call",
        ),
        Metric::Failures => (
            "tool_call_attempts",
            format!("sum(CASE WHEN o.ok=0 AND {selected} THEN 1 ELSE 0 END)"),
            selected_denominator.as_str(),
            unknown_outcomes.as_str(),
            "tool_call_attempts_without_outcome_or_program",
            "LEFT JOIN call_outcomes o ON o.harness=f.harness AND o.session_id=e.session_id AND o.call_id=e.call_id",
            "tool_call",
        ),
        Metric::Denials => (
            "tool_result_events",
            format!(
                "sum(CASE WHEN EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id) AND {selected} THEN 1 ELSE 0 END)"
            ),
            selected_denominator.as_str(),
            unknown_denials.as_str(),
            "tool_result_events_with_unknown_denial_reason_or_program",
            if denials_by_program {
                "LEFT JOIN program_calls pc ON pc.harness=f.harness AND pc.session_ref=e.session_ref AND pc.call_ref=e.call_ref"
            } else {
                ""
            },
            "tool_result",
        ),
    };
    let prefix = if expressions.is_empty() {
        String::new()
    } else {
        expressions.join(",") + ","
    };
    let group = if expressions.is_empty() {
        String::new()
    } else {
        format!(
            " GROUP BY {} ORDER BY {}",
            expressions.join(","),
            expressions.join(",")
        )
    };
    let program_calls = if denials_by_program {
        PROGRAM_CALLS
    } else {
        ""
    };
    let sql = format!(
        "{TOOL_EVENTS}{program_calls} SELECT {prefix}coalesce({numerator},0),coalesce({denominator},0),coalesce({unclassified},0) FROM canonical_events e JOIN files f ON f.id=e.file_id JOIN sessions s ON s.file_id=f.id {join} WHERE {clause} AND e.kind='{kind}'{group}"
    );
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
            obj.insert("unit".into(), json!(unit));
            if let Some(program) = program {
                obj.insert("program".into(), json!(redact(program)));
            }
            for (i, key) in ["numerator", "denominator", "unclassified"]
                .iter()
                .enumerate()
            {
                obj.insert((*key).into(), json!(r.get::<_, i64>(keys.len() + i)?));
            }
            obj.insert("unclassified_unit".into(), json!(unclassified_unit));
            Ok(Json::Object(obj))
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
}

pub fn doctor(db: &Connection) -> Result<Json> {
    let scalar = |sql: &str| -> Result<i64> { Ok(db.query_row(sql, [], |r| r.get(0))?) };
    let shapes = scalar("SELECT coalesce(sum(n),0) FROM shapes")?;
    let unknown = scalar("SELECT coalesce(sum(n),0) FROM shapes WHERE known=0")?;
    let shell = scalar("SELECT count(DISTINCT event_id) FROM commands")?;
    let unparsed = scalar("SELECT count(DISTINCT event_id) FROM commands WHERE parsed=0")?;
    let signatures:Vec<Json>=db.prepare("SELECT signature,sum(n) FROM shapes WHERE known=0 GROUP BY signature ORDER BY sum(n) DESC LIMIT 100")?.query_map([],|r|Ok(json!({"signature":redact(&r.get::<_,String>(0)?),"records":r.get::<_,i64>(1)?})))?.collect::<rusqlite::Result<_>>()?;
    Ok(
        json!({"files":scalar("SELECT count(*) FROM files")?,"sessions":scalar("SELECT count(DISTINCT harness||':'||session_id) FROM sessions")?,"events":scalar("SELECT count(*) FROM events")?,"canonical_events":scalar("SELECT count(*) FROM canonical_events")?,"unknown_shapes":{"numerator":unknown,"denominator":shapes,"rate":if shapes==0 {0.0}else {unknown as f64/shapes as f64},"signatures":signatures},"unparsed_shell_calls":{"numerator":unparsed,"denominator":shell,"rate":if shell==0 {0.0}else {unparsed as f64/shell as f64}},"parse_errors":scalar("SELECT coalesce(sum(parse_errors),0) FROM files")?,"incomplete_files":scalar("SELECT count(*) FROM files WHERE index_status!='ready'")?,"unknown_models":scalar("SELECT count(*) FROM events WHERE model IS NULL")?,"unknown_session_kinds":scalar("SELECT count(*) FROM sessions WHERE kind='unknown'")?,"unclassified_denials":scalar("SELECT count(*) FROM denials WHERE reason_id='unknown'")?}),
    )
}

pub fn sql(db: &Connection, sql: &str) -> Result<Vec<Json>> {
    db.execute_batch("PRAGMA query_only=ON;")?;
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline));
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
    db.progress_handler(0, None::<fn() -> bool>);
    db.execute_batch("PRAGMA query_only=OFF;")?;
    result
}
