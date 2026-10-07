use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sessidx::{
    counting,
    discovery::{self, Root},
    query::{self, Coverage, Filters, Harness},
    redaction::redact,
    store::{Refresh, Store},
};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    version,
    about = "Local Claude Code, Codex, and Pi session index",
    after_help = "Examples:\n  sessidx search 'instruction hash'\n  sessidx grep 'Script error:' --harness codex\n  sessidx show /path/to/session.jsonl:42\n  sessidx count commands --program rg\n  sessidx sql 'SELECT role,count(*) FROM events GROUP BY role'\n  sessidx index --full\n  sessidx doctor"
)]
struct Cli {
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[arg(long = "root", global = true, value_name = "HARNESS=PATH")]
    roots: Vec<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Page {
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Metric {
    Commands,
    Failures,
    Denials,
}

#[derive(Subcommand)]
enum Command {
    /// Ranked full-text lookup
    Search {
        query: String,
        #[command(flatten)]
        filters: Filters,
        #[command(flatten)]
        page: Page,
    },
    /// Regex match over original indexed source ranges; requires narrowing
    Grep {
        pattern: String,
        #[command(flatten)]
        filters: Filters,
        #[command(flatten)]
        page: Page,
    },
    /// Expand a hit ref or session ID to source records
    Show {
        target: String,
        #[arg(long, default_value_t = 3)]
        around: usize,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Aggregate static sites, native failures, or denial events
    Count {
        #[arg(value_enum)]
        metric: Metric,
        #[arg(long, default_value = "harness,model,role,week")]
        by: String,
        #[arg(long)]
        program: Option<String>,
        #[command(flatten)]
        filters: Filters,
    },
    /// Run one read-only SELECT/WITH without refresh
    Sql { query: String },
    /// Refresh the derived index; --full rebuilds its schema
    Index {
        #[arg(long)]
        full: bool,
    },
    /// Report index and parser coverage
    Doctor,
}

#[derive(Clone, Serialize, Deserialize)]
struct Cursor {
    version: u8,
    scope: String,
    position: i64,
    high_water: i64,
    revision: i64,
    search_order: Option<String>,
}

fn scope(db: &rusqlite::Connection, request: &Value) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(request)?);
    let instance: String =
        db.query_row("SELECT instance FROM index_identity WHERE id=1", [], |r| {
            r.get(0)
        })?;
    hash.update(instance);
    Ok(format!("{:x}", hash.finalize()))
}

fn snapshot(db: &rusqlite::Connection, cursor: Option<&str>, request: &Value) -> Result<Cursor> {
    let revision: i64 = db.query_row("SELECT coalesce(max(id),0) FROM index_changes", [], |r| {
        r.get(0)
    })?;
    let Some(cursor) = cursor else {
        return Ok(Cursor {
            version: 2,
            scope: scope(db, request)?,
            position: 0,
            high_water: db.query_row("SELECT coalesce(max(id),0) FROM locations", [], |r| {
                r.get(0)
            })?,
            revision,
            search_order: None,
        });
    };
    let cursor: Cursor = serde_json::from_str(cursor)
        .context("invalid cursor; restart the query without --cursor")?;
    let changed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM index_changes WHERE id>? AND first_event_id<=?)",
        [cursor.revision, cursor.high_water],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        cursor.version == 2
            && cursor.position >= 0
            && cursor.high_water >= 0
            && cursor.revision <= revision
            && cursor.revision >= 0
            && cursor.scope == scope(db, request)?
            && !changed,
        "cursor belongs to another query or index version; restart without --cursor"
    );
    Ok(cursor)
}

fn next(snapshot: &Cursor, position: Option<i64>) -> Result<Option<String>> {
    position
        .map(|position| {
            Ok(serde_json::to_string(&Cursor {
                position,
                ..snapshot.clone()
            })?)
        })
        .transpose()
}

fn emit(kind: &str, value: Value) -> Result<()> {
    let mut value = value;
    value
        .as_object_mut()
        .context("output record must be an object")?
        .insert("type".into(), json!(kind));
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

fn end(
    refresh: Option<&Refresh>,
    coverage: &Coverage,
    cursor: Option<String>,
    unit: &str,
    returned: usize,
) -> Result<()> {
    let stale = refresh.is_some_and(|r| r.stale);
    emit(
        "end",
        json!({"complete": !coverage.incomplete && !stale, "stale":stale,"next":cursor,"searched":{"unit":unit,"records":coverage.records,"bytes":coverage.bytes,"returned":returned},"unavailable_ranges":coverage.unavailable_ranges,"refresh":refresh}),
    )
}

fn run(cli: Cli) -> Result<i32> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME missing")?);
    let path = cli
        .db
        .unwrap_or_else(|| home.join(".cache/sessidx/index.db"));
    if let Command::Sql { query } = &cli.command {
        let _read_lock = Store::read_lock(&path)?;
        let refresh = Refresh {
            stale: _read_lock.is_none(),
            writer_busy: _read_lock.is_none(),
            ..Refresh::default()
        };
        let db = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        db.busy_timeout(Duration::from_millis(50))?;
        Store::require_schema(&db)?;
        let rows = counting::sql(&db, query)?;
        for row in &rows {
            emit("row", json!({"data":row}))?;
        }
        end(
            Some(&refresh),
            &Coverage {
                records: rows.len(),
                ..Coverage::default()
            },
            None,
            "sql_rows",
            rows.len(),
        )?;
        return Ok(if rows.is_empty() { 1 } else { 0 });
    }
    let roots = if cli.roots.is_empty() {
        discovery::defaults(&home)
    } else {
        cli.roots
            .iter()
            .map(|r| {
                let (h, p) = r.split_once('=').context("--root requires HARNESS=PATH")?;
                Harness::from_str(h, false).map_err(anyhow::Error::msg)?;
                Ok(Root {
                    harness: h.into(),
                    path: p.into(),
                })
            })
            .collect::<Result<Vec<_>>>()?
    };
    if let Command::Grep { filters, .. } = &cli.command {
        anyhow::ensure!(
            filters.narrowed(),
            "grep requires a narrowing filter: add --since, --until, --cwd, --session, or --harness"
        );
    }
    let full = matches!(cli.command, Command::Index { full: true });
    let mut store = if full {
        Store::open_for_rebuild(&path)?
    } else {
        Store::open(&path)?
    };
    if let Command::Index { full } = cli.command {
        let report = store.refresh(&roots, full, None)?;
        emit("index", serde_json::to_value(&report)?)?;
        end(
            Some(&report),
            &Coverage {
                records: report.records as usize,
                ..Coverage::default()
            },
            None,
            "source_records",
            report.records as usize,
        )?;
        return Ok(if report.stale { 2 } else { 0 });
    }
    let refresh = store.refresh(&roots, false, Some(Duration::from_secs(2)))?;
    // All records and cursor fingerprints come from one SQLite read snapshot.
    let db = store.db.unchecked_transaction()?;
    match cli.command {
        Command::Search {
            query,
            mut filters,
            page,
        } => {
            anyhow::ensure!((1..=1000).contains(&page.limit), "--limit must be 1..1000");
            let request = json!(["search_sessions", path, query, filters]);
            let mut snapshot = snapshot(&db, page.cursor.as_deref(), &request)?;
            let offset = snapshot.position;
            filters.high_water = Some(snapshot.high_water);
            filters.search_order = snapshot.search_order.clone();
            let mut sessions =
                query::search(&db, &query, &filters, page.limit + 1, offset as usize)?;
            if snapshot.search_order.is_none() {
                snapshot.search_order = sessions.first_mut().and_then(|s| s.search_order.take());
            }
            let records = sessions.iter().map(|s| s.hits.len()).sum();
            let more = sessions.len() > page.limit;
            sessions.truncate(page.limit);
            for session in &sessions {
                emit("session", serde_json::to_value(session)?)?;
            }
            let cursor = next(&snapshot, more.then_some(offset + sessions.len() as i64))?;
            end(
                Some(&refresh),
                &Coverage {
                    incomplete: more,
                    records,
                    ..Coverage::default()
                },
                cursor,
                "session_hits",
                sessions.len(),
            )?;
            Ok(if sessions.is_empty() { 1 } else { 0 })
        }
        Command::Grep {
            pattern,
            mut filters,
            page,
        } => {
            anyhow::ensure!((1..=1000).contains(&page.limit), "--limit must be 1..1000");
            let request = json!(["grep", path, pattern, filters]);
            let snapshot = snapshot(&db, page.cursor.as_deref(), &request)?;
            let after = snapshot.position;
            filters.high_water = Some(snapshot.high_water);
            let (hits, coverage) = query::scan(
                &db,
                &pattern,
                &filters,
                page.limit,
                after,
                Duration::from_secs(2),
            )?;
            for hit in &hits {
                emit("hit", serde_json::to_value(hit)?)?;
            }
            end(
                Some(&refresh),
                &coverage,
                next(&snapshot, coverage.continuation)?,
                "source_ranges",
                hits.len(),
            )?;
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        Command::Show {
            target,
            around,
            limit,
            cursor,
        } => {
            anyhow::ensure!((1..=1000).contains(&limit), "--limit must be 1..1000");
            let request = json!(["show", path, target, around]);
            let snapshot = snapshot(&db, cursor.as_deref(), &request)?;
            let after = snapshot.position;
            let (hits, coverage) =
                query::show_snapshot(&db, &target, around, limit, after, snapshot.high_water)?;
            for hit in &hits {
                let mut value = serde_json::to_value(hit)?;
                let obj = value.as_object_mut().unwrap();
                let text = obj.remove("snippet").unwrap();
                obj.insert("text".into(), text);
                emit("record", value)?;
            }
            end(
                Some(&refresh),
                &coverage,
                next(&snapshot, coverage.continuation)?,
                "source_ranges",
                hits.len(),
            )?;
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        Command::Count {
            metric,
            by,
            program,
            filters,
        } => {
            let metric = metric.to_possible_value().unwrap();
            let rows = counting::count(&db, metric.get_name(), &by, program.as_deref(), &filters)?;
            for row in &rows {
                emit("count", row.clone())?;
            }
            end(
                Some(&refresh),
                &Coverage {
                    records: rows.len(),
                    ..Coverage::default()
                },
                None,
                "aggregate_rows",
                rows.len(),
            )?;
            Ok(if rows.is_empty() { 1 } else { 0 })
        }
        Command::Doctor => {
            emit("doctor", counting::doctor(&db)?)?;
            end(Some(&refresh), &Coverage::default(), None, "diagnostic", 1)?;
            Ok(0)
        }
        Command::Index { .. } | Command::Sql { .. } => unreachable!(),
    }
}

fn fail(message: &str) {
    println!(
        "{}",
        json!({"type":"end","complete":false,"stale":null,"next":null,"searched":{"unit":null,"records":0,"bytes":0,"returned":0},"error":redact(message)})
    );
}

fn main() {
    let code = match Cli::try_parse() {
        Ok(cli) => match run(cli) {
            Ok(code) => code,
            Err(e) => {
                fail(&format!("{e:#}"));
                2
            }
        },
        Err(e) => {
            let code = e.exit_code();
            if code == 0 {
                println!("{}", redact(&e.to_string()));
            } else {
                fail(&e.to_string());
            }
            code
        }
    };
    std::process::exit(code);
}
