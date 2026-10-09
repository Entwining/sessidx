use crate::common::{events, indexed};
use clap::ValueEnum;
use sessidx::{
    adapters,
    discovery::Root,
    model::{Harness, State},
    store::Store,
};
use sha2::Digest;
use std::fs;

#[test]
fn claude_blocks_flags_and_synthetic_model() {
    let (s, es) = events(Harness::Claude, include_str!("../fixtures/claude.jsonl"));
    assert_eq!(es.iter().filter(|e| e.kind == "tool_call").count(), 1);
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "flag")
            .count(),
        1
    );
    assert_eq!(s.model.as_deref(), Some("claude-sonnet"));
    assert_eq!(es.iter().filter(|e| e.role == "user").count(), 1);
    assert!(
        es.iter()
            .filter(|e| e.kind == "tool_result")
            .all(|e| e.text.as_ref().is_some_and(|s| !s.is_empty()))
    );
}

#[test]
fn codex_context_arguments_and_telemetry() {
    let (mut s, es) = events(Harness::Codex, include_str!("../fixtures/codex.jsonl"));
    adapters::parse(
        Harness::Codex,
        &serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions\nlater fallback"}]}}),
        &mut s,
    );
    assert_eq!(s.model.as_deref(), Some("gpt-fixture"));
    assert_eq!(
        es.iter()
            .filter(|e| e.command.as_deref() == Some("rg word src"))
            .count(),
        1
    );
    assert_eq!(es.iter().filter(|e| e.kind == "message").count(), 1);
    assert_eq!(
        s.instruction_hash.as_deref(),
        Some(hex::encode(sha2::Sha256::digest("Synthetic instruction")).as_str())
    );
    let mut history = State::default();
    adapters::parse(
        Harness::Codex,
        &serde_json::json!({"type":"session_meta","payload":{"history_base":{"thread_id":"history-parent"}}}),
        &mut history,
    );
    assert_eq!(history.parent_id.as_deref(), Some("history-parent"));
    let search = adapters::parse(
        Harness::Codex,
        &serde_json::json!({"type":"response_item","payload":{"type":"tool_search_call","call_id":"search"}}),
        &mut history,
    );
    assert_eq!(search.events[0].tool.as_deref(), Some("tool_search"));
    let array = adapters::parse(
        Harness::Codex,
        &serde_json::json!({"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":{"command":["printf", "literal quote's"]}}}),
        &mut history,
    );
    assert_eq!(array.events[0].sites[0].program.as_deref(), Some("printf"));
    assert_eq!(
        array.events[0].sites[0].argv,
        ["'printf'", "'literal quote'\\''s'"]
    );
    assert!(
        es.iter()
            .filter(|e| e.kind == "tool_result")
            .all(|e| e.text.as_ref().is_some_and(|s| !s.is_empty()))
    );
}

#[test]
fn pi_model_tool_call_and_camel_case_flag() {
    let (s, es) = events(Harness::Pi, include_str!("../fixtures/pi.jsonl"));
    assert_eq!(s.model.as_deref(), Some("pi-model"));
    assert_eq!(es.iter().filter(|e| e.kind == "tool_call").count(), 1);
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "flag")
            .count(),
        1
    );
    assert_eq!(es.iter().filter(|e| e.ok.is_none()).count(), 5);
    let r = adapters::parse(
        Harness::Pi,
        &serde_json::json!({"type":"message","id":"numeric-system","message":{"role":"system","timestamp":1234,"content":"synthetic system"}}),
        &mut State::default(),
    );
    assert_eq!(r.events[0].kind, "message");
    assert_eq!(r.events[0].role, "system");
    assert_eq!(r.events[0].ts.as_deref(), Some("1970-01-01T00:00:01.234Z"));
}

#[test]
fn inventory_variants_are_classified_with_future_shape_negative_control() {
    let variants: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/inventory.json")).unwrap();
    let mut known = 0;
    let mut unknown = 0;
    let mut mismatches = Vec::new();
    for variant in variants.as_array().unwrap() {
        let r = adapters::parse(
            Harness::from_str(variant["harness"].as_str().unwrap(), false).unwrap(),
            &variant["record"],
            &mut State::default(),
        );
        if r.known != variant["known"].as_bool().unwrap() {
            mismatches.push(format!("{} {}", variant["harness"], variant["shape"]));
        }
        if variant["scope"] == "block" {
            let shape = variant["shape"].as_str().unwrap();
            let kind = if shape.ends_with("server_tool_use") {
                Some("server_tool_call")
            } else if shape.ends_with("advisor_tool_result") {
                Some("server_tool_result")
            } else if shape.ends_with("tool_use") || shape.ends_with("toolCall") {
                Some("tool_call")
            } else if shape.ends_with("tool_result") {
                Some("tool_result")
            } else {
                Some("message")
            };
            if let Some(kind) = kind {
                assert_eq!(
                    r.events.iter().filter(|e| e.kind == kind).count(),
                    1,
                    "{shape}"
                );
            }
            if shape.ends_with("/ text") {
                assert!(
                    r.events
                        .iter()
                        .any(|e| e.kind == "message" && e.text.as_deref() == Some("synthetic-text")),
                    "{shape}"
                );
            }
        }
        known += usize::from(r.known);
        unknown += usize::from(!r.known);
    }
    assert_eq!(known, 79, "{mismatches:?}");
    assert_eq!(unknown, 1);
    assert!(mismatches.is_empty(), "{mismatches:?}");
    let future =
        serde_json::json!({"type":"unknown-future-shape","message":{"content":"negative control"}});
    for &h in Harness::value_variants() {
        assert!(!adapters::parse(h, &future, &mut State::default()).known);
    }
}

#[test]
fn raw_shape_counts_survive_redaction_and_server_tools_keep_their_own_kind() {
    let (_, store, _) = indexed(Harness::Claude, include_str!("../fixtures/structure.jsonl"));
    let n: i64 = store
        .db
        .query_row(
            "SELECT coalesce(sum(n),0) FROM shapes WHERE signature LIKE 'assistant/%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 3);
    let (_, store, _) = indexed(
        Harness::Codex,
        "{\"type\":\"inter_agent_communication_metadata\",\"payload\":{}}\n",
    );
    let n:i64=store.db.query_row("SELECT coalesce(sum(n),0) FROM shapes WHERE signature LIKE 'inter_agent_communication_metadata/%'",[],|r|r.get(0)).unwrap();
    assert_eq!(n, 1);
    let variants: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/inventory.json")).unwrap();
    let es: Vec<_> = variants
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| {
            v["shape"] == "assistant / server_tool_use"
                || v["shape"] == "assistant / advisor_tool_result"
        })
        .flat_map(|v| adapters::parse(Harness::Claude, &v["record"], &mut State::default()).events)
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
fn opaque_blocks_are_not_materialized_and_normalized_counts_are_preserved() {
    let raw = include_bytes!("../fixtures/opaque.jsonl");
    let original: serde_json::Value = serde_json::from_slice(raw).unwrap();
    let filtered = sessidx::normalize::record_for_index(raw, Harness::Pi).unwrap();
    let payload_fields: usize = filtered["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| matches!(b["type"].as_str(), Some("thinking" | "image" | "fallback")))
        .map(|b| b.as_object().unwrap().len() - 1)
        .sum();
    assert_eq!(payload_fields, 0);
    let original = adapters::parse(Harness::Pi, &original, &mut State::default());
    let filtered = adapters::parse(Harness::Pi, &filtered, &mut State::default());
    let snapshot = |r: sessidx::model::Record| {
        r.events
            .into_iter()
            .map(|e| (e.kind, e.role, e.text, e.command, e.sites.len()))
            .collect::<Vec<_>>()
    };
    assert_eq!(snapshot(original), snapshot(filtered));
    let scalar = b"true";
    assert_eq!(
        sessidx::normalize::record_for_index(scalar, Harness::Pi).unwrap(),
        serde_json::json!(true)
    );
    assert_eq!(
        sessidx::normalize::record_for_index(br#"{"message":"plain"}"#, Harness::Claude).unwrap(),
        serde_json::json!({"message":"plain"})
    );
    assert!(
        sessidx::normalize::record_for_index(
            b"{\"message\":{\"content\":[{\"type\":\"thinking\",\"thinkingSignature\":invalid}]}}",
            Harness::Pi
        )
        .is_err()
    );
    // Real thinkingSignature records reach 16 MiB; only records above the
    // limit are skipped as parse errors.
    let record = |id: &str, len: usize| {
        let head = format!(
            "{{\"type\":\"message\",\"id\":\"{id}\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"thinking\",\"thinkingSignature\":\""
        );
        let tail = "\"}]}}\n";
        format!("{head}{}{tail}", "a".repeat(len - head.len() - tail.len()))
    };
    let dir = tempfile::tempdir().unwrap();
    let large = record("large", 2 << 20) + &record("oversized", sessidx::store::MAX_RECORD + 1);
    fs::write(dir.path().join("large.jsonl"), large).unwrap();
    let roots = [Root {
        harness: Harness::Pi,
        path: dir.path().into(),
    }];
    let r = Store::open(&dir.path().join("index.db"))
        .unwrap()
        .refresh(&roots, false, None)
        .unwrap();
    assert_eq!((r.records, r.parse_errors), (2, 1));
}

#[test]
fn codex_parent_fields_follow_their_precedence() {
    let spawn =
        serde_json::json!({"subagent":{"thread_spawn":{"parent_thread_id":"spawn-parent"}}});
    let history = serde_json::json!({"thread_id":"history-parent"});
    let mut failures = Vec::new();
    for (payload, expected) in [
        (
            serde_json::json!({"parent_thread_id":"top-parent","forked_from_id":"fork-parent","source":spawn.clone(),"history_base":history.clone()}),
            "top-parent",
        ),
        (
            serde_json::json!({"forked_from_id":"fork-parent","source":spawn.clone(),"history_base":history.clone()}),
            "fork-parent",
        ),
        (
            serde_json::json!({"forked_from_id":"fork-parent","history_base":history.clone()}),
            "fork-parent",
        ),
        (
            serde_json::json!({"source":spawn,"history_base":history}),
            "spawn-parent",
        ),
    ] {
        let mut s = State::default();
        adapters::parse(
            Harness::Codex,
            &serde_json::json!({"type":"session_meta","payload":payload}),
            &mut s,
        );
        if s.parent_id.as_deref() != Some(expected) {
            failures.push(format!("{payload} -> {:?}", s.parent_id));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
