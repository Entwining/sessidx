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
            s.parent_id = string(p, "parent_thread_id")
                .or_else(|| string(p, "forked_from_id"))
                .or_else(|| {
                    p.pointer("/source/subagent/thread_spawn/parent_thread_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .or_else(|| {
                    p.pointer("/history_base/thread_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            if p.pointer("/source/subagent").is_some() {
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
                    e.kind_source = "payload.type".into();
                    e.text = Some(text(&p["content"]));
                    if s.instruction_hash.is_none()
                        && role == "user"
                        && e.text
                            .as_deref()
                            .is_some_and(|t| t.starts_with("# AGENTS.md instructions"))
                    {
                        s.instruction_hash = e.text.as_deref().map(|t| hash(t.as_bytes()));
                    }
                    e
                }
                "function_call" | "custom_tool_call" | "tool_search_call" => {
                    let mut e = Event::new("assistant", "tool_call", "payload.type");
                    e.tool = string(p, "name")
                        .or_else(|| (typ == "tool_search_call").then(|| "tool_search".into()));
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
                "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
                    let mut e = Event::new("tool", "tool_result", "payload.type");
                    e.call_id = string(p, "call_id");
                    // Codex text and structured outcomes: testdata/outcomes.jsonl.
                    crate::outcomes::classify(&mut e, &p["output"], "codex");
                    e
                }
                "agent_message" => {
                    let mut e = Event::new(
                        p["role"].as_str().unwrap_or("unknown"),
                        "communication",
                        "payload.type",
                    );
                    e.text = Some(text(&p["content"]));
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
        "event_msg"
        | "compacted"
        | "inter_agent_communication_metadata"
        | "token_usage_record"
        | "world_state" => Record {
            events: vec![],
            known: true,
        },
        _ => Record {
            events: vec![],
            known: false,
        },
    }
}
