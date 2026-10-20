use crate::{
    model::{
        Agent, Compaction, Event, EventKind, Request, RequestError, Route, SessionKind, State,
        Usage,
    },
    normalize::{
        Digest, arguments, hash, label, message_text, native_id, shell, string, text, timestamp,
    },
    redaction::Redacted,
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Vec<Event> {
    match v["type"].as_str().unwrap_or("") {
        "session" => {
            if !s.identified {
                if let Some(id) = label(v, "id") {
                    s.session_id = id;
                }
                if let Some(cwd) = label(v, "cwd") {
                    s.cwd = cwd;
                }
                s.parent_id = label(v, "parentSession");
                // Pi ships without subagents, and a fork is a session a user
                // started; a package that adds subagents could write ordinary
                // sessions this rule cannot tell apart.
                s.session_kind = SessionKind::Main;
                s.session_kind_source = "session".into();
                s.identified = true;
            }
            Vec::new()
        }
        "model_change" => {
            // Pi model changes precede messages: tests/fixtures/pi.jsonl.
            s.model = label(v, "modelId");
            s.model_source = "model_change.modelId".into();
            Vec::new()
        }
        "message" => message(v, s),
        "compaction" => {
            let mut e = Event::context(v);
            e.compaction = Some(Compaction {
                trigger: "unknown",
                pre_tokens: v["tokensBefore"].as_i64(),
                post_tokens: None,
                summary: usage(&v["usage"]),
                request_id: None,
            });
            vec![e]
        }
        // A `usage` entry is a cache warm-up's, not a request's: docs/formats.md
        // `pi_request_usage`.
        _ => Vec::new(),
    }
}

fn message(v: &Value, s: &mut State) -> Vec<Event> {
    let m = &v["message"];
    let role = m["role"].as_str().unwrap_or("unknown");
    let model = label(m, "model");
    if let Some(model) = &model {
        s.model = Some(model.clone());
        s.model_source = "message.model".into();
    }
    let ts = timestamp(v).or_else(|| timestamp(m));
    if role == "toolResult" {
        return vec![tool_result(v, m, s, ts)];
    }
    let mut events = Vec::new();
    // Pi empty replies: tests/it/messages.rs::pi_empty_response_and_explicit_message_model_override.
    if role == "assistant" || role == "user" || role == "system" {
        let mut e = Event::new(role, EventKind::Message, "message.role");
        let mut body = message_text(&m["content"]);
        // Pi system sections: tests/fixtures/pi-sections.jsonl.
        if let Some(sections) = m["sections"].as_object() {
            for section in sections.values().map(text).filter(|s| !s.is_empty()) {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(&section);
            }
        }
        // Pi request failures: tests/fixtures/usage-pi.jsonl.
        if let Some(error) = string(m, "errorMessage") {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&error);
        }
        e.text = Some(Redacted::new(&body));
        e.native_id = native_id(v, "id");
        e.ts = ts.clone();
        e.model = model.clone();
        // Pi request usage: tests/fixtures/usage-pi.jsonl.
        if role == "assistant" {
            let route = label(m, "provider").map(|value| Route {
                value,
                source: "message.provider",
            });
            let stop_reason = label(m, "stopReason");
            if stop_reason.as_deref() == Some("error") {
                e.error = Some(RequestError {
                    source: "stop_reason",
                    status: None,
                    code: None,
                    retry_attempt: None,
                    route: route.clone(),
                });
            }
            e.request = usage(&m["usage"]).map(|usage| Request {
                finished: true,
                stop_reason,
                route,
                usage,
                passes: Vec::new(),
            });
        }
        events.push(e);
    }
    if let Some(blocks) = m["content"].as_array() {
        let record = native_id(v, "id");
        for (i, b) in blocks.iter().enumerate() {
            let mut e = match b["type"].as_str().unwrap_or("") {
                "toolCall" => tool_call(b, record.as_ref(), s),
                _ => continue,
            };
            e.ts = ts.clone();
            // Pi call IDs are too short to identify a call across
            // sessions. Rule: docs/formats.md `native_call_identity`.
            e.native_id = string(v, "id").map(|id| hash(format!("{id}:{i}").as_bytes()));
            e.model = model.clone();
            events.push(e);
        }
    }
    events
}

fn tool_result(v: &Value, m: &Value, s: &State, ts: Option<String>) -> Event {
    let mut e = Event::new("tool", EventKind::ToolResult, "message.role");
    e.call_id = s
        .pi_tool_record
        .as_ref()
        .zip(string(m, "toolCallId"))
        .map(|(record, id)| call_id(record, &id));
    e.tool = label(m, "toolName");
    e.ok = m.get("isError").and_then(Value::as_bool).map(|v| !v);
    if e.ok.is_some() {
        e.ok_source = "flag";
    }
    crate::outcomes::classify(&mut e, &m["content"], Agent::Pi);
    crate::normalize::output_prefix(&mut e, &m["content"]);
    e.native_id = native_id(v, "id");
    e.ts = ts;
    e
}

fn tool_call(b: &Value, record: Option<&Digest>, s: &mut State) -> Event {
    let mut e = Event::new("assistant", EventKind::ToolCall, "content.toolCall");
    e.call_id = record
        .zip(string(b, "id"))
        .map(|(record, id)| call_id(record, &id));
    s.pi_tool_record = record.cloned();
    e.tool = label(b, "name");
    let args = arguments(&b["arguments"]);
    e.command = e.tool.as_deref().and_then(|t| shell(t, &args));
    e.text = Some(Redacted::new(&args.to_string()));
    e
}

/// Pi call IDs repeat across the entries of one session, so a call ID is
/// scoped by the record of the entry that made the call. Rule:
/// docs/formats.md `native_call_identity`.
fn call_id(record: &Digest, id: &str) -> Digest {
    hash(format!("{}:{id}", &**record).as_bytes())
}

fn usage(u: &Value) -> Option<Usage> {
    let n = |key: &str| u.get(key).and_then(Value::as_i64);
    Some(Usage {
        input: n("input")?,
        output: n("output")?,
        cache_read: n("cacheRead"),
        cache_write: n("cacheWrite"),
        cache_write_5m: None,
        cache_write_1h: n("cacheWrite1h"),
        reasoning: n("reasoning"),
    })
}
