use anyhow::Result;
use clap::{Parser, Subcommand};
use sessidx::{
    discovery::{self, Root},
    query::{self, Filters},
    redaction::redact,
    store::{Refresh, Store},
};
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(version, about = "Local Claude Code, Codex, and Pi session index")]
struct Cli {
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[arg(long = "root", global = true, value_name = "HARNESS=PATH")]
    roots: Vec<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Index {
        #[arg(long)]
        full: bool,
    },
    Search {
        #[arg(required_unless_present = "scan", conflicts_with = "scan")]
        query: Option<String>,
        #[arg(long)]
        scan: Option<String>,
        #[command(flatten)]
        filters: Filters,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 0)]
        cursor: i64,
    },
    Show {
        target: String,
        #[arg(long, default_value_t = 3)]
        around: usize,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        cursor: i64,
    },
}

fn notice(report: &Refresh) -> Result<()> {
    if report.stale {
        eprintln!(
            "{}",
            serde_json::json!({"type":"coverage","refresh":report,"notice":"index may be stale; run sessidx index to finish refresh"})
        );
    }
    Ok(())
}

fn run() -> Result<i32> {
    let cli = Cli::parse();
    let home =
        PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME missing"))?);
    let path = cli
        .db
        .unwrap_or_else(|| home.join(".cache/sessidx/index.db"));
    let roots = if cli.roots.is_empty() {
        discovery::defaults(&home)
    } else {
        cli.roots
            .iter()
            .map(|r| {
                let (h, p) = r
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("--root requires HARNESS=PATH"))?;
                Ok(Root {
                    harness: h.into(),
                    path: p.into(),
                })
            })
            .collect::<Result<Vec<_>>>()?
    };
    if let Command::Search {
        scan: Some(_),
        filters,
        ..
    } = &cli.command
    {
        anyhow::ensure!(
            filters.narrowed(),
            "--scan requires a narrowing filter: add --since, --until, --cwd, --session, --harness, or --file"
        );
    }
    let mut store = Store::open(&path)?;
    match cli.command {
        Command::Index { full } => {
            let report = store.refresh(&roots, full, None)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(if report.stale { 2 } else { 0 })
        }
        Command::Search {
            query,
            scan,
            filters,
            json,
            limit,
            offset,
            cursor,
        } => {
            anyhow::ensure!((1..=1000).contains(&limit), "--limit must be 1..1000");
            notice(&store.refresh(&roots, false, Some(Duration::from_secs(2)))?)?;
            let hits = if let Some(pattern) = scan {
                let (hits, coverage) = query::scan(
                    &store.db,
                    &pattern,
                    &filters,
                    limit,
                    cursor,
                    Duration::from_secs(2),
                )?;
                if coverage.incomplete {
                    eprintln!(
                        "{}",
                        serde_json::json!({"type":"coverage","scan":coverage,"notice":"scan incomplete; continue with --cursor"})
                    );
                }
                hits
            } else {
                query::search(
                    &store.db,
                    query.as_deref().unwrap(),
                    &filters,
                    limit,
                    offset,
                )?
            };
            query::emit_hits(&hits, json)?;
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        Command::Show {
            target,
            around,
            json,
            limit,
            cursor,
        } => {
            anyhow::ensure!((1..=1000).contains(&limit), "--limit must be 1..1000");
            notice(&store.refresh(&roots, false, Some(Duration::from_secs(2)))?)?;
            let (hits, coverage) = query::show(&store.db, &target, around, limit, cursor)?;
            if coverage.incomplete {
                eprintln!(
                    "{}",
                    serde_json::json!({"type":"coverage","show":coverage,"notice":"show incomplete; continue with --cursor"})
                );
            }
            query::emit_hits(&hits, json)?;
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("sessidx: {}", redact(&format!("{e:#}")));
            std::process::exit(2);
        }
    }
}
