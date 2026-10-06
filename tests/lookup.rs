use sessidx::{
    discovery::Root,
    query::{self, Filters},
    store::Store,
};
use std::{fs, process::Command, time::Duration};

fn setup(harness: &str, content: &str) -> (tempfile::TempDir, Store, Vec<Root>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("logs");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("session.jsonl"), content).unwrap();
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    let roots = vec![Root {
        harness: harness.into(),
        path: root,
    }];
    store.refresh(&roots, false, None).unwrap();
    (dir, store, roots)
}

#[test]
fn lookup_cjk_latin_filters_and_show_references() {
    let (_dir, store, _) = setup("claude", include_str!("../testdata/claude.jsonl"));
    let filters = Filters::default();
    assert_eq!(
        query::search(&store.db, "太长", &filters, 20, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        query::search(&store.db, "Latin", &filters, 20, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        query::search(&store.db, "Lat", &filters, 20, 0)
            .unwrap()
            .len(),
        0
    );
    let fts_body: Option<String> = store
        .db
        .query_row("SELECT text FROM fts WHERE fts MATCH 'Latin'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(fts_body.is_none());
    assert!(
        query::search(
            &store.db,
            "Latin",
            &Filters {
                harness: Some("pi".into()),
                ..filters.clone()
            },
            20,
            0
        )
        .unwrap()
        .is_empty()
    );
    let hits = query::search(
        &store.db,
        "Latin",
        &Filters {
            cwd: Some("/synthetic".into()),
            ..filters
        },
        20,
        0,
    )
    .unwrap();
    let (shown, coverage) = query::show(
        &store.db,
        &format!("{}:{}", hits[0].path, hits[0].line_no),
        0,
        100,
        0,
    )
    .unwrap();
    assert_eq!(shown.len(), 1);
    assert!(!coverage.incomplete);
    let (_dir, store, _) = setup("codex", include_str!("../testdata/codex.jsonl"));
    assert_eq!(
        query::show(&store.db, "codex://threads/codex-fixture", 3, 100, 0)
            .unwrap()
            .0
            .len(),
        6
    );
}

#[test]
fn scan_requires_filter_reads_only_selected_ranges_and_reports_changed_source() {
    let (dir, store, _) = setup("codex", include_str!("../testdata/codex.jsonl"));
    assert!(
        query::scan(
            &store.db,
            "exited",
            &Filters::default(),
            20,
            0,
            Duration::from_secs(2)
        )
        .is_err()
    );
    let filters = Filters {
        session: Some("codex-fixture".into()),
        role: Some("tool".into()),
        ..Filters::default()
    };
    let path = dir.path().join("logs/session.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b'!';
    fs::write(&path, &bytes).unwrap();
    let (hits, coverage) =
        query::scan(&store.db, "exited", &filters, 20, 0, Duration::from_secs(2)).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(!coverage.incomplete);
    let off = hits[0].byte_off as usize;
    bytes[off + 1] = b'!';
    fs::write(&path, bytes).unwrap();
    let (hits, coverage) =
        query::scan(&store.db, "exited", &filters, 20, 0, Duration::from_secs(2)).unwrap();
    assert!(hits.is_empty());
    assert_eq!(coverage.unavailable_ranges, 1);
    assert!(coverage.incomplete);
}

#[test]
fn writer_lock_budget_missing_root_and_scan_cursor_are_visible() {
    let (_dir, mut store, roots) = setup("codex", include_str!("../testdata/codex.jsonl"));
    let lock = store.lock().unwrap().unwrap();
    let busy = store
        .refresh(&roots, false, Some(Duration::from_secs(2)))
        .unwrap();
    assert!(busy.stale && busy.writer_busy);
    drop(lock);
    let budget = store.refresh(&roots, false, Some(Duration::ZERO)).unwrap();
    assert!(budget.stale && budget.continuation.is_some());
    let missing = store
        .refresh(
            &[Root {
                harness: "pi".into(),
                path: roots[0].path.join("absent"),
            }],
            false,
            None,
        )
        .unwrap();
    assert_eq!(missing.missing_roots, ["pi"]);
    assert!(missing.stale);
    let filters = Filters {
        harness: Some("codex".into()),
        ..Filters::default()
    };
    let (hits, c) = query::scan(&store.db, ".", &filters, 1, 0, Duration::from_secs(2)).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(c.incomplete && c.continuation.is_some());
    let (next, _) = query::scan(
        &store.db,
        ".",
        &filters,
        20,
        c.continuation.unwrap(),
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(!next.is_empty());
    assert!(next.iter().all(|h| h.event_id > hits[0].event_id));
}

pub fn canaries() -> Vec<String> {
    (0..6)
        .map(|n| {
            if n % 2 == 0 {
                format!("syntheticCanaryToken{n}ZaQwSxEdCvRfTgBhYj")
            } else {
                format!("zQ8vN2rK7xP4mT9aF6wH3cS5uD1jL0eB_yGqR{o}", o = n)
            }
        })
        .collect()
}

#[test]
fn synthetic_secret_canaries_absent_from_storage_and_lookup_outputs() {
    let values = canaries();
    let line = |id: &str, typ: &str, value: serde_json::Value| {
        serde_json::json!({"type":typ,"uuid":id,"sessionId":"canary-session","cwd":"/synthetic","message":value}).to_string()+"\n"
    };
    let mut data = String::new();
    for (i, value) in values.iter().enumerate() {
        let text = if i % 2 == 0 {
            format!("Bearer {value}")
        } else {
            value.clone()
        };
        let m = match i / 2 {
            0 => serde_json::json!({"role":"user","content":format!("needle {text}")}),
            1 => {
                serde_json::json!({"role":"assistant","model":"model","content":[{"type":"tool_use","id":format!("call-{i}"),"name":"Bash","input":{"command":format!("printf 'needle {text}'")}}]})
            }
            _ => {
                serde_json::json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call-2","is_error":false,"content":format!("needle {text}")}]})
            }
        };
        data += &line(
            &format!("event-{i}"),
            if i / 2 == 1 { "assistant" } else { "user" },
            m,
        );
    }
    let (dir, store, roots) = setup("claude", &data);
    for name in ["index.db", "index.db-wal", "index.db-shm"] {
        if let Ok(bytes) = fs::read(dir.path().join(name)) {
            for v in &values {
                assert!(
                    !bytes.windows(v.len()).any(|b| b == v.as_bytes()),
                    "canary leaked to {name}"
                );
            }
        }
    }
    let mut outputs = Vec::new();
    for args in [
        vec!["index"],
        vec!["search", "needle", "--json"],
        vec![
            "search",
            "--scan",
            "needle",
            "--session",
            "canary-session",
            "--json",
        ],
        vec!["show", "canary-session", "--json"],
        vec!["search", "--scan", "needle"],
        vec!["count", "--metric", "commands", "--json"],
        vec!["count", "--metric", "failures", "--json"],
        vec!["count", "--metric", "denials", "--json"],
        vec!["doctor"],
        vec!["sql", "SELECT * FROM events"],
        vec!["sql", "SELECT * FROM commands"],
        vec!["sql", "SELECT * FROM shapes"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sessidx"))
            .arg("--db")
            .arg(&store.path)
            .arg("--root")
            .arg(format!("claude={}", roots[0].path.display()))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if args == ["search", "--scan", "needle"] {
                2
            } else {
                0
            }),
            "{args:?}"
        );
        outputs.extend_from_slice(&output.stdout);
        outputs.extend_from_slice(&output.stderr);
    }
    for v in &values {
        assert!(
            !outputs.windows(v.len()).any(|b| b == v.as_bytes()),
            "canary leaked to command output"
        );
    }
    for name in ["index.db", "index.db-wal", "index.db-shm"] {
        if let Ok(bytes) = fs::read(dir.path().join(name)) {
            for v in &values {
                assert!(
                    !bytes.windows(v.len()).any(|b| b == v.as_bytes()),
                    "canary leaked after commands to {name}"
                );
            }
        }
    }
}

#[test]
fn adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed() {
    let values: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/privacy.json")).unwrap();
    let text = format!(
        "needle {} Authorization: Basic {} password = \"{}\"",
        values["hex"].as_str().unwrap(),
        values["basic"].as_str().unwrap(),
        values["passphrase"].as_str().unwrap()
    );
    let data=serde_json::json!({"type":"user","uuid":"privacy-user","sessionId":"privacy","message":{"role":"user","content":text}}).to_string()+"\n"+&serde_json::json!({"type":"assistant","uuid":"privacy-input","sessionId":"privacy","message":{"role":"assistant","content":[{"type":"tool_use","id":"privacy-call","name":"Bash","input":{"command":"printf needle","part_one":values["part_one"],"part_two":values["part_two"]}}]}}).to_string()+"\n";
    let (dir, store, _) = setup("claude", &data);
    let output = query::show(&store.db, "privacy", 0, 20, 0)
        .unwrap()
        .0
        .into_iter()
        .map(|h| h.snippet)
        .collect::<Vec<_>>()
        .join("\n");
    for value in values
        .as_object()
        .unwrap()
        .values()
        .filter_map(|v| v.as_str())
    {
        assert!(
            !output.contains(value),
            "adversarial canary leaked on display"
        );
        for name in ["index.db", "index.db-wal", "index.db-shm"] {
            if let Ok(bytes) = fs::read(dir.path().join(name)) {
                assert!(
                    !bytes.windows(value.len()).any(|b| b == value.as_bytes()),
                    "adversarial canary leaked to {name}"
                );
            }
        }
    }
}

#[test]
fn scan_matches_original_ranges_before_redacting_display() {
    let token = "zQ8vN2rK7xP4mT9aF6wH3cS5uD1jL0eB_yGqR1";
    let data=serde_json::json!({"type":"user","uuid":"scan-before-redaction","sessionId":"scan-private","message":{"role":"user","content":format!("needle {token}")}}).to_string()+"\n";
    let (_dir, store, _) = setup("claude", &data);
    let (hits, c) = query::scan(
        &store.db,
        token,
        &Filters {
            session: Some("scan-private".into()),
            ..Filters::default()
        },
        20,
        0,
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(!hits[0].snippet.contains(token));
    assert!(!c.incomplete);
}

#[test]
fn native_identifiers_and_cwd_remain_queryable_with_body_entropy_redaction() {
    let session = "00000000-0000-7000-8000-000000000001";
    let cwd = "/synthetic/Code/GitHub/project-with-native-identifiers";
    let data=serde_json::json!({"type":"session_meta","payload":{"id":session,"cwd":cwd}}).to_string()+"\n"+&serde_json::json!({"type":"turn_context","payload":{"model":"claude-haiku-4-5-20251001"}}).to_string()+"\n"+&serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"needle"}]}}).to_string()+"\n";
    let (_, store, _) = setup("codex", &data);
    let hits = query::search(
        &store.db,
        "needle",
        &Filters {
            session: Some(session.into()),
            cwd: Some(cwd.into()),
            ..Filters::default()
        },
        20,
        0,
    )
    .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].model.as_deref(), Some("claude-haiku-4-5-20251001"));
}

#[test]
fn initial_schema_creation_respects_the_writer_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let store = Store::open(&path).unwrap();
    let lock = store.lock().unwrap().unwrap();
    let mut second = Store::open(&path).unwrap();
    let r = second.refresh(&[], false, None).unwrap();
    assert!(r.writer_busy && r.stale);
    assert_eq!(
        second
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(lock);
    second.refresh(&[], false, None).unwrap();
    assert_eq!(
        second
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
