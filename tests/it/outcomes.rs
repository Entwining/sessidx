use crate::common::{events, indexed, serial};
use sessidx::{adapters, counting, discovery::Root, model::State, query::Filters};
use std::fs;

#[test]
fn codex_text_array_batch_failures_and_quoted_negative_control() {
    let (_, es) = events("codex", include_str!("../fixtures/outcomes.jsonl"));
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
fn split_script_error_and_truncated_batch_are_denials_with_transport_quote_control() {
    let (_, store, _) = indexed("codex", include_str!("../fixtures/truncated.jsonl"));
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
        (
            "claude",
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"Permission to use Bash has been denied.\nPreToolUse:Bash hook error: quoted log line"}]}}),
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
fn claude_hook_check_errors_respect_native_success_flags() {
    let (_, store, _) = indexed("claude", include_str!("../fixtures/hook-check.jsonl"));
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
fn call_outcomes_are_isolated_by_harness() {
    let _serial = serial();
    let (dir, mut store, _) = indexed(
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
        serde_json::from_str(include_str!("../fixtures/envelopes.json")).unwrap();
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
    for (kind, source) in [
        ("permission-rule", "permission_rule"),
        ("classifier-unavailable", "classifier_unavailable"),
        ("automode-unavailable", "classifier_unavailable"),
        ("hook", "hook"),
        ("user-rejected", "user_rejected"),
    ] {
        let record = serde_json::json!({"type":"user","toolDenialKind":kind,"message":{"content":[{"type":"tool_result","is_error":true,"content":"Permission to use Bash has been denied."}]}});
        let r = adapters::parse("claude", &record, &mut State::default());
        assert_eq!(
            r.events[0].denials,
            [(source.into(), "unknown".into())],
            "{kind}"
        );
    }
}
