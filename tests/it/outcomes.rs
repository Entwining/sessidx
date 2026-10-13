use crate::common::{events, indexed};
use clap::ValueEnum;
use sessidx::{
    adapters,
    counting::{self, Metric},
    discovery::Root,
    model::{Agent, EventKind, State},
    outcomes,
    query::Filters,
};
use std::fs;

#[test]
fn codex_text_array_batch_failures_and_quoted_negative_control() {
    let (_, es) = events(Agent::Codex, include_str!("../fixtures/outcomes.jsonl"));
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "text")
            .count(),
        5
    );
    assert_eq!(
        es.iter()
            .flat_map(|e| &e.denials)
            .filter(|d| d.source == "hook")
            .count(),
        1
    );
    assert_eq!(
        es.iter()
            .flat_map(|e| &e.denials)
            .filter(|d| d.source == "batch_hook")
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
    let (_, store, _) = indexed(Agent::Codex, include_str!("../fixtures/truncated.jsonl"));
    let mut stmt = store
        .db
        .prepare(
            "SELECT source,count(*) FROM denials WHERE reason_id='rg_include' GROUP BY source ORDER BY source",
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
    for (agent, value) in [
        (
            Agent::Claude,
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"DENIED: rg has no --include flag."}]}}),
        ),
        (
            Agent::Pi,
            serde_json::json!({"type":"message","message":{"role":"toolResult","content":"DENIED: rg has no --include flag."}}),
        ),
        (
            Agent::Claude,
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","is_error":false,"content":"DENIED: rg has no --include flag."}]}}),
        ),
        (
            Agent::Pi,
            serde_json::json!({"type":"message","message":{"role":"toolResult","isError":false,"content":"DENIED: rg has no --include flag."}}),
        ),
        (
            Agent::Codex,
            serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","output":"Process exited with code 0\nOutput:\nDENIED: rg has no --include flag."}}),
        ),
        (
            Agent::Claude,
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"Permission to use Bash has been denied.\nPreToolUse:Bash hook error: quoted log line"}]}}),
        ),
    ] {
        let r = adapters::parse(agent, &value, &mut State::default());
        assert_eq!(
            r.events.iter().flat_map(|e| &e.denials).count(),
            0,
            "{agent:?}"
        );
    }
}

#[test]
fn claude_hook_check_errors_respect_native_success_flags() {
    let (_, store, _) = indexed(Agent::Claude, include_str!("../fixtures/hook-check.jsonl"));
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
fn call_outcomes_are_isolated_by_agent() {
    let (dir, mut store, _) = indexed(
        Agent::Claude,
        "{\"type\":\"assistant\",\"sessionId\":\"shared-session\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"shared-call\",\"name\":\"Bash\",\"input\":{\"command\":\"rg word\"}}]}}\n{\"type\":\"user\",\"sessionId\":\"shared-session\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"shared-call\",\"is_error\":true}]}}\n",
    );
    let other = dir.path().join("codex");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("one.jsonl"),"{\"type\":\"session_meta\",\"payload\":{\"id\":\"shared-session\"}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"shared-call\",\"name\":\"exec_command\",\"arguments\":\"{\\\"cmd\\\":\\\"rg word\\\"}\"}}\n{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"shared-call\",\"output\":\"Process exited with code 0\"}}\n").unwrap();
    store
        .refresh(
            &[Root {
                agent: Agent::Codex,
                path: other,
            }],
            false,
            None,
        )
        .unwrap();
    let rows = counting::count(
        &store.db,
        Metric::Failures,
        &[],
        None,
        &Filters {
            agent: vec![Agent::Codex],
            ..Filters::default()
        },
    )
    .unwrap();
    assert_eq!(rows[0]["failed"], 0);
    assert_eq!(rows[0]["calls"], 1);
}

#[test]
fn a_call_pairs_with_its_result_across_transcripts_of_one_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("codex");
    fs::create_dir(&root).unwrap();
    // A thread continued in a second rollout file can hold the results of
    // calls made in the first, one of them with unparsed shell syntax.
    let meta = r#"{"type":"session_meta","payload":{"id":"thread","source":"cli"}}"#;
    fs::write(
        root.join("rollout-1.jsonl"),
        format!(
            "{meta}\n{}\n{}\n",
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"split-call","name":"exec_command","arguments":"{\"cmd\":\"rg word\"}"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"split-unparsed","name":"exec_command","arguments":"{\"cmd\":\"echo 'unterminated\"}"}}"#
        ),
    )
    .unwrap();
    fs::write(
        root.join("rollout-2.jsonl"),
        format!(
            "{meta}\n{}\n{}\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"split-call","output":"Process exited with code 1"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"split-unparsed","output":"Process exited with code 0"}}"#
        ),
    )
    .unwrap();
    let mut store = sessidx::store::Store::open(&dir.path().join("index.db")).unwrap();
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
    let count = |metric| {
        counting::count(&store.db, metric, &[], Some("rg"), &Filters::default())
            .unwrap()
            .remove(0)
    };
    let failures = count(Metric::Failures);
    assert_eq!(
        (&failures["calls"], &failures["failed"]),
        (&1.into(), &1.into())
    );
    let denials = count(Metric::Denials);
    assert_eq!(
        (
            &denials["results"],
            &denials["results_with_unresolved_program"]
        ),
        (&1.into(), &1.into())
    );
    let outcomes: Vec<(Option<i64>, String)> = store
        .db
        .prepare("SELECT ok,ok_source FROM call_outcomes ORDER BY line_no")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        outcomes,
        [(Some(0), "text".into()), (Some(1), "text".into())]
    );
}

#[test]
fn a_call_reads_its_result_first_recorded_in_another_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("codex");
    fs::create_dir(&root).unwrap();
    let result = r#"{"type":"response_item","timestamp":"2026-10-02T00:00:01Z","payload":{"type":"function_call_output","id":"copied-result","call_id":"copied-call","output":"Process exited with code 1"}}"#;
    fs::write(
        root.join("rollout-earlier.jsonl"),
        format!(
            "{}\n{result}\n",
            r#"{"type":"session_meta","timestamp":"2026-10-01T00:00:00Z","payload":{"id":"earlier","source":"cli"}}"#
        ),
    )
    .unwrap();
    fs::write(
        root.join("rollout-later.jsonl"),
        format!(
            "{}\n{}\n{result}\n",
            r#"{"type":"session_meta","timestamp":"2026-10-02T00:00:00Z","payload":{"id":"later","source":"cli"}}"#,
            r#"{"type":"response_item","timestamp":"2026-10-02T00:00:00Z","payload":{"type":"function_call","id":"later-call","call_id":"copied-call","name":"exec_command","arguments":"{\"cmd\":\"rg needle\"}"}}"#
        ),
    )
    .unwrap();
    let mut store = sessidx::store::Store::open(&dir.path().join("index.db")).unwrap();
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
    let failures = counting::count(&store.db, Metric::Failures, &[], None, &Filters::default())
        .unwrap()
        .remove(0);
    assert_eq!(
        (&failures["calls"], &failures["failed"]),
        (&1.into(), &1.into())
    );
}

#[test]
fn codex_mcp_transport_error_flag_is_text_evidence_with_content_control() {
    let mut state = State::default();
    let es:Vec<_>=[
        serde_json::json!({"status":"fulfilled","value":{"isError":true,"content":[{"type":"text","text":"synthetic failure"}]}}),
        serde_json::json!({"status":"fulfilled","value":{"isError":false,"content":[{"type":"text","text":"=== refusal.txt ===\nScript error: Command blocked by PreToolUse hook: rg has no --include flag."}]}}),
        serde_json::json!({"opaque":"synthetic result"}),
        serde_json::json!([{ "isError":true },{ "isError":false }]),
    ].iter().flat_map(|v|adapters::parse(Agent::Codex,&serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","output":v.to_string()}}),&mut state).events).collect();
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
        let agent = Agent::from_str(f["agent"].as_str().unwrap(), false).unwrap();
        let mut e = sessidx::model::Event::new("tool", EventKind::ToolResult, "fixture");
        if agent != Agent::Codex {
            e.ok = f["ok"].as_bool();
        }
        sessidx::outcomes::classify(&mut e, &f["output"], agent);
        assert_eq!(e.ok, f["ok"].as_bool(), "fixture {i}");
        assert_eq!(e.exit_code, f["exit_code"].as_i64(), "fixture {i}");
        let sources: Vec<_> = e.denials.iter().map(|d| d.source).collect();
        let expected: Vec<_> = f["source"].as_str().into_iter().collect();
        assert_eq!(sources, expected, "fixture {i}");
    }
}

#[test]
fn claude_denial_kinds_map_channels_and_interruptions_record_no_denial() {
    // Claude Code 2.1.296 writes toolDenialKind from a fixed set and labels
    // hook denials permission-rule; permissionDecision (2.1.293+) names the
    // deciding channel. The structured value replaces the generic
    // text-derived native_denial source.
    let cases: &[(&str, Option<serde_json::Value>, &str)] = &[
        ("user-rejected", None, "user_rejected"),
        ("permission-rule", None, "permission_rule"),
        ("automode-blocked", None, "classifier"),
        ("automode-unavailable", None, "classifier_unavailable"),
        ("automode-parsing-error", None, "classifier_unavailable"),
        (
            "permission-rule",
            Some(serde_json::json!({"decision":"reject","source":"hook","reasonType":"hook"})),
            "hook",
        ),
        (
            "automode-blocked",
            Some(
                serde_json::json!({"decision":"reject","source":"config","reasonType":"classifier"}),
            ),
            "classifier",
        ),
        ("future-kind", None, "native_denial"),
    ];
    for (kind, decision, source) in cases {
        let mut record = serde_json::json!({"type":"user","toolDenialKind":kind,"message":{"content":[{"type":"tool_result","is_error":true,"content":"Permission to use Bash has been denied."}]}});
        if let Some(d) = decision {
            record["permissionDecision"] = d.clone();
        }
        let r = adapters::parse(Agent::Claude, &record, &mut State::default());
        assert_eq!(
            r.events[0].denials,
            [sessidx::model::Denial::channel(source)],
            "{kind} {decision:?}"
        );
    }
    // Claude Code itself maps interrupted and cancelled to an error outcome,
    // not a denial; the result still counts as failed from is_error.
    for kind in ["interrupted", "cancelled"] {
        let record = serde_json::json!({"type":"user","toolDenialKind":kind,"message":{"content":[{"type":"tool_result","is_error":true,"content":"synthetic interruption"}]}});
        let r = adapters::parse(Agent::Claude, &record, &mut State::default());
        assert!(r.events[0].denials.is_empty(), "{kind}");
        assert_eq!(r.events[0].ok, Some(false), "{kind}");
    }
    // A text-derived denial row is kept, with its reason, and the
    // structured channel is pushed only when no text-derived row remains.
    let record = serde_json::json!({"type":"user","toolDenialKind":"permission-rule","permissionDecision":{"decision":"reject","source":"hook","reasonType":"hook"},"message":{"content":[{"type":"tool_result","is_error":true,"content":"PreToolUse:Bash hook error: rg has no --include flag."}]}});
    let r = adapters::parse(Agent::Claude, &record, &mut State::default());
    assert_eq!(
        r.events[0].denials,
        [sessidx::model::Denial::legacy(
            "hook",
            "policy",
            "rg_include"
        )]
    );
}

#[test]
fn published_guard_sentences_classify_and_recognize_only_leading_pi_reasons() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/guard-reasons.json")).unwrap();
    let covered: std::collections::BTreeSet<&str> = fixtures
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["sentence"].as_str().unwrap())
        .collect();
    let published: std::collections::BTreeSet<&str> = outcomes::GUARD_REASONS
        .iter()
        .map(|(s, _, _, _)| *s)
        .collect();
    assert_eq!(
        published, covered,
        "every published sentence needs a fixture"
    );
    // The table is frozen: it never gains rows.
    assert_eq!(outcomes::GUARD_REASONS.len(), 67);
    for f in fixtures.as_array().unwrap() {
        let text = f["text"].as_str().unwrap();
        let sentence = f["sentence"].as_str().unwrap();
        let class = f["class"].as_str().unwrap();
        let id = f["reason_id"].as_str().unwrap();
        assert_eq!(
            outcomes::reason_id(&format!("DENIED: {text}")),
            (class, id),
            "{text}"
        );
        for output in [text.to_owned(), format!("Blocked: {text}")] {
            let mut e = sessidx::model::Event::new("tool", EventKind::ToolResult, "fixture");
            e.ok = Some(false);
            outcomes::classify(&mut e, &serde_json::json!(output), Agent::Pi);
            assert_eq!(e.denials.len(), 1, "{text}");
            let d = &e.denials[0];
            assert_eq!(
                (
                    d.source,
                    d.class,
                    d.producer.as_deref(),
                    d.reason_id.as_str()
                ),
                ("guard", class, None, id),
                "{text}"
            );
        }
        // Partial sentences, diagnostic mentions and quotes are not bare refusals.
        for output in [
            sentence[1..].to_owned(),
            sentence[..sentence.len() - 1].to_owned(),
            format!("diagnostic: {text}"),
            format!("\"{text}\""),
        ] {
            if !output.contains(sentence) {
                assert_eq!(
                    outcomes::reason_id(&output),
                    ("unknown", "unknown"),
                    "{output}"
                );
            }
            let mut e = sessidx::model::Event::new("tool", EventKind::ToolResult, "fixture");
            e.ok = Some(false);
            outcomes::classify(&mut e, &serde_json::json!(output), Agent::Pi);
            assert!(e.denials.is_empty(), "{text}");
        }
    }
    // The list order, not the position in the text, decides between two reasons.
    assert_eq!(
        outcomes::reason_id(
            "rg has no --include flag. The agent guard cannot inspect this shell syntax."
        ),
        ("coverage", "unsupported_shell_syntax")
    );
    for text in [
        "DENIED: unrelated policy.",
        "Could not parse the Bash command.",
        "PreToolUse guard could not inspect this input; normal permission checks still apply.",
        "Use rg in Bash instead of the built-in Grep tool.",
    ] {
        assert_eq!(outcomes::reason_id(text), ("unknown", "unknown"), "{text}");
    }
}

#[test]
fn marker_grammar_accepts_only_its_exact_shapes() {
    // marker = "[" class ":" producer "/" reason "]" followed by a space or
    // the end of the text. Rule: docs/formats.md `reason_marker`.
    let parsed = |slot: &str| outcomes::parse_marker(slot).map(|m| (m.class, m.producer, m.reason));
    for (slot, expected) in [
        (
            "[policy:example-guard/credential_file] reason text",
            Some(("policy", "example-guard", "credential_file")),
        ),
        (
            "[coverage:example-guard/unsupported_shell_syntax] reason text",
            Some(("coverage", "example-guard", "unsupported_shell_syntax")),
        ),
        (
            "[error:example-guard/deadline] reason text",
            Some(("error", "example-guard", "deadline")),
        ),
        (
            "[policy:example-guard/at_end]",
            Some(("policy", "example-guard", "at_end")),
        ),
        (
            "[policy:example.guard-1/r_2.a] reason text",
            Some(("policy", "example.guard-1", "r_2.a")),
        ),
        (
            "[policy:example-guard/UPPER_lower_1] reason text",
            Some(("policy", "example-guard", "UPPER_lower_1")),
        ),
        ("[security:example-guard/x] unknown class", None),
        ("[policy:1guard/x] producer starting with a digit", None),
        ("[policy:example-Guard/x] uppercase later in producer", None),
        ("[policy:example-guard/a/b] slash in reason", None),
        ("[policy:/x] empty producer", None),
        ("[policy:example-guard/] empty reason", None),
        ("[policy:example-guard/x missing bracket", None),
        ("[policy:example-guard/x]letter after bracket", None),
        ("[policy:example-guard/x]\nnewline after bracket", None),
        (
            "[policy:example-guard/x] tail [coverage:g/y] second",
            Some(("policy", "example-guard", "x")),
        ),
    ] {
        let expected = expected
            .map(|(class, producer, reason)| (class, producer.to_string(), reason.to_string()));
        assert_eq!(parsed(slot), expected, "{slot:?}");
    }
    // producer and reason end at 63 characters.
    let producer = format!("{}a", "b".repeat(62));
    let reason = "r".repeat(63);
    assert_eq!(
        parsed(&format!("[policy:{producer}/{reason}] tail")),
        Some(("policy", producer, reason))
    );
    let producer = format!("{}ac", "b".repeat(62));
    assert_eq!(parsed(&format!("[policy:{producer}/r] tail")), None);
    let reason = "r".repeat(64);
    assert_eq!(
        parsed(&format!("[policy:example-guard/{reason}] tail")),
        None
    );
}

#[test]
fn markers_read_at_slot_starts_and_win_over_frozen_sentences() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/markers.json")).unwrap();
    for (i, f) in fixtures.as_array().unwrap().iter().enumerate() {
        let agent = Agent::from_str(f["agent"].as_str().unwrap(), false).unwrap();
        let mut e = sessidx::model::Event::new("tool", EventKind::ToolResult, "fixture");
        if agent != Agent::Codex {
            e.ok = f["ok"].as_bool();
        }
        sessidx::outcomes::classify(&mut e, &f["output"], agent);
        assert_eq!(e.ok, f["ok"].as_bool(), "fixture {i}");
        let denials: Vec<_> = e
            .denials
            .iter()
            .map(|d| {
                (
                    d.source,
                    d.class,
                    d.producer.as_deref(),
                    d.reason_id.as_str(),
                )
            })
            .collect();
        let expected: Vec<_> = f["denials"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d[0].as_str().unwrap(),
                    d[1].as_str().unwrap(),
                    d[2].as_str(),
                    d[3].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(denials, expected, "fixture {i}");
    }
}

#[test]
fn marker_and_legacy_denial_rows_reach_the_sql_table() {
    let (_, store, _) = indexed(
        Agent::Claude,
        concat!(
            "{\"type\":\"assistant\",\"uuid\":\"marker-call\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"m\",\"name\":\"Bash\",\"input\":{\"command\":\"rg word\"}}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"marker-result\",\"toolDenialKind\":\"permission-rule\",\"permissionDecision\":{\"decision\":\"reject\",\"source\":\"hook\"},\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"m\",\"is_error\":true,\"content\":\"PreToolUse:Bash hook error: [policy:example-guard/credential_file] this reads a credential file.\"}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"legacy-result\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"is_error\":true,\"content\":\"DENIED: rg has no --include flag.\"}]}}\n",
        ),
    );
    let rows: Vec<(String, String, Option<String>, String)> = store
        .db
        .prepare("SELECT source,class,producer,reason_id FROM denials ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (
                "hook".into(),
                "policy".into(),
                Some("example-guard".into()),
                "credential_file".into()
            ),
            ("guard".into(), "policy".into(), None, "rg_include".into()),
        ]
    );
}
