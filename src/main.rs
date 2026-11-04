#![warn(clippy::unwrap_used)]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum, builder::RangedU64ValueParser};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sessidx::{
    counting::{self, By, Metric},
    discovery::{self, Root},
    model::{Agent, name},
    query::{self, Coverage, Filters},
    redaction::redact,
    store::{Refresh, Store},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    version,
    about = "Local Claude Code, Codex, and Pi session index",
    after_help = "Examples:\n  sessidx search 'instruction hash'\n  sessidx grep 'Script error:' --agent codex\n  sessidx show /path/to/session.jsonl:42\n  sessidx count commands --program rg\n  sessidx sql 'SELECT role,count(*) FROM events GROUP BY role'\n  sessidx index --full\n  sessidx doctor"
)]
struct Cli {
    /// Index database to use instead of ~/.cache/sessidx/index.db
    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,
    /// Session root to scan instead of the defaults; repeat for each root
    #[arg(long = "root", global = true, value_name = "AGENT=PATH", value_parser = root)]
    roots: Vec<Root>,
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Page {
    /// Maximum results per page, 1-1000; a page also ends within 24 KiB
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
        /// A hit's ref (path:line), a session ID, a transcript path, or a codex://threads/ID link
        target: String,
        /// Source lines to include on each side of a path:line target
        #[arg(long, default_value_t = 3)]
        around: usize,
        /// Maximum records per page, 1-1000; a page also ends within 24 KiB
        #[arg(long, default_value_t = 100, value_parser = RangedU64ValueParser::<usize>::new().range(1..=1000))]
        limit: usize,
        /// Continue from the previous end record's `next` with the same target
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Aggregate static sites, native failures, denial events, model requests' token usage, or compactions
    Count {
        #[arg(value_enum)]
        metric: Metric,
        /// Comma-separated grouping; with no value, one total row [default: agent,model,role,week; for usage and compactions agent,model,week]
        #[arg(long, value_enum, value_delimiter = ',', num_args = 0..=1, action = clap::ArgAction::Set)]
        by: Option<Vec<By>>,
        /// Select shell calls whose static syntax names this program
        #[arg(long, value_name = "NAME")]
        program: Option<String>,
        #[command(flatten)]
        filters: Filters,
    },
    /// Run one read-only SELECT/WITH over the refreshed index
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
    /// Report parse errors and the unknown part of each counted population
    Doctor,
    /// Print or install the agent skill this version ships
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Print the skill as Markdown
    Show,
    /// Link ~/.claude/skills/sessidx and ~/.agents/skills/sessidx to the skill packaged beside this binary
    Install,
}

fn root(s: &str) -> Result<Root, String> {
    let (agent, path) = s.split_once('=').ok_or("expected AGENT=PATH")?;
    let agent = Agent::from_str(agent, false).map_err(|_| {
        let names: Vec<_> = Agent::value_variants().iter().map(|h| name(*h)).collect();
        format!("AGENT '{agent}' must be one of {}", names.join(", "))
    })?;
    Ok(Root {
        agent,
        path: path.into(),
    })
}

/// A page's place in a query over one snapshot of the index: events up to
/// `high_water`, and the changes up to `revision`, which later changes are
/// checked against.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    scope: String,
    high_water: i64,
    revision: i64,
    /// grep and show: the last event examined.
    after: Option<query::Position>,
    /// search: how many sessions of the frozen ranking were passed, returned
    /// or no longer matching, the file IDs of that ranking, and how many
    /// sessions matched.
    offset: usize,
    search_order: Option<Vec<i64>>,
    search_matched: usize,
    /// show: where the record at `after` resumes in its JSON text, as
    /// `Coverage::within`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    within: Option<usize>,
}

const CURSOR_VERSION: u8 = 5;

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

/// The snapshot a page reads: a new one without a cursor, or the cursor's
/// unless `invalidated` finds a change since it that the later pages would
/// read. Rule: docs/formats.md `cursor_invalidation`.
fn snapshot(
    db: &rusqlite::Connection,
    cursor: Option<&str>,
    request: &Value,
    invalidated: impl Fn(&Cursor) -> Result<bool>,
) -> Result<Cursor> {
    let revision: i64 = db.query_row("SELECT coalesce(max(id),0) FROM index_changes", [], |r| {
        r.get(0)
    })?;
    let Some(cursor) = cursor else {
        return Ok(Cursor {
            version: CURSOR_VERSION,
            scope: scope(db, request)?,
            high_water: db.query_row("SELECT coalesce(max(id),0) FROM event_rows", [], |r| {
                r.get(0)
            })?,
            revision,
            after: None,
            offset: 0,
            search_order: None,
            search_matched: 0,
            within: None,
        });
    };
    let cursor: Cursor = serde_json::from_str(cursor)
        .context("invalid cursor; restart the query without --cursor")?;
    anyhow::ensure!(
        cursor.version == CURSOR_VERSION
            && cursor
                .after
                .is_none_or(|(file, line, ordinal)| file >= 0 && line >= 0 && ordinal >= 0)
            && cursor.high_water >= 0
            && cursor.revision <= revision
            && cursor.revision >= 0
            && cursor.scope == scope(db, request)?
            && !invalidated(&cursor)?,
        query::CURSOR_MISMATCH
    );
    Ok(cursor)
}

fn encode(cursor: Option<Cursor>) -> Result<Option<String>> {
    cursor
        .map(|cursor| Ok(serde_json::to_string(&cursor)?))
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

/// `refresh` is `None` for a command that runs no refresh, whose `stale` is
/// then null.
fn end(
    refresh: Option<&Refresh>,
    coverage: &Coverage,
    cursor: Option<String>,
    unit: &str,
    returned: usize,
) -> Result<()> {
    let stale = refresh.map(|r| r.stale);
    let mut record = json!({"complete": !coverage.incomplete && stale != Some(true), "stale":stale,"next":cursor,"searched":{"unit":unit,"records":coverage.records,"bytes":coverage.bytes,"returned":returned},"unavailable_ranges":coverage.unavailable_ranges});
    if let Some(refresh) = refresh {
        record["refresh"] = json!(refresh);
    }
    emit("end", record)
}

/// Writes a query's data records and its `end` record, and returns its exit
/// code: 0 with data, 1 without.
fn finish<T: Serialize>(
    kind: &str,
    records: &[T],
    refresh: Option<&Refresh>,
    coverage: &Coverage,
    cursor: Option<String>,
    unit: &str,
) -> Result<i32> {
    for record in records {
        emit(kind, serde_json::to_value(record)?)?;
    }
    end(refresh, coverage, cursor, unit, records.len())?;
    Ok(i32::from(records.is_empty()))
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
        } => count(
            &path,
            &roots,
            metric,
            &by.unwrap_or_else(|| metric.default_by()),
            program.as_deref(),
            &filters,
        ),
        Command::Sql { query } => sql(&path, &roots, &query),
        Command::Index { full } => index(&path, &roots, full),
        Command::Doctor => doctor(&path, &roots),
        Command::Skill {
            command: SkillCommand::Show,
        } => {
            io::stdout().lock().write_all(SKILL.as_bytes())?;
            Ok(0)
        }
        Command::Skill {
            command: SkillCommand::Install,
        } => install_skill(&home),
    }
}

/// Each lookup refreshes within the budget; callers then read one SQLite
/// snapshot (a transaction on the store) so all records and cursor fingerprints agree.
fn refreshed(path: &Path, roots: &[Root]) -> Result<(Store, Refresh)> {
    let mut store = Store::open(path)?;
    let refresh = store.refresh(roots, false, Some(sessidx::BUDGET))?;
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
    // Only the frozen sessions not yet returned matter to later pages.
    let mut snapshot = snapshot(&db, page.cursor.as_deref(), &request, |c| {
        let unvisited = c
            .search_order
            .as_deref()
            .and_then(|order| order.get(c.offset..))
            .unwrap_or_default();
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM index_changes WHERE id>? AND file_id IN (SELECT value FROM json_each(?)))",
            rusqlite::params![c.revision, serde_json::to_string(unvisited)?],
            |r| r.get(0),
        )?)
    })?;
    let offset = snapshot.offset;
    filters.high_water = Some(snapshot.high_water);
    let remaining: Option<Vec<i64>> = snapshot
        .search_order
        .as_ref()
        .map(|order| order.iter().skip(offset).copied().collect());
    filters.search_order = remaining.clone();
    let found = query::search(&db, query, &filters, page.limit, sessidx::BUDGET)?;
    if snapshot.search_order.is_none() {
        snapshot.search_order = found.order;
        snapshot.search_matched = found.matched_sessions;
    }
    let sessions = found.sessions;
    let records = sessions.iter().map(|s| s.hits.len()).sum();
    // A frozen session that stopped matching, as when an append changed its
    // kind, is passed over rather than read again. Rule: docs/formats.md
    // `search_page_progress`.
    let reached = match remaining {
        None => offset + sessions.len(),
        Some(remaining) if sessions.len() == found.matched_sessions => offset + remaining.len(),
        Some(remaining) => {
            let last = sessions.last().map(|s| s.file_id);
            offset
                + remaining
                    .iter()
                    .position(|&id| Some(id) == last)
                    .map_or(remaining.len(), |i| i + 1)
        }
    };
    let more = reached < snapshot.search_matched;
    // Past the frozen prefix the end stays incomplete without a cursor.
    let coverage = Coverage {
        incomplete: more,
        records,
        ..Coverage::default()
    };
    let cursor = encode(
        (more && reached < query::SEARCH_PAGE_SESSIONS).then_some(Cursor {
            offset: reached,
            ..snapshot
        }),
    )?;
    finish(
        "session",
        &sessions,
        Some(&refresh),
        &coverage,
        cursor,
        "session_hits",
    )
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
        "grep requires a narrowing filter: add --since, --until, --cwd, --session, or --agent"
    );
    let (store, refresh) = refreshed(path, roots)?;
    let db = store.db.unchecked_transaction()?;
    let request = json!(["grep", path, pattern, filters]);
    // Files are read in ID order, so a change matters only from the file the
    // cursor stopped in, and only to a file that held events then.
    let snapshot = snapshot(&db, page.cursor.as_deref(), &request, |c| {
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM index_changes WHERE id>? AND first_event_id<=? AND file_id>=?)",
            [c.revision, c.high_water, c.after.map_or(0, |a| a.0)],
            |r| r.get(0),
        )?)
    })?;
    filters.high_water = Some(snapshot.high_water);
    let (hits, coverage) = query::scan(
        &db,
        pattern,
        &filters,
        page.limit,
        snapshot.after,
        sessidx::BUDGET,
    )?;
    finish(
        "hit",
        &hits,
        Some(&refresh),
        &coverage,
        encode(coverage.continuation.map(|after| Cursor {
            after: Some(after),
            ..snapshot
        }))?,
        "source_ranges",
    )
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
    // A replaced transcript has a new file ID, which `show_snapshot` refuses.
    let snapshot = snapshot(&db, cursor, &request, |_| Ok(false))?;
    let (hits, coverage) = query::show_snapshot(
        &db,
        target,
        around,
        limit,
        snapshot.after,
        snapshot.within,
        snapshot.high_water,
    )?;
    finish(
        "record",
        &hits,
        Some(&refresh),
        &coverage,
        encode(coverage.continuation.map(|after| Cursor {
            after: Some(after),
            within: coverage.within,
            ..snapshot
        }))?,
        "source_ranges",
    )
}

fn count(
    path: &Path,
    roots: &[Root],
    metric: Metric,
    by: &[By],
    program: Option<&str>,
    filters: &Filters,
) -> Result<i32> {
    counting::check(metric, by, program, filters)?;
    let (store, refresh) = refreshed(path, roots)?;
    let db = store.db.unchecked_transaction()?;
    let rows = counting::count(&db, metric, by, program, filters)?;
    finish(
        "count",
        &rows,
        Some(&refresh),
        &Coverage {
            records: rows.len(),
            ..Coverage::default()
        },
        None,
        "aggregate_rows",
    )
}

fn doctor(path: &Path, roots: &[Root]) -> Result<i32> {
    let (store, refresh) = refreshed(path, roots)?;
    let db = store.db.unchecked_transaction()?;
    finish(
        "doctor",
        &[counting::doctor(&db)?],
        Some(&refresh),
        &Coverage::default(),
        None,
        "diagnostic",
    )
}

fn sql(path: &Path, roots: &[Root], query: &str) -> Result<i32> {
    let (store, refresh) = refreshed(path, roots)?;
    let rows: Vec<Value> = counting::sql(&store.db, query)?
        .into_iter()
        .map(|row| json!({"data":row}))
        .collect();
    finish(
        "row",
        &rows,
        Some(&refresh),
        &Coverage {
            records: rows.len(),
            ..Coverage::default()
        },
        None,
        "sql_rows",
    )
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
        Some(&report),
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

const SKILL: &str = include_str!("../skills/sessidx/SKILL.md");

/// The binary's path is taken as it was run, not resolved, so that run through
/// Homebrew's `bin` link the skill links point through its `share` link and
/// follow upgrades.
fn install_skill(home: &Path) -> Result<i32> {
    let exe = std::env::current_exe()?;
    let source = exe
        .parent()
        .and_then(Path::parent)
        .context("the binary has no parent directory")?
        .join("share/sessidx/skills/sessidx");
    anyhow::ensure!(
        source.is_dir(),
        "no skill is packaged at {}; skill install links the skill a package such as Homebrew's installs beside the binary, so install the skill as the README describes instead",
        source.display()
    );
    let dirs = [".agents/skills", ".claude/skills"].map(|dir| home.join(dir));
    // Both paths are checked before either changes, so a refusal changes nothing.
    for dir in &dirs {
        let link = dir.join("sessidx");
        if let Ok(meta) = fs::symlink_metadata(&link) {
            anyhow::ensure!(
                meta.file_type().is_symlink(),
                "{} exists and is not a link; remove it, then run sessidx skill install again",
                link.display()
            );
        }
    }
    // `~/.claude/skills` can be a link to `~/.agents/skills`, so that one is made first.
    let mut links = Vec::new();
    for dir in &dirs {
        let link = dir.join("sessidx");
        if fs::symlink_metadata(&link).is_ok() {
            fs::remove_file(&link)?;
        }
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        std::os::unix::fs::symlink(&source, &link)
            .with_context(|| format!("cannot link {}", link.display()))?;
        links.push(json!({"path": link, "target": source}));
    }
    finish(
        "link",
        &links,
        None,
        &Coverage {
            records: links.len(),
            ..Coverage::default()
        },
        None,
        "skill_paths",
    )
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
            // Help and version text are static; a bare `sessidx` gets the
            // help as text too, though it still exits 2.
            let _ = if code == 0
                || e.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            {
                write!(io::stdout().lock(), "{e}")
            } else {
                fail(&e.to_string())
            };
            code
        }
    };
    std::process::exit(code);
}
