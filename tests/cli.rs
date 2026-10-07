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
    let rows = stream(
        &command(&store, &roots, &["search", "sharedneedle", "--limit", "1"])
            .output()
            .unwrap(),
        0,
    );
    let cursor = rows.last().unwrap()["next"].as_str().unwrap();
    let mut data = fs::read_to_string(&roots[0].path).unwrap();
    data.push_str("{\"type\":\"user\",\"uuid\":\"added\",\"sessionId\":\"claude-session\",\"message\":{\"role\":\"user\",\"content\":\"sharedneedle\"}}\n");
    fs::write(&roots[0].path, data).unwrap();
    let invalid = stream(
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
        invalid[0]["error"]
            .as_str()
            .unwrap()
            .contains("index version")
    );
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
