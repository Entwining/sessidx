use crate::common::{indexed, serial, sessidx, snapshot, stream};
use sessidx::{
    discovery::Root,
    model::Harness,
    query::{self, Filters},
    store::Store,
};
use std::{fs, time::Duration};

#[test]
fn incremental_append_truncate_replace_equals_rebuild_and_exact_pointers() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("logs");
    fs::create_dir(&root).unwrap();
    let path = root.join("s.jsonl");
    let fixture = include_str!("../fixtures/codex.jsonl");
    let roots = [Root {
        harness: Harness::Codex,
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
        .unwrap()
        .sessions;
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
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.jsonl");
    let data = format!(
        "{}\n",
        serde_json::json!({"a_padding":"x".repeat(5000),"type":"message","message":{"role":"user","content":"oldword"}})
    );
    fs::write(&path, &data).unwrap();
    let roots = [Root {
        harness: Harness::Pi,
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
    let serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.jsonl");
    let first = "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"first\"}}\n";
    let tail = "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"second\"}}";
    fs::write(&path, format!("{first}{tail}")).unwrap();
    let roots = [Root {
        harness: Harness::Pi,
        path: dir.path().into(),
    }];
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    let r = store.refresh(&roots, false, None).unwrap();
    assert!(!r.stale);
    assert_eq!(r.deferred_tails, 1);
    assert_eq!(r.records, 1);
    assert!(r.continuation.is_none());
    let rows = stream(
        &sessidx(&serial, &store.path, &roots, &["index"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(rows[0]["type"], "index");
    assert_eq!(rows[0]["deferred_tails"], 1);
    assert_eq!(rows[0]["stale"], false);
    assert!(rows[0]["continuation"].is_null());
    assert_eq!(rows.last().unwrap()["complete"], true);
    let r = store.refresh(&roots, false, None).unwrap();
    assert_eq!(r.records, 0);
    assert_eq!(r.files_changed, 0);
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
    assert_eq!(store.refresh(&roots, false, None).unwrap().files_changed, 0);
    let kind_source: String = store
        .db
        .query_row("SELECT kind_source FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kind_source, "none");
}

#[test]
fn writer_lock_budget_missing_root_and_scan_cursor_are_visible() {
    let _serial = serial();
    let (_dir, mut store, roots) = indexed(Harness::Codex, include_str!("../fixtures/codex.jsonl"));
    let lock = store.lock().unwrap().unwrap();
    let busy = store
        .refresh(&roots, false, Some(Duration::from_secs(2)))
        .unwrap();
    assert!(busy.stale && busy.writer_busy);
    drop(lock);
    let location = roots[0].path.join("token=synthetic-location-key.jsonl");
    fs::rename(roots[0].path.join("session.jsonl"), &location).unwrap();
    let budget = store.refresh(&roots, false, Some(Duration::ZERO)).unwrap();
    assert!(budget.stale && budget.continuation.is_some());
    assert_eq!(budget.continuation.as_deref(), location.to_str());
    assert!(!store.refresh(&roots, false, None).unwrap().stale);
    let missing = store
        .refresh(
            &[Root {
                harness: Harness::Pi,
                path: roots[0].path.join("absent"),
            }],
            false,
            None,
        )
        .unwrap();
    assert_eq!(
        missing.missing_roots,
        [format!("pi={}", roots[0].path.join("absent").display())]
    );
    assert!(!missing.stale);
    let filters = Filters {
        harness: vec![Harness::Codex],
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

#[test]
fn a_missing_root_is_stale_only_when_it_held_indexed_files() {
    let _serial = serial();
    let (dir, mut store, roots) = indexed(Harness::Codex, include_str!("../fixtures/codex.jsonl"));
    fs::rename(&roots[0].path, dir.path().join("moved")).unwrap();
    let missing = store.refresh(&roots, false, None).unwrap();
    assert_eq!(
        missing.missing_roots,
        [format!("codex={}", roots[0].path.display())]
    );
    assert!(missing.stale);
    let files: i64 = store
        .db
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(files, 1);
    let refused = store.refresh(&roots, true, None).unwrap_err();
    assert!(format!("{refused:#}").contains("--root"), "{refused:#}");
    assert!(store.refresh(&roots, false, None).unwrap().stale);
    store.refresh(&[], true, None).unwrap();
    assert!(!store.refresh(&roots, false, None).unwrap().stale);
}

#[test]
fn an_archived_codex_thread_stays_findable_under_its_new_path() {
    let serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    let sessions = dir.path().join(".codex/sessions/2026/10/01");
    let archived = dir.path().join(".codex/archived_sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout.jsonl"),
        include_str!("../fixtures/codex.jsonl"),
    )
    .unwrap();
    let run =
        |args: &[&str], exit| stream(&sessidx(&serial, &db, &[], args).output().unwrap(), exit);
    let claude = format!("claude={}", dir.path().join(".claude/projects").display());
    let pi = format!("pi={}", dir.path().join(".pi/agent/sessions").display());
    let index = run(&["index"], 0);
    assert_eq!(
        index[0]["missing_roots"],
        serde_json::json!([claude, format!("codex={}", archived.display()), pi])
    );
    assert_eq!(index[0]["stale"], false);
    let before = run(&["show", "codex-fixture"], 0).len();
    let moved = archived.join("rollout.jsonl");
    fs::create_dir_all(&archived).unwrap();
    fs::rename(sessions.join("rollout.jsonl"), &moved).unwrap();
    let index = run(&["index"], 0);
    assert_eq!(index[0]["missing_roots"], serde_json::json!([claude, pi]));
    let shown = run(&["show", "codex-fixture"], 0);
    assert_eq!(shown.len(), before);
    for record in shown.iter().filter(|r| r["type"] == "record") {
        assert_eq!(record["path"], moved.to_str().unwrap());
    }
    let files = run(&["sql", "SELECT path FROM files"], 0);
    let paths: Vec<_> = files.iter().filter(|r| r["type"] == "row").collect();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0]["data"]["path"], moved.to_str().unwrap());
}

#[test]
fn a_path_reindexed_under_another_harness_is_parsed_again() {
    let _serial = serial();
    let (_dir, mut store, roots) = indexed(Harness::Claude, include_str!("../fixtures/pi.jsonl"));
    let pi = [Root {
        harness: Harness::Pi,
        path: roots[0].path.clone(),
    }];
    assert_eq!(store.refresh(&pi, false, None).unwrap().files_changed, 1);
    let (files, sessions): (String, String) = store
        .db
        .query_row(
            "SELECT f.harness,s.harness||'/'||s.session_id FROM files f JOIN sessions s ON s.file_id=f.id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((files.as_str(), sessions.as_str()), ("pi", "pi/pi-fixture"));
}
