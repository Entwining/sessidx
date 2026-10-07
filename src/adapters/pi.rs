use crate::{
    model::{Event, Record, State},
    normalize::{arguments, message_text, shell, string, text, timestamp},
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Record {
    match v["type"].as_str().unwrap_or("") {
        "session" => {
            if let Some(id) = string(v, "id") {
                s.session_id = id;
            }
            if let Some(cwd) = string(v, "cwd") {
                s.cwd = cwd;
            }
            s.parent_id = string(v, "parentSession");
            Record {
                events: vec![],
                known: true,
            }
        }
        "model_change" => {
            // Pi model changes precede messages: testdata/pi.jsonl.
            s.model = string(v, "modelId");
            s.model_source = "model_change.modelId".into();
            Record {
                events: vec![],
                known: true,
            }
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
                let mut e = Event::new("tool", "tool_result", "message.role");
                e.call_id = string(m, "toolCallId");
                e.tool = string(m, "toolName");
                e.ok = m.get("isError").and_then(Value::as_bool).map(|v| !v);
                if e.ok.is_some() {
                    e.ok_source = "flag".into();
                }
                crate::outcomes::classify(&mut e, &m["content"], "pi");
                crate::normalize::output_prefix(&mut e, &m["content"]);
                e.native_id = string(v, "id");
                e.ts = ts;
                return Record {
                    events: vec![e],
                    known: true,
                };
            }
            let mut events = Vec::new();
            // Pi empty replies: tests/counting.rs::pi_empty_response_and_explicit_message_model_override.
            if role == "assistant" || role == "user" || role == "system" {
                let mut e = Event::new(role, "message", "message.role");
                e.text = Some(message_text(&m["content"]));
                // Pi system sections: testdata/pi-sections.jsonl.
                if let Some(sections) = m["sections"].as_object() {
                    let body = e.text.as_mut().unwrap();
                    for section in sections.values().map(text).filter(|s| !s.is_empty()) {
                        if !body.is_empty() {
                            body.push('\n');
                        }
                        body.push_str(&section);
                    }
                }
                e.native_id = string(v, "id");
                e.ts = ts.clone();
                e.model = string(m, "model");
                events.push(e);
            }
            if let Some(blocks) = m["content"].as_array() {
                for (i, b) in blocks.iter().enumerate() {
                    let mut e = match b["type"].as_str().unwrap_or("") {
                        "text" => continue,
                        "toolCall" => {
                            let mut e = Event::new("assistant", "tool_call", "content.toolCall");
                            e.call_id = string(b, "id");
                            e.tool = string(b, "name");
                            let args = arguments(&b["arguments"]);
                            e.command = e.tool.as_deref().and_then(|t| shell(t, &args));
                            e.text = Some(args.to_string());
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
            Record {
                events,
                known: matches!(role, "assistant" | "user" | "system" | "bashExecution"),
            }
        }
        "thinking_level_change"
        | "compaction"
        | "branch_summary"
        | "context_edit"
        | "session_info"
        | "custom_message"
        | "label" => Record {
            events: vec![],
            known: true,
        },
        _ => Record {
            events: vec![],
            known: false,
        },
    }
}
