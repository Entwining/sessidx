use sessidx::{adapters, discovery::Root, model::State, store::Store};
use std::fs;

fn events(h: &str, fixture: &str) -> (State, Vec<sessidx::model::Event>) {
    let mut s = State::default();
    let es = fixture
        .lines()
        .flat_map(|l| adapters::parse(h, &serde_json::from_str(l).unwrap(), &mut s).events)
        .collect();
    (s, es)
}

#[test]
fn claude_blocks_flags_and_synthetic_model() {
    let (s, es) = events("claude", include_str!("../testdata/claude.jsonl"));
    assert_eq!(es.iter().filter(|e| e.kind == "tool_call").count(), 1);
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "flag")
            .count(),
        1
    );
    assert_eq!(s.model.as_deref(), Some("claude-sonnet"));
    assert_eq!(es.iter().filter(|e| e.role == "user").count(), 1);
    assert!(
        es.iter()
            .filter(|e| e.kind == "tool_result")
            .all(|e| e.text.as_ref().is_some_and(|s| !s.is_empty()))
    );
}

#[test]
fn codex_context_arguments_and_telemetry() {
    let (s, es) = events("codex", include_str!("../testdata/codex.jsonl"));
    assert_eq!(s.model.as_deref(), Some("gpt-fixture"));
    assert_eq!(
        es.iter()
            .filter(|e| e.command.as_deref() == Some("rg word src"))
            .count(),
        1
    );
    assert_eq!(es.iter().filter(|e| e.kind == "message").count(), 1);
    assert!(s.instruction_hash.is_some());
    assert!(
        es.iter()
            .filter(|e| e.kind == "tool_result")
            .all(|e| e.text.as_ref().is_some_and(|s| !s.is_empty()))
    );
}

#[test]
fn pi_model_tool_call_and_camel_case_flag() {
    let (s, es) = events("pi", include_str!("../testdata/pi.jsonl"));
    assert_eq!(s.model.as_deref(), Some("pi-model"));
    assert_eq!(es.iter().filter(|e| e.kind == "tool_call").count(), 1);
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "flag")
            .count(),
        1
    );
    assert_eq!(es.iter().filter(|e| e.ok.is_none()).count(), 5);
}

fn snapshot(store: &Store) -> Vec<String> {
    store.db.prepare("SELECT session_id,line_no,byte_off,byte_len,ordinal,role,kind,coalesce(model,''),coalesce(text,''),ok_source FROM events ORDER BY line_no,ordinal").unwrap()
        .query_map([], |r| { Ok((0..10).map(|i| match r.get_ref(i).unwrap() { rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(), rusqlite::types::ValueRef::Integer(n) => n.to_string(), _ => String::new() }).collect::<Vec<_>>().join("|")) }).unwrap().map(Result::unwrap).collect()
}

#[test]
fn previous_schema_requires_explicit_locked_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let old = rusqlite::Connection::open(&path).unwrap();
    old.execute_batch(include_str!("../testdata/schema-v1.sql"))
        .unwrap();
    for args in [
        vec!["search", "needle"],
        vec!["sql", "SELECT 1"],
        vec!["index"],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_sessidx"))
            .arg("--db")
            .arg(&path)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["type"], "end");
        assert_eq!(rows[0]["complete"], false);
        assert_eq!(
            rows[0]["error"],
            "database schema changed; run sessidx index --full"
        );
        assert!(out.stderr.is_empty());
    }
    assert!(Store::open(&path).is_err());
    let root = dir.path().join("logs");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("one.jsonl"),
        include_str!("../testdata/codex.jsonl"),
    )
    .unwrap();
    let roots = [Root {
        harness: "codex".into(),
        path: root,
    }];
    let mut upgrade = Store::open_for_rebuild(&path).unwrap();
    let lock = upgrade.lock().unwrap().unwrap();
    let report = upgrade.refresh(&roots, true, None).unwrap();
    assert!(report.writer_busy && report.stale);
    assert_eq!(
        old.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(lock);
    assert!(!upgrade.refresh(&roots, true, None).unwrap().stale);
    assert_eq!(
        upgrade
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        sessidx::store::SCHEMA_VERSION
    );
    let mut clean = Store::open(&dir.path().join("clean.db")).unwrap();
    clean.refresh(&roots, true, None).unwrap();
    assert_eq!(snapshot(&upgrade), snapshot(&clean));
    assert_eq!(upgrade.db.query_row("SELECT count(*) FROM locations WHERE typeof(raw_hash)!='blob' OR length(raw_hash)!=32 OR (native_id IS NOT NULL AND length(native_id)!=32)", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(upgrade.db.query_row("SELECT count(*) FROM event_details WHERE call_id IS NOT NULL AND (typeof(call_id)!='blob' OR length(call_id)!=32)", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(
        upgrade
            .db
            .query_row("SELECT count(*) FROM locations", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        upgrade
            .db
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap()
    );
    assert_eq!(
        upgrade
            .db
            .query_row("SELECT count(*) FROM event_details", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        upgrade
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE kind!='context'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
    );
}

#[test]
fn previous_compact_schema_rebuilds_without_reading_new_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old-compact.db");
    let old = rusqlite::Connection::open(&path).unwrap();
    old.execute_batch(include_str!("../testdata/schema-v2.sql"))
        .unwrap();
    assert!(Store::open(&path).is_err());
    assert!(Store::require_schema(&old).is_err());
    let mut upgrade = Store::open_for_rebuild(&path).unwrap();
    upgrade.refresh(&[], true, None).unwrap();
    Store::require_schema(&upgrade.db).unwrap();
    let n: i64 = upgrade
        .db
        .query_row(
            "SELECT count(*) FROM events WHERE text_truncated=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn incremental_append_truncate_replace_equals_rebuild_and_exact_pointers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("logs");
    fs::create_dir(&root).unwrap();
    let path = root.join("s.jsonl");
    let fixture = include_str!("../testdata/codex.jsonl");
    let roots = [Root {
        harness: "codex".into(),
        path: root,
    }];
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    for (i, data) in [
        fixture.lines().take(2).collect::<Vec<_>>().join("\n") + "\n",
        fixture.into(),
        fixture.lines().take(3).collect::<Vec<_>>().join("\n") + "\n",
        fixture.replace("gpt-fixture", "gpt-replaced"),
    ]
    .into_iter()
    .enumerate()
    {
        if i == 1 {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(
                    data.lines()
                        .skip(2)
                        .collect::<Vec<_>>()
                        .join("\n")
                        .as_bytes(),
                )
                .unwrap();
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"\n")
                .unwrap();
        } else if i == 3 {
            let replacement = path.with_extension("new");
            fs::write(&replacement, &data).unwrap();
            fs::rename(replacement, &path).unwrap();
        } else {
            fs::write(&path, &data).unwrap();
        }
        store.refresh(&roots, false, None).unwrap();
        let got = snapshot(&store);
        let mut clean = Store::open(&dir.path().join(format!("clean-{i}.db"))).unwrap();
        clean.refresh(&roots, true, None).unwrap();
        assert_eq!(got, snapshot(&clean));
        let matches = sessidx::query::search(
            &store.db,
            "太长",
            &sessidx::query::Filters::default(),
            20,
            0,
        )
        .unwrap();
        assert_eq!(matches.len(), usize::from(data.contains("太长")));
        let raw = fs::read(&path).unwrap();
        let ranges: Vec<(usize, usize, usize)> = store
            .db
            .prepare("SELECT line_no,byte_off,byte_len FROM events")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for (line, off, len) in ranges {
            assert_eq!(
                &raw[off..off + len],
                raw.split_inclusive(|b| *b == b'\n').nth(line - 1).unwrap()
            );
        }
    }
}

#[test]
fn same_size_rewrite_with_preserved_mtime_equals_clean_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.jsonl");
    let data = format!(
        "{}\n",
        serde_json::json!({"a_padding":"x".repeat(5000),"type":"message","message":{"role":"user","content":"oldword"}})
    );
    fs::write(&path, &data).unwrap();
    let roots = [Root {
        harness: "pi".into(),
        path: dir.path().into(),
    }];
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    store.refresh(&roots, false, None).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let replaced = data.replace("oldword", "newword");
    assert_eq!(data.len(), replaced.len());
    assert_eq!(&data.as_bytes()[..4096], &replaced.as_bytes()[..4096]);
    fs::write(&path, replaced).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    store.refresh(&roots, false, None).unwrap();
    let mut clean = Store::open(&dir.path().join("clean.db")).unwrap();
    clean.refresh(&roots, true, None).unwrap();
    assert_eq!(snapshot(&store), snapshot(&clean));
}

#[test]
fn partial_tail_is_deferred_without_staleness_and_resumes_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.jsonl");
    let first = "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"first\"}}\n";
    let tail = "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"second\"}}";
    fs::write(&path, format!("{first}{tail}")).unwrap();
    let roots = [Root {
        harness: "pi".into(),
        path: dir.path().into(),
    }];
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    let r = store.refresh(&roots, false, None).unwrap();
    assert!(!r.stale);
    assert_eq!(r.deferred_tails, 1);
    assert_eq!(r.records, 1);
    assert!(r.continuation.is_none());
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_sessidx"))
        .arg("--db")
        .arg(&store.path)
        .arg("--root")
        .arg(format!("pi={}", dir.path().display()))
        .arg("index")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows[0]["type"], "index");
    assert_eq!(rows[0]["deferred_tails"], 1);
    assert_eq!(rows[0]["stale"], false);
    assert!(rows[0]["continuation"].is_null());
    assert_eq!(rows.last().unwrap()["complete"], true);
    let r = store.refresh(&roots, false, None).unwrap();
    assert_eq!(r.records, 0);
    assert_eq!(r.deferred_tails, 1);
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let r = store.refresh(&roots, false, None).unwrap();
    assert_eq!(r.records, 1);
    assert_eq!(r.deferred_tails, 0);
    assert!(!r.stale);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn opaque_blocks_are_not_materialized_and_normalized_counts_are_preserved() {
    let raw = include_bytes!("../testdata/opaque.jsonl");
    let original: serde_json::Value = serde_json::from_slice(raw).unwrap();
    let filtered = sessidx::normalize::record_for_index(raw, "pi").unwrap();
    let payload_fields: usize = filtered["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .take(2)
        .map(|b| b.as_object().unwrap().len() - 1)
        .sum();
    assert_eq!(payload_fields, 0);
    let original = adapters::parse("pi", &original, &mut State::default());
    let filtered = adapters::parse("pi", &filtered, &mut State::default());
    let snapshot = |r: sessidx::model::Record| {
        r.events
            .into_iter()
            .map(|e| (e.kind, e.role, e.text, e.command, e.sites.len()))
            .collect::<Vec<_>>()
    };
    assert_eq!(snapshot(original), snapshot(filtered));
    let scalar = b"true";
    assert_eq!(
        sessidx::normalize::record_for_index(scalar, "pi").unwrap(),
        serde_json::json!(true)
    );
    assert!(
        sessidx::normalize::record_for_index(
            b"{\"message\":{\"content\":[{\"type\":\"thinking\",\"thinkingSignature\":invalid}]}}",
            "pi"
        )
        .is_err()
    );
}
