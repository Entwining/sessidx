use crate::{
    model::{Agent, Compaction, Event, EventKind, Request, Route, SessionKind, State, Usage},
    normalize::{arguments, hash, label, native_id, shell, text, timestamp},
    redaction::{Label, Redacted},
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Vec<Event> {
    let p = &v["payload"];
    match v["type"].as_str().unwrap_or("") {
        "session_meta" => {
            session_meta(p, s);
            Vec::new()
        }
        "turn_context" => {
            // Codex models are turn-scoped: tests/fixtures/codex.jsonl.
            if let Some(m) = label(p, "model") {
                s.model = Some(m);
                s.model_source = "turn_context.model".into();
            }
            Vec::new()
        }
        "response_item" => response_item(v, p, s),
        // Codex request usage: tests/fixtures/usage-codex.jsonl.
        "token_usage_record" => {
            s.codex_usage_records = true;
            vec![usage_record(v, p, s)]
        }
        // A running total that moved is a request, until the rollout writes a
        // record per request. Rule: docs/formats.md `codex_token_count_request`.
        "event_msg" if p["type"] == "token_count" && !s.codex_usage_records => {
            let mut e = Event::context(v);
            let info = &p["info"];
            let total = info
                .pointer("/total_token_usage/total_tokens")
                .and_then(Value::as_i64);
            if total.is_some() && total != s.codex_total_tokens {
                s.codex_total_tokens = total;
                e.request = request(&info["last_token_usage"], s);
            }
            vec![e]
        }
        // A window ID that repeats in another rollout is a copy. The usage
        // record it embeds is one more record of its response. Rule:
        // docs/formats.md `codex_compaction_request`.
        "compacted" => {
            let mut e = Event::context(v);
            e.native_id = native_id(p, "window_id");
            e.compaction = Some(Compaction {
                trigger: "unknown",
                pre_tokens: None,
                post_tokens: None,
                summary: None,
                request_id: native_id(p, "compaction_response_id"),
            });
            let mut events = vec![e];
            if let Some(record) = p.get("latest_token_usage_record").filter(|r| r.is_object()) {
                events.push(usage_record(v, record, s));
            }
            events
        }
        _ => Vec::new(),
    }
}

fn session_meta(p: &Value, s: &mut State) {
    if !s.identified {
        if let Some(id) = label(p, "id") {
            s.session_id = id;
        }
        if let Some(cwd) = label(p, "cwd") {
            s.cwd = cwd;
        }
        s.codex_provider = label(p, "model_provider");
        s.parent_id = label(p, "parent_thread_id")
            .or_else(|| label(p, "forked_from_id"))
            .or_else(|| {
                p.pointer("/source/subagent/thread_spawn/parent_thread_id")
                    .and_then(Value::as_str)
                    .map(Label::new)
            })
            .or_else(|| {
                p.pointer("/history_base/thread_id")
                    .and_then(Value::as_str)
                    .map(Label::new)
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
}

fn response_item(v: &Value, p: &Value, s: &mut State) -> Vec<Event> {
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
            e.tool = label(p, "name")
                .or_else(|| (typ == "tool_search_call").then(|| Label::new("tool_search")));
            e.call_id = native_id(p, "call_id");
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
            e.call_id = native_id(p, "call_id");
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
        _ => return Vec::new(),
    };
    e.ts = timestamp(v);
    e.native_id = native_id(p, "id").or_else(|| e.call_id.clone());
    vec![e]
}

/// The event of a `token_usage_record` payload, wherever it is written.
fn usage_record(v: &Value, record: &Value, s: &State) -> Event {
    let mut e = Event::context(v);
    e.native_id = native_id(record, "response_id");
    e.request = request(&record["usage"], s);
    e
}

/// A request with the tokens a Codex usage object states, sent to the
/// provider of the rollout's header.
fn request(u: &Value, s: &State) -> Option<Request> {
    Some(Request {
        finished: true,
        stop_reason: None,
        route: s.codex_provider.clone().map(|value| Route {
            value,
            source: "session_meta.model_provider",
        }),
        usage: usage(u)?,
        passes: Vec::new(),
    })
}

/// The tokens a Codex `usage` or `last_token_usage` object states, whose
/// `input_tokens` include the cached and cache-written ones. Rule:
/// docs/formats.md `usage_columns`.
fn usage(u: &Value) -> Option<Usage> {
    let n = |key: &str| u.get(key).and_then(Value::as_i64);
    let cache_read = n("cached_input_tokens");
    let cache_write = n("cache_write_input_tokens");
    Some(Usage {
        input: n("input_tokens")? - cache_read.unwrap_or(0) - cache_write.unwrap_or(0),
        output: n("output_tokens")?,
        cache_read,
        cache_write,
        cache_write_5m: None,
        cache_write_1h: None,
        reasoning: n("reasoning_output_tokens"),
    })
}
