use anyhow::Result;
use clap::{Parser, Subcommand};
use sessidx::{
    discovery::{self, Root},
    redaction::redact,
    store::Store,
};
use std::path::PathBuf;

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
}

fn run() -> Result<i32> {
    let cli = Cli::parse();
    let home =
        PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME missing"))?);
    let db_path = cli
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
    let mut store = Store::open(&db_path)?;
    match cli.command {
        Command::Index { full } => {
            println!(
                "{}",
                serde_json::to_string(&store.refresh(&roots, full, None)?)?
            );
        }
    }
    Ok(0)
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
