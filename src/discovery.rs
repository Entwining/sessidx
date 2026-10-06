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

pub fn files(roots: &[Root]) -> Result<(Vec<(String, PathBuf)>, Vec<String>)> {
    let mut files = Vec::new();
    let mut missing = Vec::new();
    for root in roots {
        anyhow::ensure!(
            ["claude", "codex", "pi"].contains(&root.harness.as_str()),
            "unknown harness"
        );
        if !root.path.exists() {
            missing.push(root.harness.clone());
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
    Ok((files, missing))
}
