use crate::common::{indexed, serial};
use sessidx::{
    adapters,
    model::{Harness, State, name},
    store::Store,
};
use sha2::Digest;
use std::fs;

#[test]
fn native_message_fragments_empty_replies_summary_and_history_dedup() {
    let _serial = serial();
    let (_dir, mut store, roots) =
        indexed(Harness::Claude, include_str!("../fixtures/structure.jsonl"));
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
fn pi_empty_response_and_explicit_message_model_override() {
    let records = [
        serde_json::json!({"type":"model_change","modelId":"last-model"}),
        serde_json::json!({"type":"message","id":"empty-attempt","message":{"role":"assistant","model":"new-model","content":[]}}),
        serde_json::json!({"type":"message","id":"retry-attempt","message":{"role":"assistant","content":[{"type":"text","text":"retry"}]}}),
    ];
    let (_, store, _) = indexed(
        Harness::Pi,
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
    for h in [Harness::Claude, Harness::Codex, Harness::Pi] {
        let mut s = State::default();
        let es: Vec<_> = fixtures[name(h)]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|v| adapters::parse(h, v, &mut s).events)
            .collect();
        if h == Harness::Claude {
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
        } else if h == Harness::Codex {
            assert_eq!(
                usize::from(
                    s.session_id == "child-thread"
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
fn native_copy_ownership_uses_origin_time_before_filename() {
    let _serial = serial();
    let (_dir, mut store, roots) = indexed(
        Harness::Claude,
        "{\"type\":\"user\",\"uuid\":\"shared-native\",\"sessionId\":\"origin\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"same\"}}\n",
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
    for (harness, content, model, kind, role) in [
        (
            Harness::Claude,
            include_str!("../fixtures/claude.jsonl"),
            "claude-sonnet",
            "message",
            "user",
        ),
        (
            Harness::Codex,
            include_str!("../fixtures/codex.jsonl"),
            "gpt-fixture",
            "tool_call",
            "assistant",
        ),
        (
            Harness::Pi,
            include_str!("../fixtures/pi.jsonl"),
            "pi-model",
            "tool_result",
            "tool",
        ),
    ] {
        let (_, store, _) = indexed(harness, content);
        let n: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM canonical_events WHERE model=? AND kind=? AND role=?",
                [model, kind, role],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{harness:?}");
    }
}
