use crate::common::{serial, sessidx, stream, three_harnesses};
use serde_json::json;
use sessidx::{
    discovery::Root,
    model::{Harness, name},
    store::Store,
};
use std::fs;

#[test]
fn cli_streams_end_coverage_and_query_bound_cursors() {
    let serial = serial();
    let (_dir, store, roots) = three_harnesses();
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
        let rows = stream(
            &sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap(),
            exit,
        );
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
        &sessidx(&serial, &store.path, &roots, &["search", "absentneedle"])
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
        let rows = stream(
            &sessidx(&serial, &store.path, &roots, &first)
                .output()
                .unwrap(),
            0,
        );
        assert_eq!(rows.last().unwrap()["complete"], false);
        let cursor = rows.last().unwrap()["next"].as_str().unwrap();
        let mut second = first.clone();
        second.extend(["--cursor", cursor]);
        let page = stream(
            &sessidx(&serial, &store.path, &roots, &second)
                .output()
                .unwrap(),
            0,
        );
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
            stream(
                &sessidx(&serial, &store.path, &roots, &second)
                    .output()
                    .unwrap(),
                2,
            );
        }
    }
    stream(
        &sessidx(&serial, &store.path, &roots, &["grep", "absentneedle"])
            .output()
            .unwrap(),
        2,
    );
    stream(
        &sessidx(&serial, &store.path, &roots, &["sql", "DELETE FROM files"])
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
        let first = stream(
            &sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap(),
            0,
        );
        let cursor = first.last().unwrap()["next"].as_str().unwrap().to_owned();
        *args.last_mut().unwrap() = "1000";
        args.extend(["--cursor", &cursor]);
        let expected = stream(
            &sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap(),
            0,
        );
        let root = if verb == "search" {
            roots
                .iter()
                .find(|r| name(r.harness) == expected[0]["harness"])
                .unwrap()
        } else {
            &roots[0]
        };
        let id = format!("added-{verb}");
        let record = match root.harness {
            Harness::Claude => {
                json!({"type":"user","uuid":id,"sessionId":"claude-session","timestamp":"2030-01-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}})
            }
            Harness::Codex => {
                json!({"type":"response_item","timestamp":"2030-01-01T00:00:00Z","payload":{"type":"message","id":id,"role":"user","content":[{"type":"input_text","text":"sharedneedle"}]}})
            }
            Harness::Pi => {
                json!({"type":"message","id":id,"timestamp":"2030-01-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}})
            }
        };
        let mut data = fs::read_to_string(&root.path).unwrap();
        data.push_str(&format!("{record}\n"));
        fs::write(&root.path, data).unwrap();
        let appended = stream(
            &sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap(),
            0,
        );
        assert_eq!(
            &appended[..appended.len() - 1],
            &expected[..expected.len() - 1]
        );
        final_cursor = cursor;
    }
    fs::write(&roots[0].path, "{\"type\":\"user\",\"uuid\":\"replacement\",\"sessionId\":\"claude-session\",\"message\":{\"role\":\"user\",\"content\":\"sharedneedle\"}}\n").unwrap();
    let invalid = stream(
        &sessidx(
            &serial,
            &store.path,
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
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["search", "sharedneedle", "--limit", "1"],
        )
        .output()
        .unwrap(),
        0,
    );
    let cursor = fresh.last().unwrap()["next"].as_str().unwrap();
    stream(
        &sessidx(&serial, &store.path, &roots, &["index", "--full"])
            .output()
            .unwrap(),
        0,
    );
    let rebuilt = stream(
        &sessidx(
            &serial,
            &store.path,
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
fn search_pagination_is_bounded_by_a_frozen_session_prefix() {
    let serial = serial();
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
        harness: Harness::Pi,
        path: logs,
    }];
    let half = (reachable / 2).to_string();
    let first = stream(
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["search", "boundneedle", "--limit", &half],
        )
        .output()
        .unwrap(),
        0,
    );
    let cursor = first.last().unwrap()["next"].as_str().unwrap();
    // The cursor carries one integer per reachable session, never one per match.
    assert!(cursor.len() < 16 * reachable, "{} bytes", cursor.len());
    let second = stream(
        &sessidx(
            &serial,
            &store.path,
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
fn search_cursor_freezes_order_when_append_changes_fts_statistics() {
    let serial = serial();
    let (dir, store, mut roots) = three_harnesses();
    for (name, text, year) in [
        ("rank-a", "alpha alpha alpha alpha beta", 2030),
        ("rank-b", "alpha beta beta beta beta", 2029),
    ] {
        let path = dir.path().join(format!("{name}.jsonl"));
        fs::write(&path, format!("{}\n", json!({"type":"message","id":name,"timestamp":format!("{year}-01-01T00:00:00Z"),"message":{"role":"user","content":text}}))).unwrap();
        roots.push(Root {
            harness: Harness::Pi,
            path,
        });
    }
    let first = stream(
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["search", "alpha beta", "--limit", "1"],
        )
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
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["search", "alpha beta", "--limit", "1", "--cursor", cursor],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(second[0]["session_id"], "rank-b");
    let fresh = stream(
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["search", "alpha beta", "--limit", "1"],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(fresh[0]["session_id"], "rank-b");
    let newer = dir.path().join("newer.jsonl");
    fs::write(&newer,"{\"type\":\"message\",\"id\":\"newer\",\"message\":{\"role\":\"user\",\"content\":\"ordinary\"}}\n").unwrap();
    roots.push(Root {
        harness: Harness::Pi,
        path: newer.clone(),
    });
    stream(
        &sessidx(&serial, &store.path, &roots, &["index"])
            .output()
            .unwrap(),
        0,
    );
    fs::write(newer,"{\"type\":\"message\",\"id\":\"replaced-newer\",\"message\":{\"role\":\"user\",\"content\":\"different\"}}\n").unwrap();
    let valid = stream(
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["search", "alpha beta", "--limit", "1", "--cursor", cursor],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(valid[0]["session_id"], "rank-b");
}

#[test]
fn garbage_and_edited_cursor_fields_are_rejected() {
    let serial = serial();
    let (_dir, store, roots) = three_harnesses();
    let search = |cursor: &str, exit| {
        let out = sessidx(
            &serial,
            &store.path,
            &roots,
            &["search", "sharedneedle", "--limit", "1", "--cursor", cursor],
        )
        .output()
        .unwrap();
        stream(&out, exit)
    };
    let first = stream(
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["search", "sharedneedle", "--limit", "1"],
        )
        .output()
        .unwrap(),
        0,
    );
    let cursor = first.last().unwrap()["next"].as_str().unwrap();
    let garbage = search("not-a-cursor", 2);
    assert!(
        garbage[0]["error"]
            .as_str()
            .unwrap()
            .contains("invalid cursor")
    );
    let original: serde_json::Value = serde_json::from_str(cursor).unwrap();
    search(&original.to_string(), 0);
    let revision = original["revision"].as_i64().unwrap();
    for (field, value) in [
        ("version", json!(2)),
        ("position", json!(-1)),
        ("high_water", json!(-1)),
        ("revision", json!(-1)),
        ("revision", json!(revision + 1)),
    ] {
        let label = format!("{field}={value}");
        let mut edited = original.clone();
        edited[field] = value;
        let rows = search(&edited.to_string(), 2);
        assert!(
            rows[0]["error"]
                .as_str()
                .unwrap()
                .contains("restart without --cursor"),
            "{label}"
        );
    }
}
