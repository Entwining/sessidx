use crate::{
    model::{Event, Record, State},
    normalize::{arguments, hash, shell, string, text, timestamp},
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Record {
    let p = &v["payload"];
    match v["type"].as_str().unwrap_or("") {
        "session_meta" => {
            if let Some(id) = string(p, "id") {
                s.session_id = id;
            }
            if let Some(cwd) = string(p, "cwd") {
                s.cwd = cwd;
            }
            if let Some(base) = p.get("base_instructions") {
                let content = text(base);
                if !content.is_empty() {
                    s.instruction_hash = Some(hash(content.as_bytes()));
                }
            }
            s.parent_id = string(p, "parent_thread_id").or_else(|| string(p, "forked_from_id"));
            if s.parent_id.is_some() || p.pointer("/source/subagent").is_some() {
                s.kind = "delegated".into();
                s.kind_source = "session_meta.ancestry".into();
            }
            Record {
                events: vec![],
                known: true,
            }
        }
        "turn_context" => {
            // Codex models are turn-scoped: testdata/codex.jsonl.
            if let Some(m) = string(p, "model") {
                s.model = Some(m);
                s.model_source = "turn_context.model".into();
            }
            if let Some(cwd) = string(p, "cwd") {
                s.cwd = cwd;
            }
            Record {
                events: vec![],
                known: true,
            }
        }
        "response_item" => {
            let typ = p["type"].as_str().unwrap_or("");
            let mut e = match typ {
                "message" => {
                    let role = p["role"].as_str().unwrap_or("unknown");
                    let mut e = Event::new(role, "message", "payload.role");
                    e.text = Some(text(&p["content"]));
                    if role == "user"
                        && e.text
                            .as_deref()
                            .is_some_and(|t| t.starts_with("# AGENTS.md instructions"))
                    {
                        s.instruction_hash = e.text.as_deref().map(|t| hash(t.as_bytes()));
                    }
                    e
                }
                "function_call" | "custom_tool_call" => {
                    let mut e = Event::new("assistant", "tool_call", "payload.type");
                    e.tool = string(p, "name");
                    e.call_id = string(p, "call_id");
                    let args = arguments(
                        p.get("arguments")
                            .or_else(|| p.get("input"))
                            .unwrap_or(&Value::Null),
                    );
                    e.command = e.tool.as_deref().and_then(|t| shell(t, &args));
                    e.text = Some(args.to_string());
                    e
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let mut e = Event::new("tool", "tool_result", "payload.type");
                    e.call_id = string(p, "call_id");
                    e
                }
                "reasoning" | "compaction" | "web_search_call" => {
                    return Record {
                        events: vec![],
                        known: true,
                    };
                }
                _ => {
                    return Record {
                        events: vec![],
                        known: false,
                    };
                }
            };
            e.ts = timestamp(v);
            e.native_id =
                string(p, "id").or_else(|| e.call_id.as_ref().map(|id| format!("{id}:{}", e.kind)));
            Record {
                events: vec![e],
                known: true,
            }
        }
        "event_msg" | "compacted" => Record {
            events: vec![],
            known: true,
        },
        _ => Record {
            events: vec![],
            known: false,
        },
    }
}
