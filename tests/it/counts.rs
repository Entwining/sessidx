use crate::common::indexed;
use sessidx::{
    counting::{self, By, Metric},
    discovery::Root,
    model::{Harness, name},
    query::{Filters, Kind, Role},
    store::Store,
};
use std::fs;

#[test]
fn counts_state_units_denominators_unknowns_and_sql_is_read_only() {
    let (_dir, store, roots) = indexed(Harness::Claude, include_str!("../fixtures/claude.jsonl"));
    let commands = counting::count(
        &store.db,
        Metric::Commands,
        &[],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(commands[0]["unit"], "static_shell_command_sites");
    assert_eq!(commands[0]["numerator"], 1);
    assert_eq!(commands[0]["denominator"], 2);
    let failures = counting::count(
        &store.db,
        Metric::Failures,
        &[],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(failures[0]["numerator"], 1);
    assert_eq!(failures[0]["denominator"], 1);
    let grouped = counting::count(
        &store.db,
        Metric::Commands,
        &[By::Harness, By::Model, By::Role, By::Week, By::Kind],
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
    let sessions = [
        "claude-fixture".to_owned(),
        roots[0]
            .path
            .join("session.jsonl")
            .to_string_lossy()
            .into_owned(),
    ];
    for (metric, role, expected) in [
        (Metric::Commands, Role::Assistant, commands),
        (Metric::Failures, Role::Assistant, failures),
        (
            Metric::Denials,
            Role::Tool,
            counting::count(
                &store.db,
                Metric::Denials,
                &[],
                Some("rg"),
                &Filters::default(),
            )
            .unwrap(),
        ),
    ] {
        for session in &sessions {
            let mut filters = Filters {
                harness: vec![Harness::Claude, Harness::Pi],
                role: Some(role),
                kind: Some(Kind::Unknown),
                session: Some(session.clone()),
                since: Some("2026-10-01T00:00:00Z".parse().unwrap()),
                until: Some("2026-10-02T00:00:00Z".parse().unwrap()),
                cwd: Some("/synthetic".into()),
                ..Filters::default()
            };
            assert_eq!(
                counting::count(&store.db, metric, &[], Some("rg"), &filters).unwrap(),
                expected,
                "{metric:?} {session}"
            );
            filters.since = Some("2026-10-02T00:00:00Z".parse().unwrap());
            let empty = counting::count(&store.db, metric, &[], Some("rg"), &filters).unwrap();
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
    let (_, store, _) = indexed(Harness::Claude, data);
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
    let (_, store, _) = indexed(Harness::Claude, &fixture);
    let result = counting::count(
        &store.db,
        Metric::Failures,
        &[],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(result[0]["denominator"], 1);
    assert_eq!(result[0]["unclassified"], 2);
    let result = counting::count(
        &store.db,
        Metric::Denials,
        &[],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(result[0]["unclassified"], 1);
}

/// Every fixture indexed from two files into one store.
fn copied_history() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    // Denied calls whose program selection depends on matching the call: two
    // `rg` sites in one call, a call without `rg`, and a server call with `rg`.
    let claude = format!(
        "{}{}",
        include_str!("../fixtures/claude.jsonl"),
        concat!(
            r#"{"type":"assistant","uuid":"c3","sessionId":"claude-fixture","timestamp":"2026-10-01T00:00:02Z","message":{"role":"assistant","model":"claude-sonnet","content":[{"type":"tool_use","id":"call-twice","name":"Bash","input":{"command":"rg one; rg two"}},{"type":"tool_use","id":"call-ls","name":"Bash","input":{"command":"ls src"}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"c4","sessionId":"claude-fixture","timestamp":"2026-10-01T00:00:03Z","toolDenialKind":"hook","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-twice","is_error":true,"content":"Permission to use Bash has been denied."}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"c5","sessionId":"claude-fixture","timestamp":"2026-10-01T00:00:04Z","toolDenialKind":"hook","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-ls","is_error":true,"content":"Permission to use Bash has been denied."}]}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"c6","sessionId":"claude-fixture","timestamp":"2026-10-01T00:00:05Z","message":{"role":"assistant","model":"claude-sonnet","content":[{"type":"server_tool_use","id":"call-server","name":"Bash","input":{"command":"rg three"}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"c7","sessionId":"claude-fixture","timestamp":"2026-10-01T00:00:06Z","toolDenialKind":"hook","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-server","is_error":true,"content":"Permission to use Bash has been denied."}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"shared","sessionId":"claude-fixture","message":{"role":"user","content":"one"}}"#,
            "\n",
        )
    );
    // The same native ID in another harness, and on another kind in one file.
    let pi = format!(
        "{}{}",
        include_str!("../fixtures/pi.jsonl"),
        concat!(
            r#"{"type":"message","id":"shared","message":{"role":"user","content":"two"}}"#,
            "\n",
            r#"{"type":"message","id":"shared","message":{"role":"toolResult","toolCallId":"call","isError":true,"content":"synthetic failure"}}"#,
            "\n",
        )
    );
    let mut roots = Vec::new();
    for (harness, fixture) in [
        (Harness::Claude, claude.as_str()),
        (Harness::Codex, include_str!("../fixtures/codex.jsonl")),
        (Harness::Pi, pi.as_str()),
    ] {
        let root = dir.path().join(name(harness));
        fs::create_dir(&root).unwrap();
        // A resumed or forked session repeats its parent's records with the same
        // native IDs; the copy's own context can attribute them differently.
        fs::write(root.join("a.jsonl"), fixture).unwrap();
        fs::write(
            root.join("b.jsonl"),
            fixture.replace("gpt-fixture", "gpt-copy"),
        )
        .unwrap();
        roots.push(Root {
            harness,
            path: root,
        });
    }
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    store.refresh(&roots, false, None).unwrap();
    (dir, store)
}

#[test]
fn counts_equal_the_public_views_over_copied_history() {
    let (_dir, store) = copied_history();
    let rows = |sql: &str| -> i64 { store.db.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert!(
        rows("SELECT count(*) FROM canonical_events WHERE kind IN ('tool_call','tool_result')")
            < rows("SELECT count(*) FROM events WHERE kind IN ('tool_call','tool_result')")
    );
    let by = [By::Harness, By::Model, By::Role, By::Week, By::Kind];
    let keys = "f.harness,coalesce(e.model,'unknown'),e.role,coalesce(strftime('%G-W%V',e.ts),'unknown'),s.kind";
    let from =
        "FROM canonical_events e JOIN files f ON f.id=e.file_id JOIN sessions s ON s.file_id=f.id";
    // References over the public views, with and without a program selection.
    for program in [None, Some("rg")] {
        let (selected_site, selected_call, unknown_call, selected_result, unknown_result) =
            match program {
                None => ("1", "1", "0", "1", "0"),
                Some(_) => (
                    "c.program='rg'",
                    "EXISTS(SELECT 1 FROM commands cp WHERE cp.event_id=e.id AND cp.program='rg')",
                    "EXISTS(SELECT 1 FROM commands cp WHERE cp.event_id=e.id AND (cp.parsed=0 OR cp.program IS NULL))",
                    "EXISTS(SELECT 1 FROM commands cp JOIN canonical_events ce ON ce.id=cp.event_id JOIN files cf ON cf.id=ce.file_id WHERE cf.harness=f.harness AND ce.session_id=e.session_id AND ce.call_id=e.call_id AND cp.program='rg')",
                    "EXISTS(SELECT 1 FROM commands cp JOIN events ce ON ce.id=cp.event_id JOIN files cf ON cf.id=ce.file_id WHERE cf.harness=f.harness AND ce.session_id=e.session_id AND ce.call_id=e.call_id AND (cp.parsed=0 OR cp.program IS NULL))",
                ),
            };
        for (metric, sql) in [
            (
                Metric::Commands,
                format!(
                    "SELECT {keys},sum(c.parsed=1 AND {selected_site}),sum(c.parsed=1),sum(c.parsed=0 OR c.program IS NULL) {from} JOIN commands c ON c.event_id=e.id WHERE e.kind='tool_call' GROUP BY {keys}"
                ),
            ),
            (
                Metric::Failures,
                format!(
                    "SELECT {keys},sum(o.ok=0 AND {selected_call}),sum({selected_call}),sum(({selected_call} AND o.ok IS NULL) OR {unknown_call}) {from} LEFT JOIN call_outcomes o ON o.harness=f.harness AND o.session_id=e.session_id AND o.call_id=e.call_id WHERE e.kind='tool_call' GROUP BY {keys}"
                ),
            ),
            (
                Metric::Denials,
                format!(
                    "SELECT {keys},sum(EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id) AND {selected_result}),sum({selected_result}),sum(({selected_result} AND EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown')) OR {unknown_result}) {from} WHERE e.kind='tool_result' GROUP BY {keys}"
                ),
            ),
        ] {
            let mut expected: Vec<Vec<String>> = store
                .db
                .prepare(&sql)
                .unwrap()
                .query_map([], |r| {
                    (0..8)
                        .map(|i| {
                            r.get_ref(i).map(|v| match v {
                                rusqlite::types::ValueRef::Text(t) => {
                                    String::from_utf8_lossy(t).into_owned()
                                }
                                v => v.as_i64().unwrap().to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            expected.sort();
            let mut counted: Vec<Vec<String>> =
                counting::count(&store.db, metric, &by, program, &Filters::default())
                    .unwrap()
                    .iter()
                    .map(|row| {
                        ["harness", "model", "role", "week", "kind"]
                            .iter()
                            .map(|k| row[k].as_str().unwrap().to_owned())
                            .chain(
                                ["numerator", "denominator", "unclassified"]
                                    .iter()
                                    .map(|k| row[k].to_string()),
                            )
                            .collect()
                    })
                    .collect();
            counted.sort();
            assert!(
                expected.iter().any(|row| row[6] != "0"),
                "{metric:?} {program:?} selects nothing"
            );
            assert_eq!(counted, expected, "{metric:?} {program:?}");
        }
    }
}

#[test]
fn canonical_events_are_the_first_ranked_copy_of_each_native_event() {
    let (_dir, store) = copied_history();
    let ranked = "SELECT * FROM (SELECT e.*, row_number() OVER (PARTITION BY f.harness, CASE WHEN e.native_id IS NULL THEN 'row:'||e.id ELSE 'native:'||e.native_id END, e.kind ORDER BY coalesce(f.first_ts,'9999'),f.path,e.line_no,e.ordinal) AS copy_rank FROM events e JOIN files f ON f.id=e.file_id) WHERE copy_rank=1";
    let rows = |sql: &str| -> i64 { store.db.query_row(sql, [], |r| r.get(0)).unwrap() };
    let canonical = rows("SELECT count(*) FROM canonical_events");
    assert!(canonical < rows("SELECT count(*) FROM events"));
    assert_eq!(canonical, rows(&format!("SELECT count(*) FROM ({ranked})")));
    assert_eq!(
        rows(&format!(
            "SELECT count(*) FROM (SELECT * FROM canonical_events EXCEPT {ranked})"
        )),
        0
    );
}
