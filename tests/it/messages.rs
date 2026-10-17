use crate::common::{indexed, indexed_files};
use sessidx::{
    adapters,
    model::{Agent, EventKind, SessionKind, State, name},
    store::Store,
};
use sha2::Digest;
use std::fs;

#[test]
fn native_message_fragments_empty_replies_summary_and_history_dedup() {
    let (_dir, mut store, roots) =
        indexed(Agent::Claude, include_str!("../fixtures/structure.jsonl"));
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
        roots[0].path.join("session.jsonl"),
        roots[0].path.join("copied.jsonl"),
    )
    .unwrap();
    store.refresh(&roots, false, None).unwrap();
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
fn canonical_message_text_joins_its_fragments_in_the_kept_transcript() {
    let fragments = include_str!("../fixtures/fragments.jsonl");
    let resumed = format!(
        "{fragments}{}\n",
        r#"{"type":"assistant","uuid":"r1","sessionId":"fragments","timestamp":"2026-10-01T00:00:08Z","message":{"id":"msg-thinking-first","role":"assistant","model":"claude-x","content":[{"type":"text","text":"copy only"}]}}"#
    );
    let (_dir, store, _) = indexed_files(
        Agent::Claude,
        &[("fragments", fragments), ("resumed", &resumed)],
    );
    let rows = store
        .db
        .prepare("SELECT path, line_no, text FROM canonical_events WHERE (kind='message' AND role='assistant') OR kind='tool_result' ORDER BY line_no")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?.ends_with("/fragments.jsonl"),
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (true, 1, "the visible answer".to_owned()),
            (true, 3, "text first".to_owned()),
            (true, 5, "same output".to_owned()),
            (true, 7, "part one\npart two".to_owned()),
            (true, 9, String::new()),
        ]
    );
    let columns = |view: &str| {
        store
            .db
            .prepare(&format!("SELECT name FROM pragma_table_info('{view}')"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(columns("canonical_events"), columns("events"));
    let plan = store
        .db
        .prepare("EXPLAIN QUERY PLAN SELECT text FROM canonical_events WHERE kind='message'")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for probe in [
        "SEARCH f USING INDEX events_native (native_id=? AND kind=?)",
        "SEARCH d USING INDEX events_native (native_id=? AND kind=? AND rowid<?)",
    ] {
        assert!(plan.iter().any(|s| s == probe), "{plan:?}");
    }
}

#[test]
fn pi_empty_response_and_explicit_message_model_override() {
    let records = [
        serde_json::json!({"type":"model_change","modelId":"last-model"}),
        serde_json::json!({"type":"message","id":"empty-attempt","message":{"role":"assistant","model":"new-model","content":[]}}),
        serde_json::json!({"type":"message","id":"retry-attempt","message":{"role":"assistant","content":[{"type":"text","text":"retry"}]}}),
    ];
    let (_, store, _) = indexed(
        Agent::Pi,
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
        serde_json::from_str(include_str!("../fixtures/attribution.json")).unwrap();
    for h in [Agent::Claude, Agent::Codex, Agent::Pi] {
        // The indexer seeds a Claude transcript's identity from its file name.
        let mut s = State {
            session_id: sessidx::redaction::Label::new("delegated"),
            ..State::default()
        };
        let es: Vec<_> = fixtures[name(h)]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|v| adapters::parse(h, v, &mut s))
            .collect();
        if h == Agent::Claude {
            assert_eq!(s.session_kind, SessionKind::Subagent);
            assert_eq!(s.session_kind_source, "isSidechain");
            assert_eq!(
                es.iter()
                    .flat_map(|e| &e.denials)
                    .filter(|d| d.source == "classifier")
                    .count(),
                1
            );
            assert_eq!(
                es.iter()
                    .flat_map(|e| &e.denials)
                    .filter(|d| d.source == "hook" && d.reason_id == "rg_include")
                    .count(),
                1
            );
            assert!(
                es.iter()
                    .filter(|e| &*e.role == "user")
                    .all(|e| e.denials.is_empty())
            );
        } else if h == Agent::Codex {
            assert_eq!(
                usize::from(
                    &*s.session_id == "child-thread"
                        && s.parent_id.as_deref() == Some("parent-thread")
                ),
                1
            );
            // The injected AGENTS.md message stands in for absent base instructions.
            assert_eq!(
                s.instruction_hash.as_deref(),
                Some(
                    hex::encode(sha2::Sha256::digest(
                        "# AGENTS.md instructions\nSynthetic fallback instructions"
                    ))
                    .as_str()
                )
            );
            assert_eq!(es.iter().filter(|e| &*e.role == "assistant").count(), 2);
        } else {
            assert_eq!(usize::from(s.parent_id.as_deref() == Some("pi-parent")), 1);
            assert_eq!(
                es.iter()
                    .flat_map(|e| &e.denials)
                    .filter(|d| d.source == "guard" && d.reason_id == "rg_include")
                    .count(),
                1
            );
            assert_eq!(
                es.iter().filter(|e| e.kind == EventKind::ToolCall).count(),
                0
            );
        }
    }
}

#[test]
fn native_copy_ownership_uses_origin_time_before_filename() {
    let (_dir, mut store, roots) = indexed_files(
        Agent::Claude,
        &[(
            "origin",
            "{\"type\":\"user\",\"uuid\":\"shared-native\",\"sessionId\":\"origin\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"same\"}}\n",
        )],
    );
    fs::write(roots[0].path.join("aaa-copy.jsonl"), "{\"type\":\"system\",\"timestamp\":\"2026-02-01T00:00:00Z\"}\n{\"type\":\"user\",\"uuid\":\"shared-native\",\"sessionId\":\"copy\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"same\"}}\n{\"type\":\"user\",\"uuid\":\"new-native\",\"sessionId\":\"copy\",\"timestamp\":\"2026-02-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"new\"}}\n").unwrap();
    store.refresh(&roots, false, None).unwrap();
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
    for (agent, content, model, kind, role) in [
        (
            Agent::Claude,
            include_str!("../fixtures/claude.jsonl"),
            "claude-sonnet",
            "message",
            "user",
        ),
        (
            Agent::Codex,
            include_str!("../fixtures/codex.jsonl"),
            "gpt-fixture",
            "tool_call",
            "assistant",
        ),
        (
            Agent::Pi,
            include_str!("../fixtures/pi.jsonl"),
            "pi-model",
            "tool_result",
            "tool",
        ),
    ] {
        let (_, store, _) = indexed(agent, content);
        let n: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM canonical_events WHERE model=? AND kind=? AND role=?",
                [model, kind, role],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{agent:?}");
    }
}

#[test]
fn a_copied_codex_call_counts_once_and_distinct_pi_calls_sharing_a_short_id_do_not_merge() {
    let call = r#"{"timestamp":"2026-10-01T00:00:00Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call_copied","arguments":"{\"cmd\":\"ls\"}"}}"#;
    let output = r#"{"timestamp":"2026-10-01T00:00:01Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_copied","output":"ok"}}"#;
    let rollout = |id: &str| {
        format!(
            "{{\"timestamp\":\"2026-10-01T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"source\":\"cli\"}}}}\n{call}\n{output}\n"
        )
    };
    let (_dir, codex, _) = indexed_files(
        Agent::Codex,
        &[
            ("rollout-a", &rollout("thread-a")),
            ("rollout-b", &rollout("thread-b")),
        ],
    );
    // Pi writes parallel calls as blocks of one record.
    let pi_calls = |record: &str, commands: &[&str]| {
        let blocks: Vec<_> = commands
            .iter()
            .map(|c| serde_json::json!({"type":"toolCall","id":"abc123","name":"bash","arguments":{"command":c}}))
            .collect();
        serde_json::json!({"type":"message","id":record,"timestamp":"2026-10-01T00:00:00Z","message":{"role":"assistant","content":blocks}}).to_string() + "\n"
    };
    // A later call in the same session can reuse the ID, so each result
    // answers the call before it.
    let pi_result = |record: &str, failed: bool| {
        serde_json::json!({"type":"message","id":record,"message":{"role":"toolResult","toolCallId":"abc123","toolName":"bash","isError":failed,"content":[]}}).to_string() + "\n"
    };
    let (_dir, pi, _) = indexed_files(
        Agent::Pi,
        &[
            ("pi-a", &pi_calls("rec00001", &["ls"])),
            ("pi-b", &pi_calls("rec00002", &["pwd", "date"])),
            (
                "pi-c",
                &[
                    pi_calls("rec00003", &["false"]),
                    pi_result("rec00004", true),
                    pi_calls("rec00005", &["true"]),
                    pi_result("rec00006", false),
                ]
                .concat(),
            ),
        ],
    );
    let canonical = |store: &sessidx::store::Store, kind: &str| -> i64 {
        store
            .db
            .query_row(
                "SELECT count(*) FROM canonical_events WHERE kind=?",
                [kind],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(canonical(&codex, "tool_call"), 1);
    assert_eq!(canonical(&codex, "tool_result"), 1);
    assert_eq!(canonical(&pi, "tool_call"), 5);
    let outcomes: Vec<Option<i64>> = pi
        .db
        .prepare("SELECT ok FROM call_outcomes WHERE path LIKE '%pi-c%' ORDER BY line_no")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(outcomes, [Some(0), Some(1)]);
}
