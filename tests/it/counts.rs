use crate::common::indexed;
use sessidx::{
    counting,
    query::{Filters, Harness, Kind, Role},
};

#[test]
fn counts_state_units_denominators_unknowns_and_sql_is_read_only() {
    let (_dir, store, roots) = indexed("claude", include_str!("../fixtures/claude.jsonl"));
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
        roots[0]
            .path
            .join("session.jsonl")
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
    let (_, store, _) = indexed("claude", data);
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
fn program_filtered_failures_report_unparsed_calls_as_unclassified() {
    let fixture=include_str!("../fixtures/claude.jsonl").to_owned()+&serde_json::json!({"type":"assistant","uuid":"bad-call","sessionId":"claude-fixture","message":{"role":"assistant","content":[{"type":"tool_use","id":"bad-shell","name":"Bash","input":{"command":"echo 'unterminated"}},{"type":"tool_use","id":"no-command","name":"Bash","input":{}}]}}).to_string()+"\n"+&serde_json::json!({"type":"user","uuid":"bad-result","sessionId":"claude-fixture","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"bad-shell","content":"done"}]}}).to_string()+"\n";
    let (_, store, _) = indexed("claude", &fixture);
    let result =
        counting::count(&store.db, "failures", "", Some("rg"), &Filters::default()).unwrap();
    assert_eq!(result[0]["denominator"], 1);
    assert_eq!(result[0]["unclassified"], 2);
    let result =
        counting::count(&store.db, "denials", "", Some("rg"), &Filters::default()).unwrap();
    assert_eq!(result[0]["unclassified"], 1);
}
