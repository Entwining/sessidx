use crate::common::{indexed, indexed_files};
use sessidx::{
    counting::{self, By, Metric},
    discovery::Root,
    model::{Agent, SessionKind, name},
    query::{Filters, Role},
    store::Store,
};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[test]
fn counts_select_by_program_and_shared_filters_and_sql_is_read_only() {
    let (_dir, store, roots) = indexed_files(
        Agent::Claude,
        &[("claude-fixture", include_str!("../fixtures/claude.jsonl"))],
    );
    let commands = counting::count(
        &store.db,
        Metric::Commands,
        &[],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(commands[0]["matching_sites"], 1);
    assert_eq!(commands[0]["sites"], 2);
    let failures = counting::count(
        &store.db,
        Metric::Failures,
        &[],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(failures[0]["failed"], 1);
    assert_eq!(failures[0]["calls"], 1);
    let grouped = counting::count(
        &store.db,
        Metric::Commands,
        &[By::Agent, By::Model, By::Role, By::Week, By::SessionKind],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(grouped.len(), 1);
    for (key, expected) in [
        ("agent", "claude"),
        ("model", "claude-sonnet"),
        ("role", "assistant"),
        ("week", "2026-W40"),
        ("session_kind", "unknown"),
    ] {
        assert_eq!(grouped[0][key], expected);
    }
    assert_eq!(grouped[0]["matching_sites"], 1);
    let sessions = [
        "claude-fixture".to_owned(),
        roots[0]
            .path
            .join("claude-fixture.jsonl")
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
                agent: vec![Agent::Claude, Agent::Pi],
                role: Some(role),
                session_kind: Some(SessionKind::Unknown),
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
            let row = empty[0].as_object().unwrap();
            assert!(row.values().filter_map(|v| v.as_i64()).all(|n| n == 0));
        }
    }
    assert!(counting::sql(&store.db, "DELETE FROM events").is_err());
    assert!(counting::sql(&store.db, "SELECT 1; DELETE FROM events").is_err());
    assert!(counting::sql(&store.db, "WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
    let duplicate = counting::sql(&store.db, "SELECT 1 AS id, 2 AS id");
    assert!(
        duplicate
            .unwrap_err()
            .to_string()
            .contains("alias each column")
    );
    assert_eq!(counting::doctor(&store.db).unwrap()["parse_errors"], 0);
}

#[test]
fn doctor_reports_stored_coverage_gaps_and_sql_bounds() {
    let data = concat!(
        "{\"type\":\"assistant\",\"sessionId\":\"doctor\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"bad\",\"name\":\"Bash\",\"input\":{\"command\":\"echo 'unterminated\"}}]}}\n",
        "{\"type\":\"user\",\"sessionId\":\"doctor\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"bad\",\"is_error\":true,\"content\":\"DENIED: synthetic policy\"}]}}\n",
        "{\"type\":\"future-shape\"}\n{invalid\n{\"partial\":"
    );
    let (_, store, _) = indexed(Agent::Claude, data);
    let d = counting::doctor(&store.db).unwrap();
    for (key, expected) in [
        ("files", 1),
        ("sessions", 1),
        ("events", 4),
        ("canonical_events", 4),
        ("parse_errors", 1),
        ("incomplete_files", 1),
        ("sessions_with_unknown_kind", 1),
        ("tool_events", 2),
        ("tool_events_without_model", 2),
        ("records", 4),
        ("records_with_unknown_shape", 2),
        ("shell_calls", 1),
        ("shell_calls_unparsed", 1),
        ("denials", 1),
        ("denials_with_unknown_reason", 1),
    ] {
        assert_eq!(d[key], expected, "{key}");
    }
    assert_eq!(d["unknown_shape_signatures"].as_array().unwrap().len(), 2);
    let excess = counting::sql(
        &store.db,
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10001) SELECT x FROM n",
    );
    assert!(excess.is_err());
    assert!(excess.unwrap_err().to_string().contains("10000 rows"));
    let start = std::time::Instant::now();
    let err = counting::sql(&store.db,"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n").unwrap_err();
    assert_eq!(
        err.to_string(),
        "SQL exceeded two seconds; narrow it by session_id or ts, or aggregate fewer rows"
    );
    let overflow = counting::sql(&store.db, "SELECT abs(-9223372036854775808) AS n").unwrap_err();
    assert!(
        overflow.to_string().contains("integer overflow"),
        "{overflow}"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
    assert_eq!(
        counting::sql(&store.db, "SELECT 1 AS n").unwrap()[0]["n"],
        1
    );
}

#[test]
fn program_selection_names_calls_whose_program_is_unresolved_apart_from_its_population() {
    let fixture=include_str!("../fixtures/claude.jsonl").to_owned()+&serde_json::json!({"type":"assistant","uuid":"bad-call","sessionId":"claude-fixture","message":{"role":"assistant","content":[{"type":"tool_use","id":"bad-shell","name":"Bash","input":{"command":"echo 'unterminated"}},{"type":"tool_use","id":"no-command","name":"Bash","input":{}}]}}).to_string()+"\n"+&serde_json::json!({"type":"user","uuid":"bad-result","sessionId":"claude-fixture","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"bad-shell","content":"done"}]}}).to_string()+"\n";
    let (_, store, _) = indexed(Agent::Claude, &fixture);
    let result = counting::count(
        &store.db,
        Metric::Failures,
        &[],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(result[0]["calls"], 1);
    assert_eq!(result[0]["outcome_unknown"], 0);
    assert_eq!(result[0]["calls_with_unresolved_program"], 2);
    let result = counting::count(
        &store.db,
        Metric::Denials,
        &[],
        Some("rg"),
        &Filters::default(),
    )
    .unwrap();
    assert_eq!(result[0]["results_with_unresolved_program"], 1);
}

/// Every fixture indexed from two files into one store.
fn copied_history() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    // Denied calls whose program selection depends on matching the call: two
    // `rg` sites in one call, a call without `rg`, a server call with `rg`,
    // and a call whose shell syntax does not parse; then an `rg` call with no
    // result, whose outcome is unknown.
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
            r#"{"type":"assistant","uuid":"c8","sessionId":"claude-fixture","timestamp":"2026-10-01T00:00:07Z","message":{"role":"assistant","model":"claude-sonnet","content":[{"type":"tool_use","id":"call-unparsed","name":"Bash","input":{"command":"echo 'unterminated"}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"c9","sessionId":"claude-fixture","timestamp":"2026-10-01T00:00:08Z","toolDenialKind":"hook","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-unparsed","is_error":true,"content":"Permission to use Bash has been denied."}]}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"c10","sessionId":"claude-fixture","timestamp":"2026-10-01T00:00:09Z","message":{"role":"assistant","model":"claude-sonnet","content":[{"type":"tool_use","id":"call-pending","name":"Bash","input":{"command":"rg pending"}}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"shared","sessionId":"claude-fixture","message":{"role":"user","content":"one"}}"#,
            "\n",
        )
    );
    // The same native ID in another agent, and on another kind in one file.
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
    for (agent, fixture) in [
        (Agent::Claude, claude.as_str()),
        (Agent::Codex, include_str!("../fixtures/codex.jsonl")),
        (Agent::Pi, pi.as_str()),
    ] {
        let root = dir.path().join(name(agent));
        fs::create_dir(&root).unwrap();
        // A resumed or forked session repeats its parent's records with the same
        // native IDs; the copy's own context can attribute them differently.
        fs::write(root.join("a.jsonl"), fixture).unwrap();
        fs::write(
            root.join("b.jsonl"),
            fixture.replace("gpt-fixture", "gpt-copy"),
        )
        .unwrap();
        roots.push(Root { agent, path: root });
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
    let by = [By::Agent, By::Model, By::Role, By::Week, By::SessionKind];
    let names = ["agent", "model", "role", "week", "session_kind"];
    let keys = "s.agent,coalesce(e.model,'unknown'),e.role,coalesce(strftime('%G-W%V',e.ts),'unknown'),s.session_kind";
    let events = "FROM canonical_events e JOIN sessions s ON s.file_id=e.file_id";
    let calls = "FROM call_outcomes e JOIN sessions s ON s.file_id=e.file_id";
    let unresolved = "(cp.parsed=0 OR cp.program IS NULL)";
    // References over the public views, with and without a program selection.
    for program in [None, Some("rg")] {
        let call = "EXISTS(SELECT 1 FROM commands cp WHERE cp.event_id=e.id AND cp.program='rg')";
        let result = "EXISTS(SELECT 1 FROM commands cp JOIN canonical_events ce ON ce.id=cp.event_id WHERE ce.agent=e.agent AND ce.session_id=e.session_id AND ce.call_id=e.call_id AND cp.program='rg')";
        let unresolved_call =
            format!("EXISTS(SELECT 1 FROM commands cp WHERE cp.event_id=e.id AND {unresolved})");
        let unresolved_result = format!(
            "EXISTS(SELECT 1 FROM commands cp JOIN events ce ON ce.id=cp.event_id WHERE ce.agent=e.agent AND ce.session_id=e.session_id AND ce.call_id=e.call_id AND ce.kind!='tool_result' AND {unresolved})"
        );
        let (call, result) = match program {
            None => ("1", "1"),
            Some(_) => (call, result),
        };
        let denied = "EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id)";
        let unknown_reason =
            "EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown')";
        let mut metrics = [
            (
                Metric::Commands,
                vec![
                    ("sites", "c.parsed=1".to_owned()),
                    (
                        "sites_without_program",
                        "c.parsed=1 AND c.program IS NULL".into(),
                    ),
                ],
                format!("{events} JOIN commands c ON c.event_id=e.id WHERE e.kind='tool_call'"),
            ),
            (
                Metric::Failures,
                vec![
                    ("calls", call.to_owned()),
                    ("failed", format!("{call} AND e.ok=0")),
                    ("succeeded", format!("{call} AND e.ok=1")),
                    ("outcome_unknown", format!("{call} AND e.ok IS NULL")),
                ],
                calls.to_owned(),
            ),
            (
                Metric::Denials,
                vec![
                    ("results", result.to_owned()),
                    ("denied", format!("{result} AND {denied}")),
                    (
                        "denied_reason_unknown",
                        format!("{result} AND {unknown_reason}"),
                    ),
                ],
                format!("{events} WHERE e.kind='tool_result'"),
            ),
        ];
        if program.is_some() {
            metrics[0]
                .1
                .push(("matching_sites", "c.parsed=1 AND c.program='rg'".into()));
            metrics[1].1.push((
                "calls_with_unresolved_program",
                format!("NOT {call} AND {unresolved_call}"),
            ));
            metrics[2].1.push((
                "results_with_unresolved_program",
                format!("NOT {result} AND {unresolved_result}"),
            ));
        }
        for (metric, fields, source) in metrics {
            let mut sums: Vec<String> = fields
                .iter()
                .map(|(_, condition)| format!("sum(CASE WHEN {condition} THEN 1 ELSE 0 END)"))
                .collect();
            if let Metric::Commands = metric {
                sums.insert(
                    2,
                    "count(DISTINCT CASE WHEN c.parsed=0 THEN e.id END)".into(),
                );
            }
            let mut fields: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
            if let Metric::Commands = metric {
                fields.insert(2, "unparsed_calls");
            }
            let sql = format!("SELECT {keys},{} {source} GROUP BY {keys}", sums.join(","));
            let width = names.len() + fields.len();
            let mut expected: Vec<Vec<String>> = store
                .db
                .prepare(&sql)
                .unwrap()
                .query_map([], |r| {
                    (0..width)
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
                        names
                            .iter()
                            .map(|k| row[k].as_str().unwrap().to_owned())
                            .chain(fields.iter().map(|k| row[k].to_string()))
                            .collect()
                    })
                    .collect();
            counted.sort();
            assert!(
                expected.iter().any(|row| row[names.len()] != "0"),
                "{metric:?} {program:?} selects nothing"
            );
            assert_eq!(counted, expected, "{metric:?} {program:?}");
        }
    }
}

#[test]
fn failure_count_work_follows_the_selected_calls() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("codex");
    fs::create_dir(&root).unwrap();
    // One session continued across many transcripts, each with one call.
    let calls = 300;
    for i in 0..calls {
        fs::write(
            root.join(format!("rollout-{i}.jsonl")),
            format!(
                "{}\n{}\n{}\n",
                r#"{"type":"session_meta","timestamp":"2026-10-02T00:00:00Z","payload":{"id":"thread","source":"cli"}}"#,
                format_args!(r#"{{"type":"response_item","timestamp":"2026-10-02T00:00:00Z","payload":{{"type":"function_call","call_id":"call-{i}","name":"exec_command","arguments":"{{\"cmd\":\"rg word\"}}"}}}}"#),
                format_args!(r#"{{"type":"response_item","timestamp":"2026-10-02T00:00:01Z","payload":{{"type":"function_call_output","call_id":"call-{i}","output":"Process exited with code 0"}}}}"#),
            ),
        )
        .unwrap();
    }
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    store
        .refresh(
            &[Root {
                agent: Agent::Codex,
                path: root,
            }],
            false,
            None,
        )
        .unwrap();
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<200000) INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,ts,role,role_source,kind,kind_source,model_source,ok_source) SELECT r.file_id,n.i+100,r.byte_off,r.byte_len,0,r.raw_hash,'2020-01-01T00:00:00.000Z',r.role,r.role_source,r.kind,r.kind_source,r.model_source,r.ok_source FROM n JOIN event_rows r ON r.id=1;").unwrap();
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
    let since = "2026-10-01T00:00:00.000Z";
    let counted = work(&|| {
        let rows = counting::count(
            &store.db,
            Metric::Failures,
            &[],
            None,
            &Filters {
                since: Some(since.parse().unwrap()),
                ..Filters::default()
            },
        )
        .unwrap();
        assert_eq!(
            (&rows[0]["calls"], &rows[0]["succeeded"]),
            (&calls.into(), &calls.into())
        );
    });
    let probed = work(&|| {
        let n: i64 = store
            .db
            .query_row(
                "SELECT count(ok) FROM call_outcomes WHERE ts>=?",
                [since],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, calls);
    });
    // One callback per 100 virtual machine instructions. A pass over the
    // 200,000 earlier events, or a probe of every transcript for each call,
    // costs thousands; probing each call once per reference to its outcome
    // costs about two and a half times the single probe.
    assert!(counted < 1000, "{counted} hundred instructions");
    assert!(
        counted < 2 * probed,
        "{counted} against {probed} for one probe"
    );
}

#[test]
fn canonical_events_are_the_first_ranked_copy_of_each_native_event() {
    let (_dir, store) = copied_history();
    let ranked = "SELECT * FROM events WHERE id IN (SELECT id FROM (SELECT e.id, row_number() OVER (PARTITION BY s.agent, CASE WHEN e.native_id IS NULL THEN 'row:'||e.id ELSE 'native:'||e.native_id END, e.kind ORDER BY coalesce(s.started_at,'9999'),s.path,e.line_no,e.ordinal) AS copy_rank FROM events e JOIN sessions s ON s.file_id=e.file_id) WHERE copy_rank=1)";
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

#[test]
fn count_rows_partition_their_population() {
    let (_dir, store) = copied_history();
    for program in [None, Some("rg")] {
        let rows = |metric| {
            counting::count(
                &store.db,
                metric,
                &[By::Agent],
                program,
                &Filters::default(),
            )
            .unwrap()
        };
        let n = |row: &serde_json::Value, key: &str| row[key].as_i64().unwrap();
        for row in rows(Metric::Failures) {
            assert_eq!(
                n(&row, "calls"),
                n(&row, "failed") + n(&row, "succeeded") + n(&row, "outcome_unknown"),
                "{row}"
            );
        }
        for row in rows(Metric::Denials) {
            assert!(
                n(&row, "denied_reason_unknown") <= n(&row, "denied"),
                "{row}"
            );
            assert!(n(&row, "denied") <= n(&row, "results"), "{row}");
        }
        for row in rows(Metric::Commands) {
            let matching = program.map_or(0, |_| n(&row, "matching_sites"));
            assert!(
                matching + n(&row, "sites_without_program") <= n(&row, "sites"),
                "{row}"
            );
        }
    }
}
