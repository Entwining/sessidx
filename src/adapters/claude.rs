use crate::{
    model::{Event, Record, State},
    normalize::{arguments, shell, string, text, timestamp},
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Record {
    if let Some(id) = string(v, "sessionId") {
        s.session_id = id;
    }
    if let Some(cwd) = string(v, "cwd") {
        s.cwd = cwd;
    }
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        s.kind = "delegated".into();
        s.kind_source = "isSidechain".into();
    }
    let typ = v.get("type").and_then(Value::as_str).unwrap_or("");
    if !matches!(typ, "user" | "assistant") {
        return Record {
            events: Vec::new(),
            known: matches!(
                typ,
                "summary"
                    | "system"
                    | "progress"
                    | "file-history-snapshot"
                    | "queue-operation"
                    | "last-prompt"
                    | "custom-title"
            ),
        };
    }
    let m = &v["message"];
    // Claude model fields and synthetic errors: testdata/claude.jsonl.
    if let Some(model) = string(m, "model").filter(|m| m != "<synthetic>") {
        s.model = Some(model);
        s.model_source = "message.model".into();
    }
    let role = m.get("role").and_then(Value::as_str).unwrap_or(typ);
    let content = &m["content"];
    let mut events = Vec::new();
    if let Some(blocks) = content.as_array() {
        for (i, b) in blocks.iter().enumerate() {
            let bt = b["type"].as_str().unwrap_or("");
            let mut e = match bt {
                "text" => {
                    let mut e = Event::new(role, "message", "message.role");
                    e.text = Some(text(b));
                    e
                }
                "tool_use" => {
                    let mut e = Event::new("assistant", "tool_call", "content.tool_use");
                    e.tool = string(b, "name");
                    e.call_id = string(b, "id");
                    let args = arguments(&b["input"]);
                    e.command = e.tool.as_deref().and_then(|t| shell(t, &args));
                    e.text = Some(args.to_string());
                    e
                }
                "tool_result" => {
                    let mut e = Event::new("tool", "tool_result", "content.tool_result");
                    e.call_id = string(b, "tool_use_id");
                    e.ok = b.get("is_error").and_then(Value::as_bool).map(|v| !v);
                    if e.ok.is_some() {
                        e.ok_source = "flag".into();
                    }
                    e
                }
                "thinking" | "redacted_thinking" | "image" => continue,
                _ => continue,
            };
            e.native_id = string(v, "uuid").map(|id| format!("{id}:{i}"));
            e.ts = timestamp(v);
            events.push(e);
        }
    } else if content.is_string() {
        let mut e = Event::new(role, "message", "message.role");
        e.text = Some(text(content));
        e.native_id = string(v, "uuid");
        e.ts = timestamp(v);
        events.push(e);
    }
    Record {
        events,
        known: true,
    }
}
