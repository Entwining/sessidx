use crate::{
    model::{Agent, Compaction, Event, EventKind, Request, SessionKind, State, Usage},
    normalize::{arguments, message_text, shell, string, text, timestamp},
    redaction::Redacted,
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Vec<Event> {
    match v["type"].as_str().unwrap_or("") {
        "session" => {
            if !s.identified {
                if let Some(id) = string(v, "id") {
                    s.session_id = id;
                }
                if let Some(cwd) = string(v, "cwd") {
                    s.cwd = cwd;
                }
                s.parent_id = string(v, "parentSession");
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
            s.model = string(v, "modelId");
            s.model_source = "model_change.modelId".into();
            Vec::new()
        }
        "message" => {
            let m = &v["message"];
            let role = m["role"].as_str().unwrap_or("unknown");
            if let Some(model) = string(m, "model") {
                s.model = Some(model);
                s.model_source = "message.model".into();
            }
            let ts = timestamp(v).or_else(|| timestamp(m));
            if role == "toolResult" {
                let mut e = Event::new("tool", EventKind::ToolResult, "message.role");
                e.call_id = string(m, "toolCallId");
                e.tool = string(m, "toolName");
                e.ok = m.get("isError").and_then(Value::as_bool).map(|v| !v);
                if e.ok.is_some() {
                    e.ok_source = "flag";
                }
                crate::outcomes::classify(&mut e, &m["content"], Agent::Pi);
                crate::normalize::output_prefix(&mut e, &m["content"]);
                e.native_id = string(v, "id");
                e.ts = ts;
                return vec![e];
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
                e.text = Some(Redacted::new(&body));
                e.native_id = string(v, "id");
                e.ts = ts.clone();
                e.model = string(m, "model");
                // Pi request usage: tests/fixtures/usage-pi.jsonl.
                if role == "assistant" {
                    e.request = usage(&m["usage"]).map(|usage| Request {
                        finished: true,
                        usage,
                        passes: Vec::new(),
                    });
                }
                events.push(e);
            }
            if let Some(blocks) = m["content"].as_array() {
                for (i, b) in blocks.iter().enumerate() {
                    let mut e = match b["type"].as_str().unwrap_or("") {
                        "toolCall" => {
                            let mut e =
                                Event::new("assistant", EventKind::ToolCall, "content.toolCall");
                            e.call_id = string(b, "id");
                            e.tool = string(b, "name");
                            let args = arguments(&b["arguments"]);
                            e.command = e.tool.as_deref().and_then(|t| shell(t, &args));
                            e.text = Some(Redacted::new(&args.to_string()));
                            e
                        }
                        _ => continue,
                    };
                    e.ts = ts.clone();
                    e.native_id = e
                        .call_id
                        .as_ref()
                        .map(|id| format!("call:{id}"))
                        .or_else(|| string(v, "id").map(|id| format!("{id}:{i}")));
                    e.model = string(m, "model");
                    events.push(e);
                }
            }
            events
        }
        "compaction" => {
            let mut e = Event::context(v);
            e.compaction = Some(Compaction {
                trigger: "unknown",
                pre_tokens: v["tokensBefore"].as_i64(),
                post_tokens: None,
                summary: usage(&v["usage"]),
            });
            vec![e]
        }
        // A `usage` entry is a cache warm-up's, not a request's: docs/formats.md
        // `pi_request_usage`.
        _ => Vec::new(),
    }
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
