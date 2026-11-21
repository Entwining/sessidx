//! Checks what a push would publish for this machine's session IDs and home
//! directory, before pushing:
//!
//! ```sh
//! cargo run --example leaks -- [--base origin/main] [--file DRAFT]...
//! ```
//!
//! A real session UUID once entered a test as a sample identifier and had to
//! be rewritten out of history. This reads every session ID from the default
//! index through the installed `sessidx`, as any query does, and looks for one
//! as a whole word in the lines `git diff BASE` adds, in untracked files, in
//! the messages of commits after `BASE`, and in each `--file`, such as a pull
//! request description; it also looks for the home directory's path. Project
//! names are left out: on the maintainer's history, cwd basenames flagged one
//! commit in ten, every one a common word or a public tool's name. A shortened
//! ID, a quoted log fragment, or a path outside the home directory is not
//! detected.
//!
//! The JSON Lines report never prints a value: a `leak` record for each line
//! that holds one, with its `source` (`diff`, `untracked`, `message`, or
//! `file`), its `location`, and its `kind` (`session_id` or `home`), then a
//! `leaks` record with the number of session IDs checked and of lines found.
//! It exits 1 when it finds any.

use anyhow::{Context, Result, ensure};
use clap::Parser;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
struct Args {
    /// The revision a push publishes changes against
    #[arg(long, default_value = "origin/main")]
    base: String,
    /// A draft to check as well, such as a pull request description
    #[arg(long, value_name = "PATH")]
    file: Vec<PathBuf>,
    /// The sessidx binary to read session IDs with
    #[arg(long, value_name = "PATH", default_value = "sessidx")]
    sessidx: OsString,
}

/// One line of text a push would publish, with where it comes from.
struct Line {
    source: &'static str,
    location: String,
    text: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let ids = session_ids(&args.sessidx)?;
    let home = std::env::home_dir().context("home directory unknown")?;
    let home = home.to_str().context("home directory is not UTF-8")?;

    let diff = run(
        "git",
        &[
            "diff",
            "--unified=0",
            "--no-color",
            "--no-ext-diff",
            &args.base,
        ],
    )?;
    let mut lines = added_lines(&diff);
    let untracked = run("git", &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for path in untracked.split('\0').filter(|p| !p.is_empty()) {
        // Binary files cannot hold a pasted identifier as text.
        if let Ok(text) = std::fs::read_to_string(path) {
            lines.extend(numbered("untracked", path, &text));
        }
    }
    let log = run(
        "git",
        &[
            "log",
            "--format=%h%x00%B%x1e",
            &format!("{}..HEAD", args.base),
        ],
    )?;
    for entry in log.split('\x1e').map(str::trim).filter(|e| !e.is_empty()) {
        let (commit, message) = entry
            .split_once('\0')
            .context("unexpected git log output")?;
        lines.extend(numbered("message", commit, message));
    }
    for path in &args.file {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        lines.extend(numbered("file", &path.to_string_lossy(), &text));
    }

    let found = findings(&lines, &ids, home);
    for (line, kind) in &found {
        println!(
            "{}",
            json!({"type": "leak", "source": line.source, "location": line.location, "kind": kind})
        );
    }
    println!(
        "{}",
        json!({"type": "leaks", "session_ids": ids.len(), "found": found.len()})
    );
    if !found.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

/// The lines that hold a session ID as a whole word or the home directory's
/// path, each with the kind it holds, a session ID first.
fn findings<'a>(
    lines: &'a [Line],
    ids: &HashSet<String>,
    home: &str,
) -> Vec<(&'a Line, &'static str)> {
    let word = Regex::new(r"[A-Za-z0-9_-]+").expect("valid pattern");
    lines
        .iter()
        .filter_map(|line| {
            if word.find_iter(&line.text).any(|w| ids.contains(w.as_str())) {
                Some((line, "session_id"))
            } else if line.text.contains(home) {
                Some((line, "home"))
            } else {
                None
            }
        })
        .collect()
}

/// The lines a `--unified=0` diff adds, located by file and new line number.
fn added_lines(diff: &str) -> Vec<Line> {
    let hunk = Regex::new(r"^@@ -\S+ \+(\d+)").expect("valid pattern");
    let mut lines = Vec::new();
    let (mut path, mut next) = (String::new(), 0);
    for text in diff.lines() {
        if let Some(p) = text.strip_prefix("+++ ") {
            path = p.strip_prefix("b/").unwrap_or(p).to_owned();
        } else if let Some(c) = hunk.captures(text) {
            next = c[1].parse().unwrap_or(0);
        } else if let Some(added) = text.strip_prefix('+') {
            lines.push(Line {
                source: "diff",
                location: format!("{path}:{next}"),
                text: added.to_owned(),
            });
            next += 1;
        }
    }
    lines
}

fn numbered(source: &'static str, name: &str, text: &str) -> Vec<Line> {
    text.lines()
        .enumerate()
        .map(|(i, t)| Line {
            source,
            location: format!("{name}:{}", i + 1),
            text: t.to_owned(),
        })
        .collect()
}

/// Every session ID in the default index, read a page at a time because
/// `sql` refuses more than 10,000 rows.
fn session_ids(sessidx: &OsString) -> Result<HashSet<String>> {
    let mut ids = HashSet::new();
    let mut last = String::new();
    loop {
        let sql = format!(
            "SELECT DISTINCT session_id FROM sessions WHERE session_id > '{last}' \
             ORDER BY session_id LIMIT 5000"
        );
        let out = run(sessidx, &["sql", &sql])?;
        let records: Vec<Value> = out
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let end = records
            .last()
            .filter(|r| r["type"] == "end")
            .context("no end record")?;
        ensure!(
            end["error"].is_null() && end["complete"] == true,
            "sessidx sql failed: {end}"
        );
        let page: Vec<String> = records
            .iter()
            .filter_map(|r| r["data"]["session_id"].as_str().map(str::to_owned))
            .collect();
        let Some(tail) = page.last() else { break };
        last = tail.clone();
        ids.extend(page);
    }
    Ok(ids)
}

/// The standard output of a command that exits 0, or for `sessidx`, whose
/// `end` record carries the outcome, 0 or 1 (no data).
#[expect(
    clippy::disallowed_methods,
    reason = "the check reads Git and the installed binary as a caller does"
)]
fn run(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<String> {
    let sessidx = program.as_ref() != "git";
    let out = Command::new(program).args(args).output()?;
    ensure!(
        out.status.success() || (sessidx && out.status.code() == Some(1)),
        "{} failed: {}",
        args[0],
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b9f6a52-3c1e-4d7a-9e2f-5a8c7d6b4e31";

    #[test]
    fn added_lines_carry_their_new_line_numbers() {
        let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -3,0 +4,2 @@\n+one\n+two\n\
                    @@ -10 +12 @@ fn f() {\n-old\n+new\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n";
        let found: Vec<_> = added_lines(diff)
            .into_iter()
            .map(|l| (l.location, l.text))
            .collect();
        let expected = [("x.rs:4", "one"), ("x.rs:5", "two"), ("x.rs:12", "new")];
        assert_eq!(found, expected.map(|(a, b)| (a.to_owned(), b.to_owned())));
    }

    #[test]
    fn a_session_id_counts_as_a_whole_word_and_the_home_path_anywhere() {
        let ids = HashSet::from([ID.to_owned()]);
        let lines = numbered(
            "file",
            "draft.md",
            &format!(
                "see codex://threads/{ID}\n{ID}.jsonl\nx{ID}\n/Users/someone/project\n\
                 {ID}0\nplain text\n/Users/someone2"
            ),
        );
        let found: Vec<_> = findings(&lines, &ids, "/Users/someone")
            .into_iter()
            .map(|(l, kind)| (l.location.as_str(), kind))
            .collect();
        assert_eq!(
            found,
            [
                ("draft.md:1", "session_id"),
                ("draft.md:2", "session_id"),
                ("draft.md:4", "home"),
                ("draft.md:7", "home"),
            ]
        );
    }
}
