use crate::common::{indexed, search_hits};
use sessidx::{
    discovery::Root,
    model::Harness,
    query::{self, Filters},
};
use std::{fs, time::Duration};

#[test]
fn session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step() {
    let (_dir, store, _) = indexed(Harness::Claude, include_str!("../fixtures/claude.jsonl"));
    let filters = Filters {
        session: Some("claude-fixture".into()),
        ..Filters::default()
    };
    let (clause, args) = filters.sql(&store.db).unwrap();
    let plan = store
        .db
        .prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT e.id FROM events e WHERE {clause} ORDER BY e.id"
        ))
        .unwrap()
        .query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|s| s.starts_with("SEARCH ") && s.contains("events_session")),
        "{plan:?}"
    );
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000000) INSERT INTO locations(file_id,session_ref,line_no,byte_off,byte_len,raw_hash,ordinal,model_source_ref) SELECT l.file_id,l.session_ref,n.i+100,l.byte_off,l.byte_len,l.raw_hash,0,l.model_source_ref FROM n JOIN locations l ON l.id=1;").unwrap();
    let (hits, coverage) = query::scan(
        &store.db,
        "absent",
        &filters,
        20,
        0,
        Duration::from_millis(10),
    )
    .unwrap();
    assert!(hits.is_empty());
    assert!(coverage.incomplete);
    assert_eq!(coverage.records, 0);
    assert_eq!(coverage.continuation, Some(0));
}

#[test]
fn lookup_cjk_latin_filters_and_show_references() {
    let (_dir, store, _) = indexed(Harness::Claude, include_str!("../fixtures/claude.jsonl"));
    let filters = Filters::default();
    assert_eq!(
        search_hits(&store.db, "太长", &filters, 20, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        search_hits(&store.db, "Latin", &filters, 20, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        search_hits(&store.db, "Lat", &filters, 20, 0)
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
        search_hits(
            &store.db,
            "Latin",
            &Filters {
                harness: vec![Harness::Pi],
                ..filters.clone()
            },
            20,
            0
        )
        .unwrap()
        .is_empty()
    );
    let hits = search_hits(
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
        1,
        0,
    )
    .unwrap();
    assert_eq!(shown.len(), 1);
    assert!(!coverage.incomplete);
    let (_dir, store, _) = indexed(Harness::Codex, include_str!("../fixtures/codex.jsonl"));
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
    let (dir, store, _) = indexed(Harness::Codex, include_str!("../fixtures/codex.jsonl"));
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
    for narrowed in [
        Filters {
            since: Some("2026-10-01T00:00:00Z".parse().unwrap()),
            ..Filters::default()
        },
        Filters {
            until: Some("2026-10-01T00:00:00Z".parse().unwrap()),
            ..Filters::default()
        },
        Filters {
            cwd: Some("/synthetic".into()),
            ..Filters::default()
        },
    ] {
        assert!(narrowed.narrowed());
    }
    let filters = Filters {
        session: Some("codex-fixture".into()),
        role: Some(sessidx::query::Role::Tool),
        ..Filters::default()
    };
    assert!(query::scan(&store.db, "(", &filters, 20, 0, Duration::from_secs(2)).is_err());
    let path = dir.path().join("logs/session.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b'!';
    fs::write(&path, &bytes).unwrap();
    let (hits, coverage) =
        query::scan(&store.db, "exited", &filters, 20, 0, Duration::from_secs(2)).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(!coverage.incomplete);
    let off = hits[0].byte_off as usize;
    let reference = hits[0].reference.clone();
    bytes[off + 1] = b'!';
    fs::write(&path, bytes).unwrap();
    let (hits, coverage) =
        query::scan(&store.db, "exited", &filters, 20, 0, Duration::from_secs(2)).unwrap();
    assert!(hits.is_empty());
    assert_eq!(coverage.unavailable_ranges, 1);
    assert!(coverage.incomplete);
    let (shown, coverage) = query::show(&store.db, &reference, 0, 20, 0).unwrap();
    assert_eq!(
        shown[0].snippet,
        "[source range unavailable; run sessidx index]"
    );
    assert_eq!(coverage.unavailable_ranges, 1);
    assert!(coverage.incomplete);
}

#[test]
fn ranked_search_pages_sessions_before_selecting_best_hits() {
    let message = |session: &str, id: usize, ts: &str, text: &str| {
        serde_json::json!({"type":"user","uuid":format!("{session}-{id}"),"sessionId":session,"timestamp":ts,"message":{"role":"user","content":text}}).to_string()+"\n"
    };
    let data = (0..25)
        .map(|i| message("crowded", i, "2026-10-02T00:00:00Z", "groupneedle"))
        .collect::<String>();
    let (dir, mut store, mut roots) = indexed(Harness::Claude, &data);
    fs::write(
        roots[0].path.join("older.jsonl"),
        message("older", 0, "2026-10-01T00:00:00Z", "groupneedle"),
    )
    .unwrap();
    fs::write(
        roots[0].path.join("weaker.jsonl"),
        message(
            "weaker",
            0,
            "2026-10-03T00:00:00Z",
            &format!("groupneedle {}", "filler ".repeat(100)),
        ),
    )
    .unwrap();
    store.refresh(&roots, false, None).unwrap();
    let page = query::search(&store.db, "groupneedle", &Filters::default(), 2, 0)
        .unwrap()
        .sessions;
    assert_eq!(
        page.iter()
            .map(|s| &s.session_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    assert_eq!(
        page.iter()
            .map(|s| s.session_id.as_str())
            .collect::<Vec<_>>(),
        ["crowded", "older"]
    );
    let next = query::search(&store.db, "groupneedle", &Filters::default(), 2, 2)
        .unwrap()
        .sessions;
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].session_id, "weaker");
    assert_eq!(page[0].matched_hits, 25);
    assert_eq!(page[0].hits.len(), 3);
    assert_eq!(page[0].hits[0].line_no, 25);
    let (shown, coverage) = query::show(&store.db, &page[0].hits[0].reference, 0, 20, 0).unwrap();
    assert!(!coverage.incomplete);
    assert!(shown[0].snippet.contains("crowded-24"));
    let codex = dir.path().join("codex.jsonl");
    fs::write(&codex, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"crowded\"}}\n{\"type\":\"response_item\",\"timestamp\":\"2026-10-02T00:00:00Z\",\"payload\":{\"type\":\"message\",\"id\":\"same-session-other-harness\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"groupneedle\"}]}}\n").unwrap();
    roots.push(Root {
        harness: Harness::Codex,
        path: codex,
    });
    store.refresh(&roots, false, None).unwrap();
    let all = query::search(&store.db, "groupneedle", &Filters::default(), 20, 0)
        .unwrap()
        .sessions;
    assert_eq!(all.len(), 4);
    assert_eq!(all.iter().filter(|s| s.session_id == "crowded").count(), 2);
}

#[test]
fn grep_reads_each_range_from_its_own_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pi");
    fs::create_dir(&root).unwrap();
    for name in ["a", "b"] {
        fs::write(
            root.join(format!("{name}.jsonl")),
            format!(
                "{{\"type\":\"session\",\"id\":\"{name}\"}}\n{{\"type\":\"message\",\"id\":\"{name}1\",\"message\":{{\"role\":\"user\",\"content\":\"needle {name}\"}}}}\n"
            ),
        )
        .unwrap();
    }
    let mut store = sessidx::store::Store::open(&dir.path().join("index.db")).unwrap();
    let roots = [Root {
        harness: Harness::Pi,
        path: root,
    }];
    store.refresh(&roots, false, None).unwrap();
    let filters = Filters {
        harness: vec![Harness::Pi],
        ..Filters::default()
    };
    let (hits, coverage) =
        query::scan(&store.db, "needle", &filters, 10, 0, Duration::from_secs(2)).unwrap();
    assert_eq!(coverage.records, 4);
    assert_eq!(coverage.unavailable_ranges, 0);
    assert_eq!(
        hits.iter()
            .map(|h| h.session_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

#[test]
fn cwd_filter_selects_the_directory_and_children_but_not_a_prefix_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    fs::create_dir(&logs).unwrap();
    for (session, cwd) in [
        ("exact", "/x/proj"),
        ("child", "/x/proj/child"),
        ("sibling", "/x/projx"),
    ] {
        fs::write(
            logs.join(format!("{session}.jsonl")),
            serde_json::json!({"type":"user","uuid":session,"sessionId":session,"cwd":cwd,"message":{"role":"user","content":"cwdneedle"}}).to_string() + "\n",
        )
        .unwrap();
    }
    let mut store = sessidx::store::Store::open(&dir.path().join("index.db")).unwrap();
    store
        .refresh(
            &[Root {
                harness: Harness::Claude,
                path: logs,
            }],
            false,
            None,
        )
        .unwrap();
    for cwd in ["/x/proj", "/x/proj/", "/x/proj//"] {
        let filters = Filters {
            cwd: Some(cwd.into()),
            ..Filters::default()
        };
        let mut sessions: Vec<_> = search_hits(&store.db, "cwdneedle", &filters, 20, 0)
            .unwrap()
            .into_iter()
            .map(|h| h.session_id)
            .collect();
        sessions.sort();
        assert_eq!(sessions, ["child", "exact"], "{cwd}");
    }
}

#[test]
fn search_terms_are_literal_phrases_joined_with_and() {
    let data: String = [
        ("adjacent", "alpha beta"),
        ("apart", "beta gamma alpha"),
        ("and_word", "alpha and beta"),
        ("or_word", "alpha or gamma"),
        ("not_word", "alpha not gamma"),
        ("near_word", "NEAR(alpha delta beta)"),
        ("prefix_word", "alph"),
    ]
    .iter()
    .map(|(session, text)| {
        serde_json::json!({"type":"user","uuid":session,"sessionId":session,"message":{"role":"user","content":text}}).to_string() + "\n"
    })
    .collect();
    let (_dir, store, _) = indexed(Harness::Claude, &data);
    let sessions = |q: &str| {
        let mut found: Vec<_> = query::search(&store.db, q, &Filters::default(), 20, 0)
            .map_err(|e| format!("{e:#}"))?
            .sessions
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        found.sort();
        Ok::<_, String>(found)
    };
    let both = ["adjacent", "and_word", "apart", "near_word"];
    let mut failures = Vec::new();
    for (q, expected) in [
        ("alpha beta", &both[..]),
        ("\"alpha beta\"", &["adjacent"]),
        ("alpha\"beta", &["adjacent"]),
        ("alpha AND beta", &["and_word"]),
        ("alpha OR gamma", &["or_word"]),
        ("alpha NOT gamma", &["not_word"]),
        ("NEAR(alpha beta)", &["near_word"]),
        ("alph*", &["prefix_word"]),
        ("alpha -beta", &both),
        ("(alpha beta", &both),
    ] {
        let found = sessions(q);
        if !found.as_ref().is_ok_and(|f| *f == expected) {
            failures.push(format!("{q}: {found:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    let empty = query::search(&store.db, " \t", &Filters::default(), 20, 0)
        .err()
        .unwrap();
    assert!(format!("{empty:#}").contains("search query is empty"));
}
