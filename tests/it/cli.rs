use crate::common::{serial, sessidx, stream, three_harnesses};
use serde_json::json;
use std::{
    fs,
    process::Stdio,
    time::{Duration, Instant},
};

#[test]
fn cli_static_help_and_version_preserve_locators_and_errors_redact_values() {
    let serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    let invoke = |args: &[&str]| sessidx(&serial, &db, &[], args).output().unwrap();
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
fn sql_without_an_index_names_the_build_command() {
    let serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    let out = sessidx(&serial, &db, &[], &["sql", "SELECT 1"])
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
    let out = sessidx(&serial, &db, &[], &["index"]).output().unwrap();
    let home = dir.path().display();
    assert_eq!(
        stream(&out, 0)[0]["missing_roots"],
        json!([
            format!("claude={home}/.claude/projects"),
            format!("codex={home}/.codex/sessions"),
            format!("codex={home}/.codex/archived_sessions"),
            format!("pi={home}/.pi/agent/sessions"),
        ])
    );
}

#[test]
fn cli_validates_enum_values_and_unions_harnesses() {
    let serial = serial();
    let (_dir, store, roots) = three_harnesses();
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
            &sessidx(
                &serial,
                &store.path,
                &roots,
                &["search", "sharedneedle", flag, bad],
            )
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
        &sessidx(
            &serial,
            &store.path,
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
            &sessidx(
                &serial,
                &store.path,
                &roots,
                &["search", "sharedneedle", flag, "1"],
            )
            .output()
            .unwrap(),
            2,
        );
    }
    let rows = stream(
        &sessidx(
            &serial,
            &store.path,
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
fn a_closed_stdout_ends_quietly() {
    let serial = serial();
    let (_dir, store, roots) = three_harnesses();
    // Output larger than the pipe buffer forces a write after the reader is gone.
    let query = "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<5000) SELECT i, printf('%040d', i) AS pad FROM n";
    let mut child = sessidx(&serial, &store.path, &roots, &["sql", query])
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
fn cli_writer_contention_returns_stale_without_waiting() {
    let serial = serial();
    let (_dir, store, roots) = three_harnesses();
    let _lock = store.lock().unwrap().unwrap();
    store.db.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let start = Instant::now();
    let mut child = sessidx(&serial, &store.path, &roots, &["search", "sharedneedle"])
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
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["sql", "SELECT count(*) AS n FROM events"],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(rows.last().unwrap()["stale"], true);
    assert_eq!(rows.last().unwrap()["complete"], false);
    assert_eq!(rows.last().unwrap()["refresh"]["writer_busy"], true);
}

#[test]
fn cli_rejects_malformed_values_in_one_end_record_naming_the_flag() {
    let serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    for (args, expected) in [
        (
            vec!["search", "x", "--limit", "0"],
            vec!["'--limit <LIMIT>'", "1..=1000"],
        ),
        (
            vec!["show", "x", "--limit", "1001"],
            vec!["'--limit <LIMIT>'", "1..=1000"],
        ),
        (
            vec!["count", "commands", "--by", "harness,week,foo"],
            vec!["'foo' for '--by", "harness, model, role, week, kind"],
        ),
        (
            vec!["--root", "claude_code=/synthetic", "sql", "SELECT 1"],
            vec![
                "'--root <HARNESS=PATH>'",
                "'claude_code'",
                "claude, codex, pi",
            ],
        ),
        (
            vec!["--root", "/synthetic", "index"],
            vec!["'--root <HARNESS=PATH>'", "HARNESS=PATH"],
        ),
        (
            vec!["grep", "x", "--since", "2026-13-01"],
            vec!["'--since <TIME>'", "RFC 3339 or YYYY-MM-DD"],
        ),
        (
            vec!["search", "x", "--until", "yesterday"],
            vec!["'--until <TIME>'", "RFC 3339 or YYYY-MM-DD"],
        ),
        (
            vec!["count", "commands", "--by", "harness", "--by", "model"],
            vec!["'--by [<BY>]' cannot be used multiple times"],
        ),
        (
            vec!["count", "commands", "--by", "model,role,model"],
            vec!["--by repeats model"],
        ),
    ] {
        let out = sessidx(&serial, &db, &[], &args).output().unwrap();
        assert!(!db.exists(), "{args:?} opened the store");
        let rows = stream(&out, 2);
        let error = rows[0]["error"].as_str().unwrap();
        for part in expected {
            assert!(error.contains(part), "{args:?}: {error}");
        }
        assert!(!error.contains('\u{1b}'), "{args:?}: {error}");
        assert!(out.stderr.is_empty());
    }
}

#[test]
fn cli_count_groupings_and_date_bounds_parse_at_the_boundary() {
    let serial = serial();
    let (_dir, store, roots) = three_harnesses();
    let count = |args: &[&str], exit| {
        let mut args = args.to_vec();
        args.insert(0, "count");
        let rows = stream(
            &sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap(),
            exit,
        );
        rows[..rows.len() - 1].to_vec()
    };
    let keys = |row: &serde_json::Value| {
        let mut keys: Vec<_> = ["harness", "model", "role", "week", "kind"]
            .into_iter()
            .filter(|k| row.get(k).is_some())
            .collect();
        keys.sort();
        keys
    };
    let grouped = count(&["commands"], 0);
    assert_eq!(grouped.len(), 1);
    assert_eq!(keys(&grouped[0]), ["harness", "model", "role", "week"]);
    assert_eq!(grouped[0]["harness"], "claude");
    let total = count(&["commands", "--by"], 0);
    assert_eq!(total.len(), 1);
    assert!(keys(&total[0]).is_empty());
    assert_eq!(total[0]["denominator"], 1);
    let chosen = count(&["commands", "--by", "kind,harness"], 0);
    assert_eq!(keys(&chosen[0]), ["harness", "kind"]);
    let search = |bound: &[&str], exit| {
        let mut args = vec!["search", "sharedneedle"];
        args.extend(bound);
        let out = sessidx(&serial, &store.path, &roots, &args)
            .output()
            .unwrap();
        stream(&out, exit).len() - 1
    };
    // Every synthetic message is at 2026-10-01T00:00:00Z.
    assert_eq!(search(&["--since", "2026-10-01"], 0), 3);
    assert_eq!(search(&["--since", "2026-10-01T00:00:00.001Z"], 1), 0);
    assert_eq!(search(&["--until", "2026-10-01T02:00:00+02:00"], 1), 0);
    assert_eq!(search(&["--until", "2026-10-01T02:00:01+02:00"], 0), 3);
}

#[test]
fn empty_grep_show_count_and_sql_results_exit_one_with_a_complete_end() {
    let serial = serial();
    let (_dir, store, roots) = three_harnesses();
    for args in [
        vec!["grep", "absentneedle", "--harness", "claude"],
        vec!["show", "absent-session"],
        vec!["count", "commands", "--harness", "pi"],
        vec!["sql", "SELECT 1 AS n WHERE 0"],
    ] {
        let rows = stream(
            &sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap(),
            1,
        );
        assert_eq!(rows.len(), 1, "{args:?}");
        assert_eq!(rows[0]["complete"], true, "{args:?}");
        assert!(rows[0].get("error").is_none(), "{args:?}");
    }
}
