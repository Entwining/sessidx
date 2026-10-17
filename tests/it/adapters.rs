use crate::common::events;
use sessidx::{
    adapters,
    discovery::Root,
    model::{Agent, EventKind, State},
    store::Store,
};
use sha2::Digest;
use std::fs;

#[test]
fn claude_blocks_flags_and_synthetic_model() {
    let (s, es) = events(Agent::Claude, include_str!("../fixtures/claude.jsonl"));
    assert_eq!(
        es.iter().filter(|e| e.kind == EventKind::ToolCall).count(),
        1
    );
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "flag")
            .count(),
        1
    );
    assert_eq!(s.model.as_deref(), Some("claude-sonnet"));
    assert_eq!(es.iter().filter(|e| &*e.role == "user").count(), 1);
    assert!(
        es.iter()
            .filter(|e| e.kind == EventKind::ToolResult)
            .all(|e| e.text.as_ref().is_some_and(|s| !s.is_empty()))
    );
}

#[test]
fn codex_context_arguments_and_telemetry() {
    let (mut s, es) = events(Agent::Codex, include_str!("../fixtures/codex.jsonl"));
    adapters::parse(
        Agent::Codex,
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
    assert_eq!(
        es.iter().filter(|e| e.kind == EventKind::Message).count(),
        1
    );
    assert_eq!(
        s.instruction_hash.as_deref(),
        Some(hex::encode(sha2::Sha256::digest("Synthetic instruction")).as_str())
    );
    let mut history = State::default();
    adapters::parse(
        Agent::Codex,
        &serde_json::json!({"type":"session_meta","payload":{"history_base":{"thread_id":"history-parent"}}}),
        &mut history,
    );
    assert_eq!(history.parent_id.as_deref(), Some("history-parent"));
    let search = adapters::parse(
        Agent::Codex,
        &serde_json::json!({"type":"response_item","payload":{"type":"tool_search_call","call_id":"search"}}),
        &mut history,
    );
    assert_eq!(search[0].tool.as_deref(), Some("tool_search"));
    let found = adapters::parse(
        Agent::Codex,
        &serde_json::json!({"type":"response_item","payload":{"type":"tool_search_output","call_id":"search","tools":[]}}),
        &mut history,
    );
    assert_eq!(found[0].kind, EventKind::ToolResult);
    let sent = adapters::parse(
        Agent::Codex,
        &serde_json::json!({"type":"response_item","payload":{"type":"agent_message","id":"synthetic-id","recipient":"synthetic-name","role":"assistant","content":[{"type":"input_text","text":"synthetic-text"}]}}),
        &mut history,
    );
    assert_eq!(
        (sent[0].kind, &*sent[0].role),
        (EventKind::Communication, "assistant")
    );
    assert_eq!(sent[0].text.as_deref(), Some("synthetic-text"));
    let array = adapters::parse(
        Agent::Codex,
        &serde_json::json!({"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":{"command":["printf", "literal quote's"]}}}),
        &mut history,
    );
    assert_eq!(array[0].sites[0].program.as_deref(), Some("printf"));
    assert_eq!(
        array[0].sites[0]
            .argv
            .iter()
            .map(|a| &**a)
            .collect::<Vec<_>>(),
        ["'printf'", "'literal quote'\\''s'"]
    );
    assert!(
        es.iter()
            .filter(|e| e.kind == EventKind::ToolResult)
            .all(|e| e.text.as_ref().is_some_and(|s| !s.is_empty()))
    );
}

#[test]
fn pi_model_tool_call_and_camel_case_flag() {
    let (s, es) = events(Agent::Pi, include_str!("../fixtures/pi.jsonl"));
    assert_eq!(s.model.as_deref(), Some("pi-model"));
    assert_eq!(
        es.iter().filter(|e| e.kind == EventKind::ToolCall).count(),
        1
    );
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "flag")
            .count(),
        1
    );
    assert_eq!(es.iter().filter(|e| e.ok.is_none()).count(), 5);
    let system = adapters::parse(
        Agent::Pi,
        &serde_json::json!({"type":"message","id":"numeric-system","message":{"role":"system","timestamp":1234,"content":"synthetic system"}}),
        &mut State::default(),
    );
    assert_eq!(system[0].kind, EventKind::Message);
    assert_eq!(&*system[0].role, "system");
    assert_eq!(system[0].ts.as_deref(), Some("1970-01-01T00:00:01.234Z"));
}

#[test]
fn records_no_rule_reads_keep_only_a_context_pointer() {
    // A record type an agent added, Codex telemetry, and a command the user ran in Pi.
    for (agent, record) in [
        (
            Agent::Claude,
            serde_json::json!({"type":"api-request-blob","hash":"synthetic-hash","sessionId":"synthetic-session","message":{"role":"user","content":"copied request text"}}),
        ),
        (
            Agent::Codex,
            serde_json::json!({"type":"event_msg","payload":{"type":"agent_reasoning","text":"telemetry text"}}),
        ),
        (
            Agent::Pi,
            serde_json::json!({"type":"message","id":"synthetic-id","message":{"role":"bashExecution","command":"ls","output":"synthetic-text"}}),
        ),
    ] {
        let events = adapters::parse(agent, &record, &mut State::default());
        assert_eq!(events.len(), 1, "{record}");
        assert_eq!(events[0].kind, EventKind::Context, "{record}");
        assert!(events[0].text.is_none(), "{record}");
        assert!(events[0].request.is_none(), "{record}");
        assert!(events[0].compaction.is_none(), "{record}");
        assert_eq!(
            events[0].native_id.is_some(),
            record["id"].is_string(),
            "{record}"
        );
    }
}

#[test]
fn content_blocks_become_their_event_kinds() {
    use EventKind::*;
    let assistant = |block: serde_json::Value| serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[block]}});
    let user = |block: serde_json::Value| serde_json::json!({"type":"user","message":{"role":"user","content":[block]}});
    let pi = |role: &str, block: serde_json::Value| serde_json::json!({"type":"message","message":{"role":role,"content":[block]}});
    let call = serde_json::json!({"id":"synthetic-call","name":"Bash","input":{"command":"rg needle src"}});
    let with_type = |typ: &str, mut v: serde_json::Value| {
        v["type"] = typ.into();
        v
    };
    let mut failures = Vec::new();
    for (agent, record, expected) in [
        (
            Agent::Claude,
            assistant(serde_json::json!({"type":"text","text":"synthetic-text"})),
            vec![Message],
        ),
        (
            Agent::Claude,
            assistant(serde_json::json!({"type":"thinking","thinking":"synthetic-text"})),
            vec![Message],
        ),
        (
            Agent::Claude,
            assistant(serde_json::json!({"type":"fallback"})),
            vec![Message],
        ),
        (
            Agent::Claude,
            assistant(with_type("tool_use", call.clone())),
            vec![Message, ToolCall],
        ),
        (
            Agent::Claude,
            assistant(with_type("server_tool_use", call.clone())),
            vec![Message, ServerToolCall],
        ),
        (
            Agent::Claude,
            assistant(serde_json::json!({"type":"advisor_tool_result","content":"synthetic-text"})),
            vec![Message, ServerToolResult],
        ),
        (
            Agent::Claude,
            user(serde_json::json!({"type":"text","text":"synthetic-text"})),
            vec![Message],
        ),
        (
            Agent::Claude,
            user(
                serde_json::json!({"type":"image","source":{"type":"synthetic-type","data":"synthetic-data"}}),
            ),
            vec![Message],
        ),
        (
            Agent::Claude,
            user(
                serde_json::json!({"type":"tool_result","tool_use_id":"synthetic-id","content":"synthetic-text"}),
            ),
            vec![ToolResult],
        ),
        (
            Agent::Pi,
            pi(
                "user",
                serde_json::json!({"type":"text","text":"synthetic-text"}),
            ),
            vec![Message],
        ),
        (
            Agent::Pi,
            pi(
                "user",
                serde_json::json!({"type":"image","data":"synthetic-data"}),
            ),
            vec![Message],
        ),
        (
            Agent::Pi,
            pi(
                "assistant",
                serde_json::json!({"type":"thinking","thinking":"synthetic-text"}),
            ),
            vec![Message],
        ),
        (
            Agent::Pi,
            pi(
                "assistant",
                serde_json::json!({"type":"toolCall","id":"synthetic-call","name":"bash","arguments":{"command":"rg needle src"}}),
            ),
            vec![Message, ToolCall],
        ),
    ] {
        let events = adapters::parse(agent, &record, &mut State::default());
        let kinds: Vec<_> = events.iter().map(|e| e.kind).collect();
        let text_kept = record.pointer("/message/content/0/type") != Some(&"text".into())
            || events
                .iter()
                .any(|e| e.kind == Message && e.text.as_deref() == Some("synthetic-text"));
        if kinds != expected || !text_kept {
            failures.push(format!("{record} -> {kinds:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn opaque_blocks_are_not_materialized_and_normalized_counts_are_preserved() {
    let raw = include_bytes!("../fixtures/opaque.jsonl");
    let original: serde_json::Value = serde_json::from_slice(raw).unwrap();
    let filtered = sessidx::normalize::record_for_index(raw, Agent::Pi).unwrap();
    let payload_fields: usize = filtered["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| matches!(b["type"].as_str(), Some("thinking" | "image" | "fallback")))
        .map(|b| b.as_object().unwrap().len() - 1)
        .sum();
    assert_eq!(payload_fields, 0);
    let original = adapters::parse(Agent::Pi, &original, &mut State::default());
    let filtered = adapters::parse(Agent::Pi, &filtered, &mut State::default());
    let snapshot = |events: Vec<sessidx::model::Event>| {
        events
            .into_iter()
            .map(|e| (e.kind, e.role, e.text, e.command, e.sites.len()))
            .collect::<Vec<_>>()
    };
    assert_eq!(snapshot(original), snapshot(filtered));
    let scalar = b"true";
    assert_eq!(
        sessidx::normalize::record_for_index(scalar, Agent::Pi).unwrap(),
        serde_json::json!(true)
    );
    assert_eq!(
        sessidx::normalize::record_for_index(br#"{"message":"plain"}"#, Agent::Claude).unwrap(),
        serde_json::json!({"message":"plain"})
    );
    assert!(
        sessidx::normalize::record_for_index(
            b"{\"message\":{\"content\":[{\"type\":\"thinking\",\"thinkingSignature\":invalid}]}}",
            Agent::Pi
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
        agent: Agent::Pi,
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
            Some("top-parent"),
        ),
        (
            serde_json::json!({"forked_from_id":"fork-parent","source":spawn.clone(),"history_base":history.clone()}),
            Some("fork-parent"),
        ),
        (
            serde_json::json!({"forked_from_id":"fork-parent","history_base":history.clone()}),
            Some("fork-parent"),
        ),
        (
            serde_json::json!({"source":spawn,"history_base":history}),
            Some("spawn-parent"),
        ),
        (
            serde_json::json!({"id":"resumed","history_base":{"thread_id":"resumed"}}),
            None,
        ),
    ] {
        let mut s = State::default();
        adapters::parse(
            Agent::Codex,
            &serde_json::json!({"type":"session_meta","payload":payload}),
            &mut s,
        );
        if s.parent_id.as_deref() != expected {
            failures.push(format!("{payload} -> {:?}", s.parent_id));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
