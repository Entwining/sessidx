use crate::common::{body, indexed, indexed_files, search_hits};
use sessidx::{
    discovery::Root,
    model::Agent,
    query::{self, Filters},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[test]
fn transcript_path_access_is_indexed_and_counts_stay_per_transcript() {
    let (_dir, store, _) = indexed_files(Agent::Codex, &[]);
    store
        .db
        .execute_batch(include_str!("../fixtures/session-paths.sql"))
        .unwrap();
    let counts_query = |key: &str| {
        format!(
            "SELECT s.path,(SELECT count(*) FROM events e WHERE {key} AND e.kind='tool_call') AS calls
             FROM sessions s WHERE s.started_at >= '2026-10-01' ORDER BY s.file_id"
        )
    };
    let path_query = counts_query("e.path=s.path");
    let plan = store
        .db
        .prepare(&format!("EXPLAIN QUERY PLAN {path_query}"))
        .unwrap()
        .query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.starts_with("SEARCH ") && step.contains("(path=?)")),
        "{path_query}: {plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|step| step == "SCAN e" || step.starts_with("SCAN e ")),
        "{path_query}: {plan:?}"
    );
    assert!(
        plan.iter()
            .any(|step| step.starts_with("SEARCH e ") && step.contains("file_id=?")),
        "{path_query}: {plan:?}"
    );
    let counts = |key| {
        store
            .db
            .prepare(&counts_query(key))
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let by_path = counts("e.path=s.path");
    assert_eq!(by_path, counts("e.file_id=s.file_id"));
    assert_eq!(
        by_path.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
        [2, 3, 1]
    );
    assert_eq!(
        counts("e.agent=s.agent AND e.session_id=s.session_id")
            .iter()
            .map(|(_, n)| *n)
            .collect::<Vec<_>>(),
        [5, 5, 1]
    );
    let path = "/synthetic/transcript-1.jsonl";
    assert_eq!(query::session_file(&store.db, path).unwrap(), Some(1));
    assert!(
        store
            .db
            .execute(
                "UPDATE sessions SET path='/synthetic/transcript-1.jsonl' WHERE file_id=2",
                []
            )
            .is_err()
    );
    // An unindexed path lookup scans the whole sessions table, so grow it until
    // that scan dominates the indexed search the progress handler measures.
    store
        .db
        .execute_batch(
            "WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<50000)
             INSERT INTO files(id,path,agent,dev,inode,size,mtime,prefix_hash,prefix_len,state_json)
             SELECT 1000+i,'/synthetic/bulk-'||i||'.jsonl','codex',0,i,0,'0','',0,'{}' FROM n;
             INSERT INTO sessions(file_id,agent,session_id,session_kind,session_kind_source,path,cwd,started_at)
             SELECT id,agent,'bulk-'||id,'main','synthetic',path,'/synthetic','2020-01-01' FROM files WHERE id>1000;",
        )
        .unwrap();
    let steps = Arc::new(AtomicUsize::new(0));
    let counter = steps.clone();
    store
        .db
        .progress_handler(
            100,
            Some(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
    let work = |run: &dyn Fn()| {
        steps.store(0, Ordering::Relaxed);
        run();
        steps.load(Ordering::Relaxed)
    };
    let (indexed_file, indexed_show) = (
        work(&|| assert_eq!(query::session_file(&store.db, path).unwrap(), Some(1))),
        work(&|| {
            let (hits, _) = query::show(&store.db, path, 0, 10, None).unwrap();
            assert_eq!(hits.len(), 2);
        }),
    );
    store.db.execute_batch("DROP INDEX sessions_path").unwrap();
    let (unindexed_file, unindexed_show) = (
        work(&|| assert_eq!(query::session_file(&store.db, path).unwrap(), Some(1))),
        work(&|| {
            let (hits, _) = query::show(&store.db, path, 0, 10, None).unwrap();
            assert_eq!(hits.len(), 2);
        }),
    );
    // One callback per 100 virtual machine instructions. The indexed lookup
    // searches `sessions_path`; the unindexed one scans and sorts 50,000 rows.
    assert!(
        indexed_file * 5 < unindexed_file,
        "{indexed_file} indexed against {unindexed_file} unindexed"
    );
    assert!(
        indexed_show * 5 < unindexed_show,
        "{indexed_show} indexed against {unindexed_show} unindexed"
    );
}

#[test]
fn session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step() {
    let (_dir, store, _) = indexed(Agent::Claude, include_str!("../fixtures/claude.jsonl"));
    let filters = Filters {
        session: Some("session".into()),
        ..Filters::default()
    };
    let path: String = store
        .db
        .query_row("SELECT path FROM sessions", [], |r| r.get(0))
        .unwrap();
    for selector in ["session".to_owned(), path] {
        let (clause, args) = Filters {
            session: Some(selector),
            ..Filters::default()
        }
        .sql(&store.db)
        .unwrap();
        let plan = store
            .db
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT e.id FROM events e JOIN sessions s ON s.file_id=e.file_id WHERE {clause} ORDER BY e.id"
            ))
            .unwrap()
            .query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|s| s.starts_with("SEARCH e ") && s.contains("(file_id=?)")),
            "{plan:?}"
        );
    }
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000000) INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,role,role_source,kind,kind_source,model_source,ok_source,line_role_first,canonical,call_ok_source) SELECT r.file_id,n.i+100,r.byte_off,r.byte_len,0,r.raw_hash,r.role,r.role_source,r.kind,r.kind_source,r.model_source,r.ok_source,1,1,CASE WHEN r.kind='tool_call' THEN 'none' END FROM n JOIN event_rows r ON r.id=1;").unwrap();
    // No event has the developer role, so the first step reads every row.
    let (hits, coverage) = query::scan(
        &store.db,
        "absent",
        &Filters {
            role: Some(query::Role::Developer),
            ..filters
        },
        20,
        None,
        Duration::from_millis(10),
    )
    .unwrap();
    assert!(hits.is_empty());
    assert!(coverage.incomplete);
    assert_eq!(coverage.records, 0);
    assert_eq!(coverage.continuation, Some((0, 0, 0)));
}

#[test]
fn search_work_follows_its_matches_not_the_history() {
    let (_dir, store, _) = indexed(Agent::Claude, include_str!("../fixtures/claude.jsonl"));
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000000) INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,role,role_source,kind,kind_source,model_source,ok_source,line_role_first,canonical,call_ok_source) SELECT r.file_id,n.i+100,r.byte_off,r.byte_len,0,r.raw_hash,r.role,r.role_source,r.kind,r.kind_source,r.model_source,r.ok_source,1,1,CASE WHEN r.kind='tool_call' THEN 'none' END FROM n JOIN event_rows r ON r.id=1;").unwrap();
    // A scan of the 1,000,000 rows that cannot match exceeds the budget.
    let found = query::search(
        &store.db,
        "太长",
        &Filters::default(),
        20,
        Duration::from_millis(100),
    )
    .unwrap();
    assert_eq!(found.sessions.len(), 1);
}

#[test]
fn lookup_cjk_latin_filters_and_show_references() {
    let (_dir, store, _) = indexed_files(
        Agent::Claude,
        &[("claude-fixture", include_str!("../fixtures/claude.jsonl"))],
    );
    let filters = Filters::default();
    assert_eq!(
        search_hits(&store.db, "太长", &filters, 20).unwrap().len(),
        1
    );
    assert_eq!(
        search_hits(&store.db, "Latin", &filters, 20).unwrap().len(),
        1
    );
    assert_eq!(
        search_hits(&store.db, "Lat", &filters, 20).unwrap().len(),
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
                agent: vec![Agent::Pi],
                ..filters.clone()
            },
            20
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
    )
    .unwrap();
    let (shown, coverage) = query::show(
        &store.db,
        &format!("{}:{}", hits[0].path, hits[0].line_no),
        0,
        1,
        None,
    )
    .unwrap();
    assert_eq!(shown.len(), 1);
    assert!(!coverage.incomplete);
    let (_dir, store, _) = indexed(Agent::Codex, include_str!("../fixtures/codex.jsonl"));
    assert_eq!(
        query::show(&store.db, "codex://threads/codex-fixture", 3, 100, None)
            .unwrap()
            .0
            .len(),
        6
    );
}

#[test]
fn scan_requires_filter_reads_only_selected_ranges_and_reports_changed_source() {
    let (dir, store, _) = indexed(Agent::Codex, include_str!("../fixtures/codex.jsonl"));
    assert!(
        query::scan(
            &store.db,
            "exited",
            &Filters::default(),
            20,
            None,
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
    assert!(query::scan(&store.db, "(", &filters, 20, None, Duration::from_secs(2)).is_err());
    let path = dir.path().join("logs/session.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] = b'!';
    fs::write(&path, &bytes).unwrap();
    let (hits, coverage) = query::scan(
        &store.db,
        "exited",
        &filters,
        20,
        None,
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(!coverage.incomplete);
    let off = hits[0].byte_off as usize;
    let reference = hits[0].reference.clone();
    bytes[off + 1] = b'!';
    fs::write(&path, bytes).unwrap();
    let (hits, coverage) = query::scan(
        &store.db,
        "exited",
        &filters,
        20,
        None,
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(hits.is_empty());
    assert_eq!(coverage.unavailable_ranges, 1);
    assert!(coverage.incomplete);
    let (shown, coverage) = query::show(&store.db, &reference, 0, 20, None).unwrap();
    assert_eq!(body(&shown[0]), "source range changed; run sessidx index");
    assert_eq!(coverage.unavailable_ranges, 1);
    assert!(coverage.incomplete);
    // Unreadable whether the file can be listed but not opened, or is gone.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let (unreadable, _) = query::show(&store.db, &reference, 0, 20, None).unwrap();
    fs::remove_file(&path).unwrap();
    let (removed, _) = query::show(&store.db, &reference, 0, 20, None).unwrap();
    for shown in [unreadable, removed] {
        let reason = body(&shown[0]);
        assert!(
            reason.starts_with("source unreadable; run sessidx index: "),
            "{reason}"
        );
    }
}

#[test]
fn grep_and_show_return_each_line_once_per_role() {
    // Line 1's single tool_use block is indexed as an assistant message and a tool call.
    let (_dir, store, _) = indexed(Agent::Claude, include_str!("../fixtures/claude.jsonl"));
    let lines = |hits: &[query::Hit]| {
        hits.iter()
            .map(|h| (h.line_no, h.role.clone()))
            .collect::<Vec<_>>()
    };
    let (shown, _) = query::show(&store.db, "session", 0, 100, None).unwrap();
    assert_eq!(
        lines(&shown),
        [
            (1, "assistant".into()),
            (2, "tool".into()),
            (3, "assistant".into()),
            (4, "user".into())
        ]
    );
    let filters = Filters {
        session: Some("session".into()),
        ..Filters::default()
    };
    let (hits, _) = query::scan(
        &store.db,
        "call-c",
        &filters,
        20,
        None,
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(lines(&hits), [(1, "assistant".into()), (2, "tool".into())]);
}

#[test]
fn ranked_search_pages_sessions_before_selecting_best_hits() {
    let message = |session: &str, id: usize, ts: &str, text: &str| {
        serde_json::json!({"type":"user","uuid":format!("{session}-{id}"),"sessionId":session,"timestamp":ts,"message":{"role":"user","content":text}}).to_string()+"\n"
    };
    let data = (0..25)
        .map(|i| message("crowded", i, "2026-10-02T00:00:00Z", "groupneedle"))
        .collect::<String>();
    let (dir, mut store, mut roots) = indexed_files(Agent::Claude, &[("crowded", &data)]);
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
    let first = query::search(
        &store.db,
        "groupneedle",
        &Filters::default(),
        2,
        std::time::Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(first.matched_sessions, 3);
    let order = first.order.unwrap();
    let page = first.sessions;
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
    let next = query::search(
        &store.db,
        "groupneedle",
        &Filters {
            search_order: Some(order[2..].to_vec()),
            ..Filters::default()
        },
        2,
        std::time::Duration::from_secs(2),
    )
    .unwrap();
    // A later page ranks only the frozen transcripts it lists, so it neither
    // replaces the frozen order nor counts matches outside it.
    assert_eq!((next.order, next.matched_sessions), (None, 1));
    let next = next.sessions;
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].session_id, "weaker");
    assert_eq!(page[0].matched_hits, 25);
    assert_eq!(page[0].hits.len(), 3);
    assert_eq!(page[0].hits[0].line_no, 25);
    let (shown, coverage) =
        query::show(&store.db, &page[0].hits[0].reference, 0, 20, None).unwrap();
    assert!(!coverage.incomplete);
    assert!(body(&shown[0]).contains("crowded-24"));
    let codex = dir.path().join("codex.jsonl");
    fs::write(&codex, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"crowded\"}}\n{\"type\":\"response_item\",\"timestamp\":\"2026-10-02T00:00:00Z\",\"payload\":{\"type\":\"message\",\"id\":\"same-session-other-agent\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"groupneedle\"}]}}\n").unwrap();
    roots.push(Root {
        agent: Agent::Codex,
        path: codex,
    });
    store.refresh(&roots, false, None).unwrap();
    let all = query::search(
        &store.db,
        "groupneedle",
        &Filters::default(),
        20,
        std::time::Duration::from_secs(2),
    )
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
        agent: Agent::Pi,
        path: root,
    }];
    store.refresh(&roots, false, None).unwrap();
    let filters = Filters {
        agent: vec![Agent::Pi],
        ..Filters::default()
    };
    let (hits, coverage) = query::scan(
        &store.db,
        "needle",
        &filters,
        10,
        None,
        Duration::from_secs(2),
    )
    .unwrap();
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
                agent: Agent::Claude,
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
        let mut sessions: Vec<_> = search_hits(&store.db, "cwdneedle", &filters, 20)
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
    let data: Vec<(&str, String)> = [
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
        let record = serde_json::json!({"type":"user","uuid":session,"sessionId":session,"message":{"role":"user","content":text}});
        (*session, record.to_string() + "\n")
    })
    .collect();
    let files: Vec<(&str, &str)> = data.iter().map(|(s, c)| (*s, c.as_str())).collect();
    let (_dir, store, _) = indexed_files(Agent::Claude, &files);
    let sessions = |q: &str| {
        let mut found: Vec<_> = query::search(
            &store.db,
            q,
            &Filters::default(),
            20,
            std::time::Duration::from_secs(2),
        )
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
    let empty = query::search(
        &store.db,
        " \t",
        &Filters::default(),
        20,
        std::time::Duration::from_secs(2),
    )
    .err()
    .unwrap();
    assert!(format!("{empty:#}").contains("search query is empty"));
}

#[test]
fn grep_reads_only_the_selected_transcripts() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    let codex = dir.path().join("codex");
    fs::create_dir(&claude).unwrap();
    fs::create_dir(&codex).unwrap();
    fs::write(
        claude.join("session.jsonl"),
        include_str!("../fixtures/claude.jsonl"),
    )
    .unwrap();
    fs::write(
        codex.join("rollout.jsonl"),
        concat!(
            r#"{"type":"response_item","timestamp":"2026-10-02T00:00:00Z","payload":{"type":"message","id":"x-1","role":"user","content":[{"type":"input_text","text":"grepneedle"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let mut store = sessidx::store::Store::open(&dir.path().join("index.db")).unwrap();
    store
        .refresh(
            &[
                Root {
                    agent: Agent::Claude,
                    path: claude,
                },
                Root {
                    agent: Agent::Codex,
                    path: codex,
                },
            ],
            false,
            None,
        )
        .unwrap();
    // 1,000,000 more Claude events, indexed after the Codex one, before its window.
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000000) INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,ts,role,role_source,kind,kind_source,model_source,ok_source,line_role_first,canonical,call_ok_source) SELECT r.file_id,n.i+100,r.byte_off,r.byte_len,0,r.raw_hash,'2026-10-01T00:00:00.000Z',r.role,r.role_source,r.kind,r.kind_source,r.model_source,r.ok_source,1,1,CASE WHEN r.kind='tool_call' THEN 'none' END FROM n JOIN event_rows r ON r.id=1;").unwrap();
    // Reading the 1,000,000 events takes longer than the budget; reading only
    // the selected transcript does not.
    for filters in [
        Filters {
            agent: vec![Agent::Codex],
            ..Filters::default()
        },
        Filters {
            since: Some("2026-10-02T00:00:00Z".parse().unwrap()),
            ..Filters::default()
        },
    ] {
        let (hits, coverage) = query::scan(
            &store.db,
            "grepneedle",
            &filters,
            20,
            None,
            Duration::from_millis(100),
        )
        .unwrap();
        assert_eq!((hits.len(), coverage.incomplete), (1, false), "{filters:?}");
    }
}

#[test]
fn search_stops_at_its_budget_with_a_recovery_instruction() {
    let (_dir, store, _) = indexed(Agent::Claude, include_str!("../fixtures/claude.jsonl"));
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO fts(rowid,text) SELECT (SELECT max(id) FROM event_rows)+i,'busyword' FROM n;").unwrap();
    let error = query::search(
        &store.db,
        "busyword",
        &Filters::default(),
        20,
        Duration::ZERO,
    )
    .err()
    .unwrap();
    assert!(
        format!("{error:#}").contains("search exceeded 0 seconds; add distinctive terms"),
        "{error:#}"
    );
    // Only an interrupt gets the deadline instruction; other failures keep theirs.
    store.db.execute_batch("DROP TABLE fts").unwrap();
    let error = query::search(
        &store.db,
        "busyword",
        &Filters::default(),
        20,
        Duration::from_secs(2),
    )
    .err()
    .unwrap();
    assert!(format!("{error:#}").contains("no such table"), "{error:#}");
}
