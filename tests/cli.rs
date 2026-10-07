use serde_json::{Value, json};
use sessidx::{discovery::Root, store::Store};
use std::{
    fs,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

// Concurrent child launches can inherit another test's live writer lock before exec.
static CLI_PROCESS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fixture() -> (tempfile::TempDir, Store, Vec<Root>) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    let mut roots = Vec::new();
    for h in ["claude", "codex", "pi"] {
        let path = dir.path().join(format!("{h}.jsonl"));
        let mut records: Vec<Value> = (0..2).map(|i| match h {
            "claude" => json!({"type":"user","uuid":format!("c-{i}"),"sessionId":"claude-session","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}}),
            "codex" => json!({"type":"response_item","timestamp":"2026-10-01T00:00:00Z","payload":{"type":"message","id":format!("x-{i}"),"role":"user","content":[{"type":"input_text","text":"sharedneedle"}]}}),
            _ => json!({"type":"message","id":format!("p-{i}"),"timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}}),
        }).collect();
        if h == "claude" {
            records.push(json!({"type":"assistant","uuid":"c-call","sessionId":"claude-session","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"Bash","input":{"command":"rg needle ."}}]}}));
        }
        fs::write(
            &path,
            records.iter().map(|v| format!("{v}\n")).collect::<String>(),
        )
        .unwrap();
        roots.push(Root {
            harness: h.into(),
            path,
        });
    }
    store.refresh(&roots, false, None).unwrap();
    (dir, store, roots)
}

fn command(store: &Store, roots: &[Root], args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sessidx"));
    cmd.arg("--db").arg(&store.path);
    for r in roots {
        cmd.arg("--root")
            .arg(format!("{}={}", r.harness, r.path.display()));
    }
    cmd.args(args);
    cmd
}

fn stream(out: &Output, exit: i32) -> Vec<Value> {
    assert_eq!(
        out.status.code(),
        Some(exit),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let records: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(!records.is_empty());
    assert!(records.iter().all(|v| v["type"].is_string()));
    for record in records.iter().filter(|v| v["type"] == "session") {
        assert_eq!(record["path"], record["hits"][0]["path"]);
    }
    assert_eq!(records.iter().filter(|v| v["type"] == "end").count(), 1);
    assert_eq!(records.last().unwrap()["type"], "end");
    assert!(records.last().unwrap().get("searched").is_some());
    records
}

#[test]
fn cli_static_help_and_version_preserve_locators_and_errors_redact_values() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    let invoke = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sessidx"))
            .arg("--db")
            .arg(&db)
            .args(args)
            .output()
            .unwrap()
    };
    let help = invoke(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(help.stderr.is_empty());
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("sessidx show /path/to/session.jsonl:42")
    );
    let version = invoke(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        concat!("sessidx ", env!("CARGO_PKG_VERSION"), "\n")
    );
    let value = "zQ8vN2rK7xP4mT9aF6wH3cS5uD1jL0eB_yGqR1";
    let error = invoke(&["search", "needle", "--harness", value]);
    let rows = stream(&error, 2);
    assert!(rows[0]["error"].as_str().unwrap().contains("[REDACTED]"));
    assert!(!String::from_utf8_lossy(&error.stdout).contains(value));
    assert!(error.stderr.is_empty());
    assert!(!db.exists());
}

#[test]
fn skill_names_only_commands_and_options_the_cli_accepts() {
    let skill = include_str!("../skills/sessidx/SKILL.md");
    let mut help = String::new();
    let verbs = regex::Regex::new(r"`sessidx ([a-z]+)").unwrap();
    for verb in verbs.captures_iter(skill).map(|c| c[1].to_owned()) {
        let out = Command::new(env!("CARGO_BIN_EXE_sessidx"))
            .args([&verb, "--help"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "sessidx {verb} --help");
        help.push_str(&String::from_utf8_lossy(&out.stdout));
    }
    let options = regex::Regex::new(r"--[a-z][a-z-]*").unwrap();
    for option in options.find_iter(skill).map(|m| m.as_str()) {
        let listed = regex::Regex::new(&format!("{}(?:[^a-z-]|$)", regex::escape(option))).unwrap();
        assert!(listed.is_match(&help), "{option} is not a sessidx option");
    }
}

#[test]
fn sql_without_an_index_names_the_build_command() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sessidx"))
        .arg("--db")
        .arg(dir.path().join("index.db"))
        .args(["sql", "SELECT 1"])
        .output()
        .unwrap();
    let rows = stream(&out, 2);
    assert!(
        rows[0]["error"]
            .as_str()
            .unwrap()
            .contains("run sessidx index")
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn cli_validates_enum_values_and_unions_harnesses() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (_dir, store, roots) = fixture();
    for (flag, bad, values) in [
        ("--harness", "claude_code", vec!["claude", "codex", "pi"]),
        (
            "--role",
            "toolResult",
            vec![
                "user",
                "assistant",
                "tool",
                "system",
                "developer",
                "unknown",
            ],
        ),
        ("--kind", "main", vec!["unknown", "delegated"]),
    ] {
        let rows = stream(
            &command(&store, &roots, &["search", "sharedneedle", flag, bad])
                .output()
                .unwrap(),
            2,
        );
        let error = rows[0]["error"].as_str().unwrap();
        for value in values {
            assert!(error.contains(value));
        }
        assert!(error.contains("possible values"));
    }
    let rows = stream(
        &command(
            &store,
            &roots,
            &[
                "search",
                "sharedneedle",
                "--harness",
                "claude",
                "--harness",
                "pi",
                "--role",
                "user",
                "--kind",
                "unknown",
            ],
        )
        .output()
        .unwrap(),
        0,
    );
    let data = &rows[..rows.len() - 1];
    assert_eq!(data.len(), 2);
    assert!(
        data.iter()
            .all(|v| v["harness"] == "claude" || v["harness"] == "pi")
    );
    for flag in ["--json", "--scan", "--file", "--offset"] {
        stream(
            &command(&store, &roots, &["search", "sharedneedle", flag, "1"])
                .output()
                .unwrap(),
            2,
        );
    }
    let rows = stream(
        &command(
            &store,
            &roots,
            &[
                "search",
                "sharedneedle",
                "--session",
                roots[0].path.to_str().unwrap(),
            ],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(rows.len(), 2);
    assert!(
        rows[..1]
            .iter()
            .all(|r| r["hits"][0]["path"] == roots[0].path.to_str().unwrap())
    );
}

#[test]
fn cli_streams_end_coverage_and_query_bound_cursors() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (_dir, store, roots) = fixture();
    for (args, kind, exit) in [
        (vec!["search", "sharedneedle"], "session", 0),
        (
            vec!["grep", "sharedneedle", "--harness", "claude"],
            "hit",
            0,
        ),
        (vec!["show", "claude-session"], "record", 0),
        (vec!["count", "commands"], "count", 0),
        (vec!["sql", "SELECT 'row' AS type, 1 AS n"], "row", 0),
        (vec!["index"], "index", 0),
        (vec!["doctor"], "doctor", 0),
    ] {
        let rows = stream(&command(&store, &roots, &args).output().unwrap(), exit);
        assert!(rows[..rows.len() - 1].iter().all(|v| v["type"] == kind));
        assert_eq!(rows.last().unwrap()["complete"], true);
        if kind == "record" {
            assert!(rows[0]["text"].as_str().unwrap().contains("sharedneedle"));
        }
        if kind == "row" {
            assert_eq!(rows[0]["data"]["type"], "row");
        }
    }
    let rows = stream(
        &command(&store, &roots, &["search", "absentneedle"])
            .output()
            .unwrap(),
        1,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["complete"], true);
    assert!(rows[0]["next"].is_null());
    for args in [
        vec!["search", "sharedneedle"],
        vec!["grep", "sharedneedle", "--harness", "claude"],
        vec!["show", "claude-session"],
    ] {
        let mut first = args.clone();
        first.extend(["--limit", "1"]);
        let rows = stream(&command(&store, &roots, &first).output().unwrap(), 0);
        assert_eq!(rows.last().unwrap()["complete"], false);
        let cursor = rows.last().unwrap()["next"].as_str().unwrap();
        let mut second = first.clone();
        second.extend(["--cursor", cursor]);
        let page = stream(&command(&store, &roots, &second).output().unwrap(), 0);
        let reference = if args[0] == "search" {
            &page[0]["hits"][0]["ref"]
        } else {
            &page[0]["ref"]
        };
        let previous = if args[0] == "search" {
            &rows[0]["hits"][0]["ref"]
        } else {
            &rows[0]["ref"]
        };
        assert_ne!(reference, previous);
        if args[0] != "show" {
            second[1] = "changedneedle";
            stream(&command(&store, &roots, &second).output().unwrap(), 2);
        }
    }
    stream(
        &command(&store, &roots, &["grep", "absentneedle"])
            .output()
            .unwrap(),
        2,
    );
    stream(
        &command(&store, &roots, &["sql", "DELETE FROM files"])
            .output()
            .unwrap(),
        2,
    );
    let mut final_cursor = String::new();
    for verb in ["search", "grep", "show"] {
        let mut args = match verb {
            "show" => vec!["show", "claude-session"],
            "grep" => vec!["grep", "sharedneedle", "--harness", "claude"],
            _ => vec!["search", "sharedneedle"],
        };
        args.extend(["--limit", "1"]);
        let first = stream(&command(&store, &roots, &args).output().unwrap(), 0);
        let cursor = first.last().unwrap()["next"].as_str().unwrap().to_owned();
        *args.last_mut().unwrap() = "1000";
        args.extend(["--cursor", &cursor]);
        let expected = stream(&command(&store, &roots, &args).output().unwrap(), 0);
        let root = if verb == "search" {
            roots
                .iter()
                .find(|r| r.harness == expected[0]["harness"])
                .unwrap()
        } else {
            &roots[0]
        };
        let id = format!("added-{verb}");
        let record = match root.harness.as_str() {
            "claude" => {
                json!({"type":"user","uuid":id,"sessionId":"claude-session","timestamp":"2030-01-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}})
            }
            "codex" => {
                json!({"type":"response_item","timestamp":"2030-01-01T00:00:00Z","payload":{"type":"message","id":id,"role":"user","content":[{"type":"input_text","text":"sharedneedle"}]}})
            }
            _ => {
                json!({"type":"message","id":id,"timestamp":"2030-01-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}})
            }
        };
        let mut data = fs::read_to_string(&root.path).unwrap();
        data.push_str(&format!("{record}\n"));
        fs::write(&root.path, data).unwrap();
        let appended = stream(&command(&store, &roots, &args).output().unwrap(), 0);
        assert_eq!(
            &appended[..appended.len() - 1],
            &expected[..expected.len() - 1]
        );
        final_cursor = cursor;
    }
    fs::write(&roots[0].path, "{\"type\":\"user\",\"uuid\":\"replacement\",\"sessionId\":\"claude-session\",\"message\":{\"role\":\"user\",\"content\":\"sharedneedle\"}}\n").unwrap();
    let invalid = stream(
        &command(
            &store,
            &roots,
            &[
                "show",
                "claude-session",
                "--limit",
                "1",
                "--cursor",
                &final_cursor,
            ],
        )
        .output()
        .unwrap(),
        2,
    );
    assert!(
        invalid[0]["error"]
            .as_str()
            .unwrap()
            .contains("restart without --cursor")
    );
    let fresh = stream(
        &command(&store, &roots, &["search", "sharedneedle", "--limit", "1"])
            .output()
            .unwrap(),
        0,
    );
    let cursor = fresh.last().unwrap()["next"].as_str().unwrap();
    stream(
        &command(&store, &roots, &["index", "--full"])
            .output()
            .unwrap(),
        0,
    );
    let rebuilt = stream(
        &command(
            &store,
            &roots,
            &["search", "sharedneedle", "--limit", "1", "--cursor", cursor],
        )
        .output()
        .unwrap(),
        2,
    );
    assert!(
        rebuilt[0]["error"]
            .as_str()
            .unwrap()
            .contains("restart without --cursor")
    );
}

#[test]
fn a_closed_stdout_ends_quietly() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (_dir, store, roots) = fixture();
    // Output larger than the pipe buffer forces a write after the reader is gone.
    let query = "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000) SELECT i, printf('%040d', i) AS pad FROM n";
    let mut child = command(&store, &roots, &["sql", query])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn search_pagination_is_bounded_by_a_frozen_session_prefix() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    fs::create_dir(&logs).unwrap();
    let reachable = sessidx::query::SEARCH_PAGE_SESSIONS;
    for i in 0..2 * reachable {
        fs::write(
            logs.join(format!("bound-{i:04}.jsonl")),
            format!("{}\n", json!({"type":"message","id":format!("m-{i}"),"message":{"role":"user","content":"boundneedle"}})),
        )
        .unwrap();
    }
    let store = Store::open(&dir.path().join("index.db")).unwrap();
    let roots = [Root {
        harness: "pi".into(),
        path: logs,
    }];
    let half = (reachable / 2).to_string();
    let first = stream(
        &command(&store, &roots, &["search", "boundneedle", "--limit", &half])
            .output()
            .unwrap(),
        0,
    );
    let cursor = first.last().unwrap()["next"].as_str().unwrap();
    // The cursor carries one integer per reachable session, never one per match.
    assert!(cursor.len() < 16 * reachable, "{} bytes", cursor.len());
    let second = stream(
        &command(
            &store,
            &roots,
            &[
                "search",
                "boundneedle",
                "--limit",
                &half,
                "--cursor",
                cursor,
            ],
        )
        .output()
        .unwrap(),
        0,
    );
    let end = second.last().unwrap();
    assert_eq!(end["complete"], false);
    assert!(end["next"].is_null());
    let sessions: std::collections::HashSet<_> = first
        .iter()
        .chain(&second)
        .filter(|r| r["type"] == "session")
        .map(|r| r["session_id"].clone())
        .collect();
    assert_eq!(sessions.len(), reachable);
}

#[test]
fn cli_writer_contention_returns_stale_without_waiting() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (_dir, store, roots) = fixture();
    let _lock = store.lock().unwrap().unwrap();
    store.db.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let start = Instant::now();
    let mut child = command(&store, &roots, &["search", "sharedneedle"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("query waited on the writer lock");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let rows = stream(&child.wait_with_output().unwrap(), 0);
    let end = rows.last().unwrap();
    assert_eq!(end["stale"], true);
    assert_eq!(end["complete"], false);
    assert_eq!(end["refresh"]["writer_busy"], true);
    let rows = stream(
        &command(&store, &roots, &["sql", "SELECT count(*) AS n FROM events"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(rows.last().unwrap()["stale"], true);
    assert_eq!(rows.last().unwrap()["complete"], false);
}

#[test]
fn search_cursor_freezes_order_when_append_changes_fts_statistics() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (dir, store, mut roots) = fixture();
    for (name, text, year) in [
        ("rank-a", "alpha alpha alpha alpha beta", 2030),
        ("rank-b", "alpha beta beta beta beta", 2029),
    ] {
        let path = dir.path().join(format!("{name}.jsonl"));
        fs::write(&path, format!("{}\n", json!({"type":"message","id":name,"timestamp":format!("{year}-01-01T00:00:00Z"),"message":{"role":"user","content":text}}))).unwrap();
        roots.push(Root {
            harness: "pi".into(),
            path,
        });
    }
    let first = stream(
        &command(&store, &roots, &["search", "alpha beta", "--limit", "1"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(first[0]["session_id"], "rank-a");
    let cursor = first.last().unwrap()["next"].as_str().unwrap();
    let mut data = fs::read_to_string(&roots[0].path).unwrap();
    for i in 0..100 {
        data.push_str(&format!("{}\n",json!({"type":"user","uuid":format!("idf-{i}"),"sessionId":"claude-session","message":{"role":"user","content":"alpha"}})));
    }
    fs::write(&roots[0].path, data).unwrap();
    let second = stream(
        &command(
            &store,
            &roots,
            &["search", "alpha beta", "--limit", "1", "--cursor", cursor],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(second[0]["session_id"], "rank-b");
    let fresh = stream(
        &command(&store, &roots, &["search", "alpha beta", "--limit", "1"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(fresh[0]["session_id"], "rank-b");
    let newer = dir.path().join("newer.jsonl");
    fs::write(&newer,"{\"type\":\"message\",\"id\":\"newer\",\"message\":{\"role\":\"user\",\"content\":\"ordinary\"}}\n").unwrap();
    roots.push(Root {
        harness: "pi".into(),
        path: newer.clone(),
    });
    stream(&command(&store, &roots, &["index"]).output().unwrap(), 0);
    fs::write(newer,"{\"type\":\"message\",\"id\":\"replaced-newer\",\"message\":{\"role\":\"user\",\"content\":\"different\"}}\n").unwrap();
    let valid = stream(
        &command(
            &store,
            &roots,
            &["search", "alpha beta", "--limit", "1", "--cursor", cursor],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(valid[0]["session_id"], "rank-b");
}
