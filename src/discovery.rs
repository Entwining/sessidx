use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct Root {
    pub harness: String,
    pub path: PathBuf,
}

pub fn defaults(home: &Path) -> Vec<Root> {
    [
        ("claude", ".claude/projects"),
        ("codex", ".codex/sessions"),
        ("pi", ".pi/agent/sessions"),
    ]
    .into_iter()
    .map(|(h, p)| Root {
        harness: h.into(),
        path: home.join(p),
    })
    .collect()
}

pub struct Discovered {
    /// Harness and path of every session file, sorted by path.
    pub files: Vec<(String, PathBuf)>,
    /// Harnesses whose root does not exist.
    pub missing_roots: Vec<String>,
}

pub fn files(roots: &[Root]) -> Result<Discovered> {
    let mut files = Vec::new();
    let mut missing_roots = Vec::new();
    for root in roots {
        anyhow::ensure!(
            ["claude", "codex", "pi"].contains(&root.harness.as_str()),
            "unknown harness"
        );
        if !root.path.exists() {
            missing_roots.push(root.harness.clone());
            continue;
        }
        for entry in WalkDir::new(&root.path).follow_links(false) {
            let entry = entry.context("session discovery failed")?;
            if entry.file_type().is_file() && entry.path().extension().is_some_and(|v| v == "jsonl")
            {
                files.push((root.harness.clone(), entry.into_path()));
            }
        }
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(Discovered {
        files,
        missing_roots,
    })
}
