use sessidx::{
    adapters, counting,
    discovery::Root,
    model::State,
    query::{Filters, Harness, Kind, Role},
    shell,
    store::Store,
};
use std::fs;

fn indexed(h: &str, content: &str) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    fs::create_dir(&logs).unwrap();
    fs::write(logs.join("one.jsonl"), content).unwrap();
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    store
        .refresh(
            &[Root {
                harness: h.into(),
                path: logs,
            }],
            false,
            None,
        )
        .unwrap();
    (dir, store)
}

#[test]
fn codex_text_array_batch_failures_and_quoted_negative_control() {
    let mut s = State::default();
    let es: Vec<_> = include_str!("../testdata/outcomes.jsonl")
        .lines()
        .flat_map(|l| adapters::parse("codex", &serde_json::from_str(l).unwrap(), &mut s).events)
        .collect();
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "text")
            .count(),
        5
    );
    assert_eq!(
        es.iter()
            .flat_map(|e| &e.denials)
            .filter(|(s, _)| s == "hook")
            .count(),
        1
    );
    assert_eq!(
        es.iter()
            .flat_map(|e| &e.denials)
            .filter(|(s, _)| s == "batch_hook")
            .count(),
        2
    );
    assert!(es[3].denials.is_empty());
    assert_eq!(es[3].ok, Some(true));
    assert_eq!(es[4].ok, None);
    assert_eq!(es[5].ok, Some(true));
    assert_eq!(es[6].ok, Some(false));
    assert_eq!(es[6].ok_source, "text");
    assert_eq!(es[6].exit_code, Some(2));
    assert_eq!(es[7].ok, Some(false));
    assert_eq!(es[7].exit_code, Some(2));
}

#[test]
fn brush_sites_include_nested_syntax_without_counting_quoted_program_names() {
    let sites = shell::sites(
        "rg one src | cat; if test -d src; then rg two src; fi; echo \"$(rg three src)\"; printf 'rg is an argument'; f() { rg four src; }; cat <(rg five src)",
    );
    assert_eq!(
        sites
            .iter()
            .filter(|s| s.program.as_deref() == Some("rg"))
            .count(),
        5
    );
    assert!(sites.iter().all(|s| s.parsed));
    assert_eq!(
        shell::sites("printf 'rg quoted'")
            .iter()
            .filter(|s| s.program.as_deref() == Some("rg"))
            .count(),
        0
    );
    let unparsed = shell::sites("echo 'unterminated");
    assert_eq!(unparsed.len(), 1);
    assert!(!unparsed[0].parsed);
}

#[test]
fn native_message_fragments_empty_replies_summary_and_history_dedup() {
    let (dir, mut store) = indexed("claude", include_str!("../testdata/structure.jsonl"));
    let scalar =
        |store: &Store, sql: &str| store.db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(
        scalar(
            &store,
            "SELECT count(*) FROM canonical_events WHERE kind='message' AND role='assistant'"
        ),
        2
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT count(*) FROM events WHERE kind='message' AND model='<synthetic>'"
        ),
        1
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT count(*) FROM canonical_events WHERE kind='message' AND role='user'"
        ),
        2
    );
    fs::copy(
        dir.path().join("logs/one.jsonl"),
        dir.path().join("logs/copied.jsonl"),
    )
    .unwrap();
    store
        .refresh(
            &[Root {
                harness: "claude".into(),
                path: dir.path().join("logs"),
            }],
            false,
            None,
        )
        .unwrap();
    assert_eq!(
        scalar(
            &store,
            "SELECT count(*) FROM canonical_events WHERE kind='message' AND role='user'"
        ),
        2
    );
    assert_eq!(
        scalar(
            &store,
            "SELECT count(*) FROM events WHERE kind='message' AND role='user'"
        ),
        4
    );
}

#[test]
fn counts_state_units_denominators_unknowns_and_sql_is_read_only() {
    let (dir, store) = indexed("claude", include_str!("../testdata/claude.jsonl"));
    let commands =
        counting::count(&store.db, "commands", "", Some("rg"), &Filters::default()).unwrap();
    assert_eq!(commands[0]["unit"], "static_shell_command_sites");
    assert_eq!(commands[0]["numerator"], 1);
    assert_eq!(commands[0]["denominator"], 2);
    let failures =
        counting::count(&store.db, "failures", "", Some("rg"), &Filters::default()).unwrap();
    assert_eq!(failures[0]["numerator"], 1);
    assert_eq!(failures[0]["denominator"], 1);
    let grouped = counting::count(
        &store.db,
        "commands",
        "harness,model,role,week,kind",
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(grouped.len(), 1);
    for (key, expected) in [
        ("harness", "claude"),
        ("model", "claude-sonnet"),
        ("role", "assistant"),
        ("week", "2026-W40"),
        ("kind", "unknown"),
    ] {
        assert_eq!(grouped[0][key], expected);
    }
    assert_eq!(grouped[0]["numerator"], 1);
    let duplicate = counting::count(
        &store.db,
        "commands",
        "model,model",
        None,
        &Filters::default(),
    );
    assert!(duplicate.is_err());
    assert!(duplicate.unwrap_err().to_string().contains("duplicate"));
    assert!(
        counting::count(&store.db, "commands", "invalid", None, &Filters::default())
            .unwrap_err()
            .to_string()
            .contains("--by accepts")
    );
    let sessions = [
        "claude-fixture".to_owned(),
        dir.path()
            .join("logs/one.jsonl")
            .to_string_lossy()
            .into_owned(),
    ];
    for (metric, role, expected) in [
        ("commands", Role::Assistant, commands),
        ("failures", Role::Assistant, failures),
        (
            "denials",
            Role::Tool,
            counting::count(&store.db, "denials", "", Some("rg"), &Filters::default()).unwrap(),
        ),
    ] {
        for session in &sessions {
            let mut filters = Filters {
                harness: vec![Harness::Claude, Harness::Pi],
                role: Some(role),
                kind: Some(Kind::Unknown),
                session: Some(session.clone()),
                since: Some("2026-10-01".into()),
                until: Some("2026-10-02".into()),
                cwd: Some("/synthetic".into()),
                ..Filters::default()
            };
            assert_eq!(
                counting::count(&store.db, metric, "", Some("rg"), &filters).unwrap(),
                expected,
                "{metric} {session}"
            );
            filters.since = Some("2026-10-02".into());
            let empty = counting::count(&store.db, metric, "", Some("rg"), &filters).unwrap();
            assert_eq!(empty[0]["numerator"], 0);
            assert_eq!(empty[0]["denominator"], 0);
            assert_eq!(empty[0]["unclassified"], 0);
        }
    }
    assert!(counting::sql(&store.db, "DELETE FROM events").is_err());
    assert!(counting::sql(&store.db, "SELECT 1; DELETE FROM events").is_err());
    assert!(counting::sql(&store.db, "WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
    for sql in [
        "SELECT 1 AS id, 2 AS id",
        "SELECT 1 AS \"token=abc\", 2 AS \"[REDACTED]\"",
    ] {
        let duplicate = counting::sql(&store.db, sql);
        assert!(duplicate.is_err());
        assert!(
            duplicate
                .unwrap_err()
                .to_string()
                .contains("alias each column")
        );
    }
    assert_eq!(counting::doctor(&store.db).unwrap()["parse_errors"], 0);
}

#[test]
fn doctor_reports_stored_coverage_gaps_and_sql_bounds() {
    let data = concat!(
        "{\"type\":\"assistant\",\"sessionId\":\"doctor\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"bad\",\"name\":\"Bash\",\"input\":{\"command\":\"echo 'unterminated\"}}]}}\n",
        "{\"type\":\"user\",\"sessionId\":\"doctor\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"bad\",\"is_error\":true,\"content\":\"DENIED: synthetic policy\"}]}}\n",
        "{\"type\":\"future-shape\"}\n{invalid\n{\"partial\":"
    );
    let (_, store) = indexed("claude", data);
    let d = counting::doctor(&store.db).unwrap();
    for (key, expected) in [
        ("files", 1),
        ("sessions", 1),
        ("events", 4),
        ("canonical_events", 4),
        ("parse_errors", 1),
        ("incomplete_files", 1),
        ("unknown_models", 4),
        ("unknown_session_kinds", 1),
        ("unclassified_denials", 1),
    ] {
        assert_eq!(d[key], expected, "{key}");
    }
    assert_eq!(d["unknown_shapes"]["numerator"], 2);
    assert_eq!(d["unknown_shapes"]["denominator"], 4);
    assert_eq!(d["unknown_shapes"]["rate"], 0.5);
    assert_eq!(
        d["unknown_shapes"]["signatures"].as_array().unwrap().len(),
        2
    );
    assert_eq!(d["unparsed_shell_calls"]["numerator"], 1);
    assert_eq!(d["unparsed_shell_calls"]["denominator"], 1);
    assert_eq!(d["unparsed_shell_calls"]["rate"], 1.0);
    let excess = counting::sql(
        &store.db,
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10001) SELECT x FROM n",
    );
    assert!(excess.is_err());
    assert!(excess.unwrap_err().to_string().contains("10000 rows"));
    let start = std::time::Instant::now();
    let err = counting::sql(&store.db,"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n").unwrap_err();
    assert!(err.to_string().contains("interrupted"), "{err}");
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
    assert_eq!(
        counting::sql(&store.db, "SELECT 1 AS n").unwrap()[0]["n"],
        1
    );
}

#[test]
fn pi_empty_response_and_explicit_message_model_override() {
    let records = [
        serde_json::json!({"type":"model_change","modelId":"last-model"}),
        serde_json::json!({"type":"message","id":"empty-attempt","message":{"role":"assistant","model":"new-model","content":[]}}),
        serde_json::json!({"type":"message","id":"retry-attempt","message":{"role":"assistant","content":[{"type":"text","text":"retry"}]}}),
    ];
    let (_, store) = indexed(
        "pi",
        &(records
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"),
    );
    let count: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM canonical_events WHERE kind='message' AND model='new-model'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn attribution_native_denials_instructions_children_and_retry_negative_control() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/attribution.json")).unwrap();
    for h in ["claude", "codex", "pi"] {
        let mut s = State::default();
        let es: Vec<_> = fixtures[h]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|v| adapters::parse(h, v, &mut s).events)
            .collect();
        if h == "claude" {
            assert_eq!(
                usize::from(s.kind == "delegated" && s.kind_source == "isSidechain"),
                1
            );
            assert_eq!(
                es.iter()
                    .flat_map(|e| &e.denials)
                    .filter(|(source, _)| source == "classifier")
                    .count(),
                1
            );
            assert_eq!(
                es.iter()
                    .flat_map(|e| &e.denials)
                    .filter(|(source, reason)| source == "hook" && reason == "Include")
                    .count(),
                1
            );
            assert!(
                es.iter()
                    .filter(|e| e.role == "user")
                    .all(|e| e.denials.is_empty())
            );
        } else if h == "codex" {
            assert_eq!(
                usize::from(
                    s.session_id == "child-thread"
                        && s.parent_id.as_deref() == Some("parent-thread")
                ),
                1
            );
            assert_eq!(usize::from(s.instruction_hash.is_some()), 1);
            assert_eq!(es.iter().filter(|e| e.role == "assistant").count(), 2);
        } else {
            assert_eq!(usize::from(s.parent_id.as_deref() == Some("pi-parent")), 1);
            assert_eq!(
                es.iter()
                    .flat_map(|e| &e.denials)
                    .filter(|(source, reason)| source == "guard" && reason == "Include")
                    .count(),
                1
            );
            assert_eq!(es.iter().filter(|e| e.kind == "tool_call").count(), 0);
        }
    }
}

#[test]
fn split_script_error_and_truncated_batch_are_denials_with_transport_quote_control() {
    let (_, store) = indexed("codex", include_str!("../testdata/truncated.jsonl"));
    let mut stmt = store
        .db
        .prepare(
            "SELECT source,count(*) FROM denials WHERE reason_id='Include' GROUP BY source ORDER BY source",
        )
        .unwrap();
    let denials = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(denials, [("batch_hook".into(), 2), ("hook".into(), 1)]);
    let mut stmt = store
        .db
        .prepare("SELECT ok,ok_source FROM events WHERE kind='tool_result' ORDER BY line_no")
        .unwrap();
    let outcomes = stmt
        .query_map([], |r| {
            Ok((r.get::<_, Option<bool>>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        outcomes,
        [
            (Some(false), "text".into()),
            (Some(false), "text".into()),
            (Some(true), "text".into()),
            (Some(false), "text".into()),
        ]
    );
}

#[test]
fn raw_shape_counts_survive_redaction_and_server_tools_keep_their_own_kind() {
    let (_, store) = indexed("claude", include_str!("../testdata/structure.jsonl"));
    let n: i64 = store
        .db
        .query_row(
            "SELECT coalesce(sum(n),0) FROM shapes WHERE signature LIKE 'assistant/%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 3);
    let (_, store) = indexed(
        "codex",
        "{\"type\":\"inter_agent_communication_metadata\",\"payload\":{}}\n",
    );
    let n:i64=store.db.query_row("SELECT coalesce(sum(n),0) FROM shapes WHERE signature LIKE 'inter_agent_communication_metadata/%'",[],|r|r.get(0)).unwrap();
    assert_eq!(n, 1);
    let variants: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/inventory.json")).unwrap();
    let es: Vec<_> = variants
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| {
            v["shape"] == "assistant / server_tool_use"
                || v["shape"] == "assistant / advisor_tool_result"
        })
        .flat_map(|v| adapters::parse("claude", &v["record"], &mut State::default()).events)
        .collect();
    assert_eq!(
        es.iter()
            .filter(|e| e.kind == "tool_call" || e.kind == "tool_result")
            .count(),
        0
    );
    assert_eq!(
        es.iter()
            .filter(|e| e.kind == "server_tool_call" || e.kind == "server_tool_result")
            .count(),
        2
    );
}

#[test]
fn successful_stdout_guard_examples_are_not_denials() {
    for (harness, value) in [
        (
            "claude",
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"DENIED: rg has no --include flag."}]}}),
        ),
        (
            "pi",
            serde_json::json!({"type":"message","message":{"role":"toolResult","content":"DENIED: rg has no --include flag."}}),
        ),
        (
            "claude",
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","is_error":false,"content":"DENIED: rg has no --include flag."}]}}),
        ),
        (
            "pi",
            serde_json::json!({"type":"message","message":{"role":"toolResult","isError":false,"content":"DENIED: rg has no --include flag."}}),
        ),
        (
            "codex",
            serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","output":"Process exited with code 0\nOutput:\nDENIED: rg has no --include flag."}}),
        ),
    ] {
        let r = adapters::parse(harness, &value, &mut State::default());
        assert_eq!(
            r.events.iter().flat_map(|e| &e.denials).count(),
            0,
            "{harness}"
        );
    }
}

#[test]
fn program_filtered_failures_report_unparsed_calls_as_unclassified() {
    let fixture=include_str!("../testdata/claude.jsonl").to_owned()+&serde_json::json!({"type":"assistant","uuid":"bad-call","sessionId":"claude-fixture","message":{"role":"assistant","content":[{"type":"tool_use","id":"bad-shell","name":"Bash","input":{"command":"echo 'unterminated"}}]}}).to_string()+"\n";
    let (_, store) = indexed("claude", &fixture);
    let result =
        counting::count(&store.db, "failures", "", Some("rg"), &Filters::default()).unwrap();
    assert_eq!(result[0]["denominator"], 1);
    assert_eq!(result[0]["unclassified"], 1);
}

#[test]
fn claude_hook_check_errors_respect_native_success_flags() {
    let (_, store) = indexed("claude", include_str!("../testdata/hook-check.jsonl"));
    let mut stmt = store
        .db
        .prepare("SELECT source,reason_id FROM denials ORDER BY id")
        .unwrap();
    let denials = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(denials, [("hook".into(), "unknown".into())]);
    let flags: (i64, i64) = store
        .db
        .query_row(
            "SELECT sum(ok=0 AND ok_source='flag'),sum(ok=1 AND ok_source='flag') FROM events WHERE kind='tool_result'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(flags, (1, 1));
}

#[test]
fn native_copy_ownership_uses_origin_time_before_filename() {
    let (dir, mut store) = indexed(
        "claude",
        "{\"type\":\"user\",\"uuid\":\"shared-native\",\"sessionId\":\"origin\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"same\"}}\n",
    );
    fs::write(dir.path().join("logs/aaa-copy.jsonl"), "{\"type\":\"system\",\"timestamp\":\"2026-02-01T00:00:00Z\"}\n{\"type\":\"user\",\"uuid\":\"shared-native\",\"sessionId\":\"copy\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"same\"}}\n{\"type\":\"user\",\"uuid\":\"new-native\",\"sessionId\":\"copy\",\"timestamp\":\"2026-02-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"new\"}}\n").unwrap();
    store
        .refresh(
            &[Root {
                harness: "claude".into(),
                path: dir.path().join("logs"),
            }],
            false,
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM canonical_events WHERE session_id='origin'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn inherited_models_change_retrospective_group_counts() {
    for (harness, content, model, kind, role) in [
        (
            "claude",
            include_str!("../testdata/claude.jsonl"),
            "claude-sonnet",
            "message",
            "user",
        ),
        (
            "codex",
            include_str!("../testdata/codex.jsonl"),
            "gpt-fixture",
            "tool_call",
            "assistant",
        ),
        (
            "pi",
            include_str!("../testdata/pi.jsonl"),
            "pi-model",
            "tool_result",
            "tool",
        ),
    ] {
        let (_, store) = indexed(harness, content);
        let n: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM canonical_events WHERE model=? AND kind=? AND role=?",
                [model, kind, role],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{harness}");
    }
}

#[test]
fn call_outcomes_are_isolated_by_harness() {
    let (dir, mut store) = indexed(
        "claude",
        "{\"type\":\"assistant\",\"sessionId\":\"shared-session\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"shared-call\",\"name\":\"Bash\",\"input\":{\"command\":\"rg word\"}}]}}\n{\"type\":\"user\",\"sessionId\":\"shared-session\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"shared-call\",\"is_error\":true}]}}\n",
    );
    let other = dir.path().join("codex");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("one.jsonl"),"{\"type\":\"session_meta\",\"payload\":{\"id\":\"shared-session\"}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"shared-call\",\"name\":\"exec_command\",\"arguments\":\"{\\\"cmd\\\":\\\"rg word\\\"}\"}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"shared-call\",\"output\":\"Process exited with code 0\"}}\n").unwrap();
    store
        .refresh(
            &[Root {
                harness: "codex".into(),
                path: other,
            }],
            false,
            None,
        )
        .unwrap();
    let rows = counting::count(
        &store.db,
        "failures",
        "",
        None,
        &Filters {
            harness: vec![sessidx::query::Harness::Codex],
            ..Filters::default()
        },
    )
    .unwrap();
    assert_eq!(rows[0]["numerator"], 0);
    assert_eq!(rows[0]["denominator"], 1);
}

#[test]
fn codex_mcp_transport_error_flag_is_text_evidence_with_content_control() {
    let mut state = State::default();
    let es:Vec<_>=[
        serde_json::json!({"status":"fulfilled","value":{"isError":true,"content":[{"type":"text","text":"synthetic failure"}]}}),
        serde_json::json!({"status":"fulfilled","value":{"isError":false,"content":[{"type":"text","text":"=== refusal.txt ===\nScript error: Command blocked by PreToolUse hook: rg has no --include flag."}]}}),
        serde_json::json!({"opaque":"synthetic result"}),
        serde_json::json!([{ "isError":true },{ "isError":false }]),
    ].iter().flat_map(|v|adapters::parse("codex",&serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","output":v.to_string()}}),&mut state).events).collect();
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "text")
            .count(),
        2
    );
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(true) && e.ok_source == "text")
            .count(),
        1
    );
    assert_eq!(es.iter().filter(|e| e.ok.is_none()).count(), 1);
    assert_eq!(es.iter().flat_map(|e| &e.denials).count(), 0);
}

#[test]
fn result_envelopes_exclude_quoted_markers_and_preserve_native_denials() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/envelopes.json")).unwrap();
    for (i, f) in fixtures.as_array().unwrap().iter().enumerate() {
        let harness = f["harness"].as_str().unwrap();
        let mut e = sessidx::model::Event::new("tool", "tool_result", "fixture");
        if harness != "codex" {
            e.ok = f["ok"].as_bool();
        }
        sessidx::outcomes::classify(&mut e, &f["output"], harness);
        assert_eq!(e.ok, f["ok"].as_bool(), "fixture {i}");
        assert_eq!(e.exit_code, f["exit_code"].as_i64(), "fixture {i}");
        let sources: Vec<_> = e.denials.iter().map(|(s, _)| s.as_str()).collect();
        let expected: Vec<_> = f["source"].as_str().into_iter().collect();
        assert_eq!(sources, expected, "fixture {i}");
    }
    let record = serde_json::json!({"type":"user","toolDenialKind":"permission-rule","message":{"content":[{"type":"tool_result","is_error":true,"content":"Permission to use Bash has been denied."}]}});
    let r = adapters::parse("claude", &record, &mut State::default());
    assert_eq!(
        r.events[0].denials,
        [("permission_rule".into(), "unknown".into())]
    );
}

#[test]
fn codex_code_mode_literals_decode_exactly_and_other_sites_stay_unparsed() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../testdata/code-mode.json")).unwrap();
    for f in fixtures.as_array().unwrap() {
        let r = adapters::parse(
            "codex",
            &serde_json::json!({"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"wrapper","input":f["source"]}}),
            &mut State::default(),
        );
        let sites = &r.events[0].sites;
        let mut programs: Vec<_> = sites.iter().filter_map(|s| s.program.as_deref()).collect();
        let mut expected: Vec<_> = f["programs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        programs.sort();
        expected.sort();
        assert_eq!(programs, expected, "{}", f["name"]);
        assert_eq!(
            sites.iter().filter(|s| !s.parsed).count(),
            f["unparsed"].as_u64().unwrap() as usize,
            "{}",
            f["name"]
        );
        if let Some(argv) = f["argv"].as_array() {
            let expected: Vec<Vec<String>> =
                serde_json::from_value(serde_json::Value::Array(argv.clone())).unwrap();
            assert_eq!(
                sites.iter().map(|s| s.argv.clone()).collect::<Vec<_>>(),
                expected,
                "{}",
                f["name"]
            );
        }
    }
}
