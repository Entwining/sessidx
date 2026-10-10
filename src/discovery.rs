use crate::model::{Agent, name};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct Root {
    pub agent: Agent,
    pub path: PathBuf,
}

pub fn defaults(home: &Path) -> Vec<Root> {
    [
        (Agent::Claude, ".claude/projects"),
        (Agent::Codex, ".codex/sessions"),
        (Agent::Codex, ".codex/archived_sessions"),
        (Agent::Pi, ".pi/agent/sessions"),
    ]
    .into_iter()
    .map(|(agent, p)| Root {
        agent,
        path: home.join(p),
    })
    .collect()
}

pub struct Discovered {
    /// Agent and plain path of every session file, sorted by path.
    pub files: Vec<(Agent, PathBuf)>,
    /// Each root that does not exist, as the `AGENT=PATH` that `--root` accepts.
    pub missing_roots: Vec<String>,
}

pub fn files(roots: &[Root]) -> Result<Discovered> {
    let mut files = Vec::new();
    let mut missing_roots = Vec::new();
    for root in roots {
        if !root.path.exists() {
            missing_roots.push(format!("{}={}", name(root.agent), root.path.display()));
            continue;
        }
        for entry in WalkDir::new(&root.path).follow_links(false) {
            let entry = entry.context("session discovery failed")?;
            if entry.file_type().is_file()
                && !(root.agent == Agent::Claude && workflow_journal(entry.path()))
                && let Some(plain) = crate::source::plain_path(entry.path())
            {
                files.push((root.agent, plain));
            }
        }
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    // A rollout caught between its plain and compressed forms is one session.
    files.dedup_by(|a, b| a.1 == b.1);
    Ok(Discovered {
        files,
        missing_roots,
    })
}

/// Claude writes a workflow run's journal of agent starts and results beside
/// its agents' transcripts; it is not a session. Rule: docs/formats.md
/// `claude_workflow_journal`.
fn workflow_journal(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "journal.jsonl")
        && path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|n| n == "workflows")
}
