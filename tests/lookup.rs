use sessidx::{
    discovery::Root,
    query::{self, Filters},
    store::Store,
};
use std::{fs, process::Command, time::Duration};

// Keep child launches from inheriting another test's live writer lock.
static CLI_PROCESS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

fn search_hits(
    db: &rusqlite::Connection,
    query: &str,
    filters: &Filters,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Vec<query::Hit>> {
    Ok(query::search(db, query, filters, limit, offset)?
        .into_iter()
        .flat_map(|s| s.hits)
        .collect())
}

fn assert_canaries_absent_from_storage(dir: &std::path::Path, values: &[&str]) {
    for name in ["index.db", "index.db-wal", "index.db-shm"] {
        let bytes = match fs::read(dir.join(name)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && name != "index.db" => continue,
            Err(e) => panic!("cannot check {name}: {e}"),
        };
        for value in values {
            assert!(
                !bytes.windows(value.len()).any(|b| b == value.as_bytes()),
                "canary leaked to {name}"
            );
        }
    }
}

#[test]
fn session_access_uses_indexes_and_grep_bounds_the_first_sqlite_step() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (_dir, store, _) = setup("claude", include_str!("../testdata/claude.jsonl"));
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
    let start = std::time::Instant::now();
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
    assert!(start.elapsed() < Duration::from_millis(200));
    assert_eq!(
        sessidx::counting::sql(&store.db, "SELECT 1 AS n").unwrap()[0]["n"],
        1
    );
}

#[test]
fn lookup_cjk_latin_filters_and_show_references() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (_dir, store, _) = setup("claude", include_str!("../testdata/claude.jsonl"));
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
                harness: vec![sessidx::query::Harness::Pi],
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
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
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
        role: Some(sessidx::query::Role::Tool),
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
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (_dir, mut store, roots) = setup("codex", include_str!("../testdata/codex.jsonl"));
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
        harness: vec![sessidx::query::Harness::Codex],
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
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let mut values = canaries();
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
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/credential-context.json")).unwrap();
    for (i, f) in fixture["labels"].as_array().unwrap().iter().enumerate() {
        let label = f["label"].as_str().unwrap();
        let value = f["value"].as_str().unwrap();
        values.push(value.into());
        let mut input = serde_json::json!({"command":format!("printf 'needle {label}={value}'")});
        input
            .as_object_mut()
            .unwrap()
            .insert(label.into(), value.into());
        for (kind, message) in [
            (
                "user",
                serde_json::json!({"role":"user","content":format!("needle {label}: {value}")}),
            ),
            (
                "assistant",
                serde_json::json!({"role":"assistant","content":[{"type":"tool_use","id":format!("label-{i}"),"name":"Bash","input":input}]}),
            ),
            (
                "user",
                serde_json::json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("label-{i}"),"is_error":false,"content":format!("needle {label}={value}")}]}),
            ),
        ] {
            data += &line(&format!("label-{i}-{kind}"), kind, message);
        }
    }
    for key in ["symbol", "slash", "url_path", "url_query"] {
        let value = fixture[key].as_str().unwrap();
        values.push(value.into());
        let carrier = match key {
            "url_path" => format!("https://hooks.slack.com/services/T123/B456/{value}"),
            "url_query" => format!("https://example.invalid/search?ref={value}"),
            _ => value.to_owned(),
        };
        data += &line(
            key,
            "user",
            serde_json::json!({"role":"user","content":format!("needle my password is {carrier} ok")}),
        );
        data += &line(
            &format!("{key}-input"),
            "assistant",
            serde_json::json!({"role":"assistant","content":[{"type":"tool_use","id":key,"name":"Bash","input":{"command":format!("printf 'needle {carrier}'")}}]}),
        );
        data += &line(
            &format!("{key}-result"),
            "user",
            serde_json::json!({"role":"user","content":[{"type":"tool_result","tool_use_id":key,"is_error":false,"content":format!("needle {carrier}")}]}),
        );
    }
    data += &line(
        "public-identifiers",
        "user",
        serde_json::json!({"role":"user","content":format!("needle {}",fixture["identifiers"].as_array().unwrap().iter().map(|v|v.as_str().unwrap()).collect::<Vec<_>>().join(" "))}),
    );
    let (dir, store, roots) = setup("claude", &data);
    assert_canaries_absent_from_storage(
        dir.path(),
        &values.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    for v in fixture["identifiers"].as_array().unwrap() {
        let value = v.as_str().unwrap();
        let hits = search_hits(&store.db, value, &Filters::default(), 20, 0).unwrap();
        assert!(
            hits.iter().any(|h| h.snippet.contains(value)),
            "public identifier lost"
        );
    }
    let mut outputs = Vec::new();
    for args in [
        vec!["index"],
        vec!["search", "needle"],
        vec![
            "grep",
            "needle",
            "--session",
            "canary-session",
            "--limit",
            "1000",
        ],
        vec!["show", "canary-session", "--limit", "1000"],
        vec!["grep", "needle"],
        vec!["count", "commands"],
        vec!["count", "failures"],
        vec!["count", "denials"],
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
            Some(if args == ["grep", "needle"] { 2 } else { 0 }),
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
    assert_canaries_absent_from_storage(
        dir.path(),
        &values.iter().map(String::as_str).collect::<Vec<_>>(),
    );
}

#[test]
fn adversarial_hex_fragments_basic_and_passphrase_canaries_are_removed() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let values: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/privacy.json")).unwrap();
    let text = format!(
        "needle {} token={} Authorization: Basic {} password = \"{}\" https://example.invalid/?token={} https://example.invalid/?page=2&key={} https://example.invalid/?access_token={}",
        values["unmarked"].as_str().unwrap(),
        values["hex"].as_str().unwrap(),
        values["basic"].as_str().unwrap(),
        values["passphrase"].as_str().unwrap(),
        values["query_short"].as_str().unwrap(),
        values["query_short"].as_str().unwrap(),
        values["query_short"].as_str().unwrap()
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
        assert_canaries_absent_from_storage(dir.path(), &[value]);
    }
}

#[test]
fn body_identifiers_are_searchable_and_credential_context_stays_redacted() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (_, store, _) = setup("claude", include_str!("../testdata/identifiers.jsonl"));
    for value in [
        "0123456789abcdef1032547698badcfe89abcdef",
        "0xabcdef0123456789abcdef0123456789abcdef01",
        "01234567-89ab-cdef-0123-456789abcdef",
        "/synthetic/long-project-directory/source.rs",
        "tools.exec_command",
        "os.path.join",
        "std::fs::File::try_lock",
        "a.b.c_d2",
        "_module_42::Foo_Bar1/Item99",
        "tools.exec_command:",
        "std::fs::File::try_lock.",
        "--max-output-tokens",
        "--dangerously-skip-permissions",
    ] {
        let hits = search_hits(
            &store.db,
            &format!("identifierneedle {value}"),
            &Filters::default(),
            20,
            0,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains(value));
    }
    let context = search_hits(&store.db, "contextneedle", &Filters::default(), 20, 0).unwrap();
    assert_eq!(context.len(), 1);
    for value in [
        "0123456789abcdef1032547698badcfe89abcdef",
        "0xabcdef0123456789abcdef0123456789abcdef01",
        "01234567-89ab-cdef-0123-456789abcdef",
        "vR9xT6qA2nL8cP4hY0sD7fG3jK5mB1wZ",
        "tools.exec_command",
        "std::fs::File::try_lock",
        "ghp_AlphabeticSyntheticToken",
        "_module_42::Foo_Bar1/Item99",
        "module.name:invalidColon9",
        "module..missing_segment9",
        "a1b2.c3d4_e5F6.g7H8",
        "--invalid--flagSyntaxExtra",
    ] {
        assert!(
            !context[0].snippet.contains(value),
            "synthetic context control survived: {value}"
        );
    }
    let input = search_hits(&store.db, "identifierinput", &Filters::default(), 20, 0).unwrap();
    assert_eq!(input.len(), 1);
    assert!(input[0].snippet.contains("tools.exec_command"));
    assert!(!input[0].snippet.contains("std::fs::File::try_lock"));
    let stored: String = store
        .db
        .query_row("SELECT text FROM events WHERE kind='tool_call'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let args: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        args["namespace_members"],
        "01234567-89ab-cdef-0123-456789abcdef"
    );
    assert_eq!(
        args["schema_properties"],
        "0123456789abcdef1032547698badcfe89abcdef"
    );
    assert!(
        search_hits(
            &store.db,
            "identifierinput 0xabcdef0123456789abcdef0123456789abcdef01",
            &Filters::default(),
            20,
            0
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        input[0]
            .snippet
            .contains("0123456789abcdef1032547698badcfe89abcdef")
    );
    assert!(
        !input[0]
            .snippet
            .contains("0xabcdef0123456789abcdef0123456789abcdef01")
    );
    assert!(
        !input[0]
            .snippet
            .contains("vR9xT6qA2nL8cP4hY0sD7fG3jK5mB1wZ")
    );
}

#[test]
fn tool_output_prefix_is_bounded_and_raw_tail_remains_reachable() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/output-prefix.json")).unwrap();
    let prefix = format!("outputneedle {}", "x".repeat(2047 - "outputneedle ".len()));
    let text = format!("{prefix}太tailoutside");
    for f in fixture.as_array().unwrap().iter().take(3) {
        let h = f["harness"].as_str().unwrap();
        let mut record = f["record"].clone();
        let field = match h {
            "claude" => "/message/content/0/content",
            "codex" => "/payload/output/0/text",
            _ => "/message/content/0/text",
        };
        *record.pointer_mut(field).unwrap() = serde_json::json!(text);
        let (_dir, store, _) = setup(h, &(record.to_string() + "\n"));
        let hits = search_hits(&store.db, "outputneedle", &Filters::default(), 20, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].role, "tool");
        assert!(hits[0].truncated);
        assert_eq!(hits[0].snippet, prefix);
        assert!(
            search_hits(&store.db, "tailoutside", &Filters::default(), 20, 0)
                .unwrap()
                .is_empty()
        );
        let (raw, c) = query::scan(
            &store.db,
            "tailoutside",
            &Filters {
                harness: vec![match h {
                    "claude" => sessidx::query::Harness::Claude,
                    "codex" => sessidx::query::Harness::Codex,
                    _ => sessidx::query::Harness::Pi,
                }],
                ..Filters::default()
            },
            20,
            0,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(raw.len(), 1);
        assert!(!c.incomplete && !raw[0].truncated);
        assert!(raw[0].snippet.contains("tailoutside"));
        let (shown, c) = query::show(&store.db, &format!("{}:1", hits[0].path), 0, 20, 0).unwrap();
        assert_eq!(shown.len(), 1);
        assert!(!c.incomplete && !shown[0].truncated);
        assert!(shown[0].snippet.contains("tailoutside"));
    }
    let mut s = sessidx::model::State::default();
    let mut record = fixture[0]["record"].clone();
    record["message"]["content"][0]["content"] = serde_json::json!("key=a ".repeat(341));
    let event = sessidx::adapters::parse("claude", &record, &mut s)
        .events
        .remove(0);
    assert!(event.text.as_ref().unwrap().len() <= 2048);
    assert!(event.text_truncated);
}

#[test]
fn diagnostic_attachments_are_tool_outputs_without_creating_results() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/output-prefix.json")).unwrap();
    let data = format!("{}\n{}\n", fixture[3]["record"], fixture[4]["record"]);
    let (_dir, store, _) = setup("claude", &data);
    let hits = search_hits(&store.db, "diagnosticneedle", &Filters::default(), 20, 0).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        search_hits(&store.db, "cSpell", &Filters::default(), 20, 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(hits[0].role, "tool");
    assert!(!hits[0].truncated);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE kind='tool_result'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE kind='context'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn pi_sections_share_the_native_message_and_are_redacted() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let (dir, store, _) = setup("pi", include_str!("../testdata/pi-sections.jsonl"));
    for term in ["contentneedle", "sectionneedle", "TypeSafe", "sectiontail"] {
        let hits = search_hits(&store.db, term, &Filters::default(), 20, 0).unwrap();
        assert_eq!(hits.len(), 1, "{term}");
        assert_eq!(hits[0].role, "system");
    }
    let messages: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM canonical_events WHERE kind='message'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(messages, 1);
    assert_canaries_absent_from_storage(dir.path(), &["Q8vN2rK7xP4mT9aF6wH3cS5uD1jL0eB"]);
}

#[test]
fn ranked_search_pages_sessions_before_selecting_best_hits() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let message = |session: &str, id: usize, ts: &str, text: &str| {
        serde_json::json!({"type":"user","uuid":format!("{session}-{id}"),"sessionId":session,"timestamp":ts,"message":{"role":"user","content":text}}).to_string()+"\n"
    };
    let data = (0..25)
        .map(|i| message("crowded", i, "2026-10-02T00:00:00Z", "groupneedle"))
        .collect::<String>();
    let (dir, mut store, mut roots) = setup("claude", &data);
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
    let page = query::search(&store.db, "groupneedle", &Filters::default(), 2, 0).unwrap();
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
    let next = query::search(&store.db, "groupneedle", &Filters::default(), 2, 2).unwrap();
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
        harness: "codex".into(),
        path: codex,
    });
    store.refresh(&roots, false, None).unwrap();
    let all = query::search(&store.db, "groupneedle", &Filters::default(), 20, 0).unwrap();
    assert_eq!(all.len(), 4);
    assert_eq!(all.iter().filter(|s| s.session_id == "crowded").count(), 2);
}

#[test]
fn scan_matches_original_ranges_before_redacting_display() {
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
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
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let session = "rollout-2026-05-01T10-30-00-syntheticId7QwX9rTbM3k";
    let cwd = "/synthetic/Code/GitHub/project-with-native-identifiers";
    let data=serde_json::json!({"type":"session_meta","payload":{"id":session,"cwd":cwd}}).to_string()+"\n"+&serde_json::json!({"type":"turn_context","payload":{"model":"claude-haiku-4-5-20251001"}}).to_string()+"\n"+&serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"needle"}]}}).to_string()+"\n";
    let (_, store, _) = setup("codex", &data);
    let hits = search_hits(
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
    let _processes = CLI_PROCESS_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let store = Store::open(&path).unwrap();
    let lock = store.lock().unwrap().unwrap();
    let mut second = Store::open(&path).unwrap();
    let r = second.refresh(&[], false, None).unwrap();
    assert!(r.writer_busy && r.stale);
    let out = Command::new(env!("CARGO_BIN_EXE_sessidx"))
        .arg("--db")
        .arg(&path)
        .arg("--root")
        .arg(format!("codex={}", dir.path().display()))
        .arg("index")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows[0]["type"], "index");
    assert_eq!(rows[0]["writer_busy"], true);
    assert_eq!(rows[0]["stale"], true);
    assert!(rows.last().unwrap().get("error").is_none());
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
        sessidx::store::SCHEMA_VERSION
    );
}
