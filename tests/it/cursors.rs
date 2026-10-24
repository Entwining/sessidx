use crate::common::{indexed_files, serial, sessidx, stream, three_agents};
use serde_json::json;
use sessidx::{
    discovery::Root,
    model::{Agent, name},
    store::Store,
};
use std::fs;

#[test]
fn cli_streams_end_coverage_and_query_bound_cursors() {
    let serial = serial();
    let (_dir, store, roots) = three_agents();
    for (args, kind, exit) in [
        (vec!["search", "sharedneedle"], "session", 0),
        (vec!["grep", "sharedneedle", "--agent", "claude"], "hit", 0),
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
        let end = rows.last().unwrap();
        assert_eq!(end["complete"], true);
        if kind != "row" {
            assert_eq!(end.get("stale"), Some(&json!(false)), "{args:?}");
        }
        if kind == "record" {
            assert_eq!(rows[0]["record"]["message"]["content"], "sharedneedle");
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
        vec!["grep", "sharedneedle", "--agent", "claude"],
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
            "grep" => vec!["grep", "sharedneedle", "--agent", "claude"],
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
                .find(|r| name(r.agent) == expected[0]["agent"])
                .unwrap()
        } else {
            &roots[0]
        };
        let id = format!("added-{verb}");
        let record = match root.agent {
            Agent::Claude => {
                json!({"type":"user","uuid":id,"sessionId":"claude-session","timestamp":"2030-01-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}})
            }
            Agent::Codex => {
                json!({"type":"response_item","timestamp":"2030-01-01T00:00:00Z","payload":{"type":"message","id":id,"role":"user","content":[{"type":"input_text","text":"sharedneedle"}]}})
            }
            Agent::Pi => {
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
        agent: Agent::Pi,
        path: logs,
    }];
    let half = (reachable / 2).to_string();
    let mut sessions = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    let end = loop {
        pages += 1;
        assert!(
            sessions.len() <= reachable && pages <= reachable,
            "search does not stop"
        );
        let mut args = vec!["search", "boundneedle", "--limit", &half];
        args.extend(cursor.iter().flat_map(|c| ["--cursor", c.as_str()]));
        let page = stream(
            &sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap(),
            0,
        );
        sessions.extend(
            page.iter()
                .filter(|r| r["type"] == "session")
                .map(|r| r["session_id"].clone()),
        );
        let end = page.last().unwrap().clone();
        match end["next"].as_str() {
            // The cursor carries one integer per reachable session, never one per match.
            Some(next) => {
                assert!(next.len() < 16 * reachable, "{} bytes", next.len());
                cursor = Some(next.to_owned());
            }
            None => break end,
        }
    };
    assert_eq!(end["complete"], false);
    assert_eq!(sessions.len(), reachable);
}

#[test]
fn search_cursor_freezes_order_when_append_changes_fts_statistics() {
    let serial = serial();
    let (dir, store, mut roots) = three_agents();
    for (name, text, year) in [
        ("rank-a", "alpha alpha alpha alpha beta", 2030),
        ("rank-b", "alpha beta beta beta beta", 2029),
    ] {
        let path = dir.path().join(format!("{name}.jsonl"));
        fs::write(&path, format!("{}\n", json!({"type":"message","id":name,"timestamp":format!("{year}-01-01T00:00:00Z"),"message":{"role":"user","content":text}}))).unwrap();
        roots.push(Root {
            agent: Agent::Pi,
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
        agent: Agent::Pi,
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
    let (_dir, store, roots) = three_agents();
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
        ("version", json!(3)),
        ("offset", json!(-1)),
        ("after", json!([-1, 0, 0])),
        ("position", json!(0)),
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
                .contains("without --cursor"),
            "{label}"
        );
    }
}

#[test]
fn cursors_survive_changes_to_files_they_already_read() {
    let serial = serial();
    let line = |session: &str| {
        format!(
            "{}\n",
            json!({"type":"user","uuid":format!("{session}-1"),"sessionId":session,"timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"pageneedle"}})
        )
    };
    let (_dir, store, roots) = indexed_files(
        Agent::Claude,
        &[("a", &line("a")), ("b", &line("b")), ("c", &line("c"))],
    );
    let root = roots[0].path.clone();
    let run = |args: &[&str], exit| {
        stream(
            &sessidx(&serial, &store.path, &roots, args)
                .output()
                .unwrap(),
            exit,
        )
    };
    let refused = |rows: &[serde_json::Value]| {
        rows[0]["error"]
            .as_str()
            .unwrap()
            .contains("restart without --cursor")
    };
    // grep reads transcripts in file ID order, here a, b, then c.
    let grep = ["grep", "pageneedle", "--agent", "claude", "--limit"];
    let first = run(&[&grep[..], &["2"]].concat(), 0);
    let cursor = first.last().unwrap()["next"].as_str().unwrap().to_owned();
    fs::remove_file(root.join("a.jsonl")).unwrap();
    let rest = run(&[&grep[..], &["2", "--cursor", &cursor]].concat(), 0);
    assert_eq!(rest.len(), 2);
    assert!(rest[0]["ref"].as_str().unwrap().contains("/c.jsonl:"));
    assert_eq!(rest[1]["complete"], true);
    let first = run(&[&grep[..], &["1"]].concat(), 0);
    let cursor = first.last().unwrap()["next"].as_str().unwrap().to_owned();
    fs::remove_file(root.join("c.jsonl")).unwrap();
    assert!(refused(&run(
        &[&grep[..], &["1", "--cursor", &cursor]].concat(),
        2
    )));

    // A search cursor names the transcripts of its frozen ranking.
    for session in ["a", "c"] {
        fs::write(root.join(format!("{session}.jsonl")), line(session)).unwrap();
    }
    let search = ["search", "pageneedle", "--limit", "1"];
    let first = run(&search, 0);
    let cursor = first.last().unwrap()["next"].as_str().unwrap().to_owned();
    let shown = first[0]["path"].as_str().unwrap().to_owned();
    fs::remove_file(&shown).unwrap();
    let second = run(&[&search[..], &["--cursor", &cursor]].concat(), 0);
    let cursor = second.last().unwrap()["next"].as_str().unwrap().to_owned();
    let unread = ["a", "b", "c"]
        .map(|s| root.join(format!("{s}.jsonl")))
        .into_iter()
        .find(|p| p.to_str() != Some(&shown) && p.to_str() != second[0]["path"].as_str())
        .unwrap();
    fs::remove_file(unread).unwrap();
    assert!(refused(&run(
        &[&search[..], &["--cursor", &cursor]].concat(),
        2
    )));
}

#[test]
fn search_pages_pass_over_frozen_sessions_that_stopped_matching() {
    let serial = serial();
    let line = |session: &str, day: u32| {
        format!(
            "{}\n",
            json!({"type":"user","uuid":format!("{session}-1"),"sessionId":session,"timestamp":format!("2026-10-0{day}T00:00:00Z"),"message":{"role":"user","content":"pageneedle"}})
        )
    };
    let sessions: Vec<(String, String)> = (1..=5)
        .map(|day| (format!("s{day}"), line(&format!("s{day}"), day)))
        .collect();
    let named: Vec<(&str, &str)> = sessions
        .iter()
        .map(|(s, l)| (s.as_str(), l.as_str()))
        .collect();
    let (_dir, store, roots) = indexed_files(Agent::Claude, &named);
    let page = |cursor: Option<&str>| {
        let mut args = vec![
            "search",
            "pageneedle",
            "--session-kind",
            "unknown",
            "--limit",
            "1",
        ];
        args.extend(cursor.iter().flat_map(|c| ["--cursor", c]));
        let rows = stream(
            &sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap(),
            0,
        );
        let ids: Vec<String> = rows
            .iter()
            .filter(|r| r["type"] == "session")
            .map(|r| r["session_id"].as_str().unwrap().to_owned())
            .collect();
        (ids, rows.last().unwrap().clone())
    };
    let (first, mut end) = page(None);
    let mut pages = vec![first];
    // s4, next in the frozen order, and s1, the last, become main sessions.
    for (session, day) in [("s4", 4), ("s1", 1)] {
        let path = roots[0].path.join(format!("{session}.jsonl"));
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str(&format!("{}\n", json!({"type":"user","uuid":format!("{session}-2"),"sessionId":session,"isSidechain":false,"timestamp":format!("2026-10-0{day}T00:00:01Z"),"message":{"role":"user","content":"later"}})));
        fs::write(&path, text).unwrap();
    }
    while let Some(cursor) = end["next"].as_str().map(str::to_owned) {
        assert!(pages.len() < 6, "search does not advance: {pages:?}");
        let (ids, next) = page(Some(&cursor));
        pages.push(ids);
        end = next;
    }
    // A partial page resumes after the session it returned; a page that
    // returns every remaining match is the last.
    assert_eq!(pages, [["s5"], ["s3"], ["s2"]]);
    assert_eq!(end["complete"], true);
}
