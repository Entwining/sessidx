use crate::{
    model::{Agent, Event, EventKind, Record, SessionKind, State},
    normalize::{arguments, hash, shell, string, text, timestamp},
    redaction::Redacted,
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Record {
    let p = &v["payload"];
    match v["type"].as_str().unwrap_or("") {
        "session_meta" => {
            if !s.identified {
                if let Some(id) = string(p, "id") {
                    s.session_id = id;
                }
                if let Some(cwd) = string(p, "cwd") {
                    s.cwd = cwd;
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
                    })
                    // A thread resumed from its own history names itself.
                    .filter(|parent| *parent != s.session_id);
                // A client name (cli, exec, vscode) starts a thread; a
                // subagent object spawns one from another thread.
                s.session_kind = match p.get("source") {
                    Some(Value::String(_)) => SessionKind::Main,
                    Some(source) if source.get("subagent").is_some() => SessionKind::Subagent,
                    _ => SessionKind::Unknown,
                };
                s.session_kind_source = "session_meta.source".into();
                s.identified = true;
            }
            if let Some(base) = p.get("base_instructions") {
                let content = text(base);
                if !content.is_empty() {
                    s.instruction_hash = Some(hash(content.as_bytes()));
                }
            }
            Record {
                events: vec![],
                known: true,
            }
        }
        "turn_context" => {
            // Codex models are turn-scoped: tests/fixtures/codex.jsonl.
            if let Some(m) = string(p, "model") {
                s.model = Some(m);
                s.model_source = "turn_context.model".into();
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
                    let mut e = Event::new(role, EventKind::Message, "payload.role");
                    e.kind_source = "payload.type";
                    let content = text(&p["content"]);
                    if s.instruction_hash.is_none()
                        && role == "user"
                        && content.starts_with("# AGENTS.md instructions")
                    {
                        s.instruction_hash = Some(hash(content.as_bytes()));
                    }
                    e.text = Some(Redacted::new(&content));
                    e
                }
                "function_call" | "custom_tool_call" | "tool_search_call" => {
                    let mut e = Event::new("assistant", EventKind::ToolCall, "payload.type");
                    e.tool = string(p, "name")
                        .or_else(|| (typ == "tool_search_call").then(|| "tool_search".into()));
                    e.call_id = string(p, "call_id");
                    let args = arguments(
                        p.get("arguments")
                            .or_else(|| p.get("input"))
                            .unwrap_or(&Value::Null),
                    );
                    e.command = e.tool.as_deref().and_then(|t| shell(t, &args));
                    if matches!(e.tool.as_deref(), Some("exec" | "functions.exec")) {
                        e.sites = args
                            .as_str()
                            .map(crate::code_mode::sites)
                            .unwrap_or_else(|| {
                                vec![crate::shell::Site {
                                    program: None,
                                    argv: Vec::new(),
                                    parsed: false,
                                }]
                            });
                    }
                    e.text = Some(Redacted::new(&args.to_string()));
                    e
                }
                "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
                    let mut e = Event::new("tool", EventKind::ToolResult, "payload.type");
                    e.call_id = string(p, "call_id");
                    // Codex text and structured outcomes: tests/fixtures/outcomes.jsonl.
                    crate::outcomes::classify(&mut e, &p["output"], Agent::Codex);
                    crate::normalize::output_prefix(&mut e, &p["output"]);
                    e
                }
                "agent_message" => {
                    let mut e = Event::new(
                        p["role"].as_str().unwrap_or("unknown"),
                        EventKind::Communication,
                        "payload.type",
                    );
                    e.text = Some(Redacted::new(&text(&p["content"])));
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
            e.native_id = string(p, "id").or_else(|| {
                e.call_id
                    .as_ref()
                    .map(|id| format!("{id}:{}", e.kind.as_str()))
            });
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
