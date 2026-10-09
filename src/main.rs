#![warn(clippy::unwrap_used)]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum, builder::RangedU64ValueParser};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sessidx::{
    counting::{self, By, Metric},
    discovery::{self, Root},
    model::{Harness, name},
    query::{self, Coverage, Filters},
    redaction::redact,
    store::{Refresh, Store},
};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Parser)]
#[command(
    version,
    about = "Local Claude Code, Codex, and Pi session index",
    after_help = "Examples:\n  sessidx search 'instruction hash'\n  sessidx grep 'Script error:' --harness codex\n  sessidx show /path/to/session.jsonl:42\n  sessidx count commands --program rg\n  sessidx sql 'SELECT role,count(*) FROM events GROUP BY role'\n  sessidx index --full\n  sessidx doctor"
)]
struct Cli {
    /// Index database to use instead of ~/.cache/sessidx/index.db
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,
    /// Session root to scan instead of the defaults; repeat for each root
    #[arg(long = "root", global = true, value_name = "HARNESS=PATH", value_parser = root)]
    roots: Vec<Root>,
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Page {
    /// Maximum results, 1-1000
    #[arg(long, default_value_t = 20, value_parser = RangedU64ValueParser::<usize>::new().range(1..=1000))]
    limit: usize,
    /// Continue from the previous end record's `next` with the same command and filters
    #[arg(long)]
    cursor: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Ranked full-text lookup
    Search {
        /// Terms joined with AND; quote a phrase to keep its word order
        query: String,
        #[command(flatten)]
        filters: Filters,
        #[command(flatten)]
        page: Page,
    },
    /// Regex match over original indexed source ranges; requires narrowing
    Grep {
        /// Regular expression matched against the original source bytes
        pattern: String,
        #[command(flatten)]
        filters: Filters,
        #[command(flatten)]
        page: Page,
    },
    /// Expand a hit ref or session ID to source records
    Show {
        /// A hit's ref (path:line), a native session ID, or a codex://threads/ID link
        target: String,
        /// Source lines to include on each side of a path:line target
        #[arg(long, default_value_t = 3)]
        around: usize,
        /// Maximum records, 1-1000
        #[arg(long, default_value_t = 100, value_parser = RangedU64ValueParser::<usize>::new().range(1..=1000))]
        limit: usize,
        /// Continue from the previous end record's `next` with the same target
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Aggregate static sites, native failures, or denial events
    Count {
        #[arg(value_enum)]
        metric: Metric,
        /// Comma-separated grouping; with no value, one total row
        #[arg(long, value_enum, value_delimiter = ',', num_args = 0..=1, action = clap::ArgAction::Set, default_value = "harness,model,role,week")]
        by: Vec<By>,
        /// Count only shell calls that run this program
        #[arg(long, value_name = "NAME")]
        program: Option<String>,
        #[command(flatten)]
        filters: Filters,
    },
    /// Run one read-only SELECT/WITH without refresh
    Sql {
        /// One SELECT or WITH statement over the tables in src/schema.sql
        query: String,
    },
    /// Refresh the derived index; --full rebuilds its schema
    Index {
        /// Rebuild the index and its schema from the raw logs
        #[arg(long)]
        full: bool,
    },
    /// Report index and parser coverage
    Doctor,
}

fn root(s: &str) -> Result<Root, String> {
    let (harness, path) = s.split_once('=').ok_or("expected HARNESS=PATH")?;
    let harness = Harness::from_str(harness, false).map_err(|_| {
        let names: Vec<_> = Harness::value_variants().iter().map(|h| name(*h)).collect();
        format!("HARNESS '{harness}' must be one of {}", names.join(", "))
    })?;
    Ok(Root {
        harness,
        path: path.into(),
    })
}

#[derive(Clone, Serialize, Deserialize)]
struct Cursor {
    version: u8,
    scope: String,
    position: i64,
    high_water: i64,
    revision: i64,
    search_order: Option<Vec<i64>>,
}

fn scope(db: &rusqlite::Connection, request: &Value) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(request)?);
    let instance: String =
        db.query_row("SELECT instance FROM index_identity WHERE id=1", [], |r| {
            r.get(0)
        })?;
    hash.update(instance);
    Ok(hex::encode(hash.finalize()))
}

fn snapshot(db: &rusqlite::Connection, cursor: Option<&str>, request: &Value) -> Result<Cursor> {
    let revision: i64 = db.query_row("SELECT coalesce(max(id),0) FROM index_changes", [], |r| {
        r.get(0)
    })?;
    let Some(cursor) = cursor else {
        return Ok(Cursor {
            version: 3,
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
        cursor.version == 3
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
    writeln!(io::stdout().lock(), "{}", serde_json::to_string(&value)?)?;
    Ok(())
}

fn end(
    refresh: &Value,
    coverage: &Coverage,
    cursor: Option<String>,
    unit: &str,
    returned: usize,
) -> Result<()> {
    let stale = refresh["stale"] == true;
    emit(
        "end",
        json!({"complete": !coverage.incomplete && !stale, "stale":stale,"next":cursor,"searched":{"unit":unit,"records":coverage.records,"bytes":coverage.bytes,"returned":returned},"unavailable_ranges":coverage.unavailable_ranges,"refresh":refresh}),
    )
}

fn run(cli: Cli) -> Result<i32> {
    let home = std::env::home_dir().context("home directory unknown")?;
    let path = cli
        .db
        .unwrap_or_else(|| home.join(".cache/sessidx/index.db"));
    let roots = if cli.roots.is_empty() {
        discovery::defaults(&home)
    } else {
        cli.roots
    };
    match cli.command {
        Command::Search {
            query,
            filters,
            page,
        } => search(&path, &roots, &query, filters, page),
        Command::Grep {
            pattern,
            filters,
            page,
        } => grep(&path, &roots, &pattern, filters, page),
        Command::Show {
            target,
            around,
            limit,
            cursor,
        } => show(&path, &roots, &target, around, limit, cursor.as_deref()),
        Command::Count {
            metric,
            by,
            program,
            filters,
        } => count(&path, &roots, metric, &by, program.as_deref(), &filters),
        Command::Sql { query } => sql(&path, &query),
        Command::Index { full } => index(&path, &roots, full),
        Command::Doctor => doctor(&path, &roots),
    }
}

/// Each lookup refreshes within the budget; callers then read one SQLite
/// snapshot (a transaction on the store) so all records and cursor fingerprints agree.
fn refreshed(path: &Path, roots: &[Root]) -> Result<(Store, Refresh)> {
    let mut store = Store::open(path)?;
    let refresh = store.refresh(roots, false, Some(Duration::from_secs(2)))?;
    Ok((store, refresh))
}

fn search(
    path: &Path,
    roots: &[Root],
    query: &str,
    mut filters: Filters,
    page: Page,
) -> Result<i32> {
    let (store, refresh) = refreshed(path, roots)?;
    let db = store.db.unchecked_transaction()?;
    let request = json!(["search_sessions", path, query, filters]);
    let mut snapshot = snapshot(&db, page.cursor.as_deref(), &request)?;
    let offset = snapshot.position;
    filters.high_water = Some(snapshot.high_water);
    filters.search_order = snapshot.search_order.clone();
    let found = query::search(&db, query, &filters, page.limit, offset as usize)?;
    if snapshot.search_order.is_none() {
        snapshot.search_order = found.order;
    }
    let sessions = found.sessions;
    let records = sessions.iter().map(|s| s.hits.len()).sum();
    let reached = offset as usize + sessions.len();
    let more = reached < found.matched_sessions;
    for session in &sessions {
        emit("session", serde_json::to_value(session)?)?;
    }
    // Past the frozen prefix the end stays incomplete without a cursor.
    let cursor = next(
        &snapshot,
        (more && reached < query::SEARCH_PAGE_SESSIONS).then_some(reached as i64),
    )?;
    end(
        &json!(refresh),
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

fn grep(
    path: &Path,
    roots: &[Root],
    pattern: &str,
    mut filters: Filters,
    page: Page,
) -> Result<i32> {
    anyhow::ensure!(
        filters.narrowed(),
        "grep requires a narrowing filter: add --since, --until, --cwd, --session, or --harness"
    );
    let (store, refresh) = refreshed(path, roots)?;
    let db = store.db.unchecked_transaction()?;
    let request = json!(["grep", path, pattern, filters]);
    let snapshot = snapshot(&db, page.cursor.as_deref(), &request)?;
    let after = snapshot.position;
    filters.high_water = Some(snapshot.high_water);
    let (hits, coverage) = query::scan(
        &db,
        pattern,
        &filters,
        page.limit,
        after,
        Duration::from_secs(2),
    )?;
    for hit in &hits {
        emit("hit", serde_json::to_value(hit)?)?;
    }
    end(
        &json!(refresh),
        &coverage,
        next(&snapshot, coverage.continuation)?,
        "source_ranges",
        hits.len(),
    )?;
    Ok(if hits.is_empty() { 1 } else { 0 })
}

fn show(
    path: &Path,
    roots: &[Root],
    target: &str,
    around: usize,
    limit: usize,
    cursor: Option<&str>,
) -> Result<i32> {
    let (store, refresh) = refreshed(path, roots)?;
    let db = store.db.unchecked_transaction()?;
    let request = json!(["show", path, target, around]);
    let snapshot = snapshot(&db, cursor, &request)?;
    let after = snapshot.position;
    let (hits, coverage) =
        query::show_snapshot(&db, target, around, limit, after, snapshot.high_water)?;
    for hit in &hits {
        let mut value = serde_json::to_value(hit)?;
        let obj = value.as_object_mut().expect("Hit serializes to an object");
        let text = obj.remove("snippet").expect("Hit has a snippet field");
        obj.insert("text".into(), text);
        emit("record", value)?;
    }
    end(
        &json!(refresh),
        &coverage,
        next(&snapshot, coverage.continuation)?,
        "source_ranges",
        hits.len(),
    )?;
    Ok(if hits.is_empty() { 1 } else { 0 })
}

fn count(
    path: &Path,
    roots: &[Root],
    metric: Metric,
    by: &[By],
    program: Option<&str>,
    filters: &Filters,
) -> Result<i32> {
    if let Some(repeated) = by
        .iter()
        .enumerate()
        .find_map(|(i, key)| by[..i].contains(key).then_some(*key))
    {
        anyhow::bail!("--by repeats {}; name each grouping once", name(repeated));
    }
    let (store, refresh) = refreshed(path, roots)?;
    let db = store.db.unchecked_transaction()?;
    let rows = counting::count(&db, metric, by, program, filters)?;
    for row in &rows {
        emit("count", row.clone())?;
    }
    end(
        &json!(refresh),
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

fn doctor(path: &Path, roots: &[Root]) -> Result<i32> {
    let (store, refresh) = refreshed(path, roots)?;
    let db = store.db.unchecked_transaction()?;
    emit("doctor", counting::doctor(&db)?)?;
    end(&json!(refresh), &Coverage::default(), None, "diagnostic", 1)?;
    Ok(0)
}

fn sql(path: &Path, query: &str) -> Result<i32> {
    anyhow::ensure!(
        path.exists(),
        "no index at {}; run sessidx index to build it",
        path.display()
    );
    let read_lock = Store::read_lock(path)?;
    // sql does not refresh, so its coverage holds only what the lock shows.
    let refresh = json!({"stale": read_lock.is_none(), "writer_busy": read_lock.is_none()});
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(50))?;
    Store::require_schema(&db)?;
    let rows = counting::sql(&db, query)?;
    for row in &rows {
        emit("row", json!({"data":row}))?;
    }
    end(
        &json!(refresh),
        &Coverage {
            records: rows.len(),
            ..Coverage::default()
        },
        None,
        "sql_rows",
        rows.len(),
    )?;
    Ok(if rows.is_empty() { 1 } else { 0 })
}

fn index(path: &Path, roots: &[Root], full: bool) -> Result<i32> {
    let mut store = if full {
        Store::open_for_rebuild(path)?
    } else {
        Store::open(path)?
    };
    let report = store.refresh(roots, full, None)?;
    emit("index", serde_json::to_value(&report)?)?;
    end(
        &json!(report),
        &Coverage {
            records: report.records,
            ..Coverage::default()
        },
        None,
        "source_records",
        report.records,
    )?;
    Ok(if report.stale { 2 } else { 0 })
}

fn fail(message: &str) -> io::Result<()> {
    writeln!(
        io::stdout().lock(),
        "{}",
        json!({"type":"end","complete":false,"stale":null,"next":null,"searched":{"unit":null,"records":0,"bytes":0,"returned":0},"error":redact(message)})
    )
}

/// A reader that stops early (`| head`) is not an error, as in other filters.
fn closed_stdout(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
    })
}

fn main() {
    let code = match Cli::try_parse() {
        Ok(cli) => match run(cli) {
            Ok(code) => code,
            Err(e) if closed_stdout(&e) => 0,
            Err(e) => {
                let _ = fail(&format!("{e:#}"));
                2
            }
        },
        Err(e) => {
            let code = e.exit_code();
            let _ = if code == 0 {
                write!(io::stdout().lock(), "{e}")
            } else {
                fail(&e.to_string())
            };
            code
        }
    };
    std::process::exit(code);
}
