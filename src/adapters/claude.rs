use crate::{
    model::{
        Agent, Compaction, Denial, Event, EventKind, Pass, Request, RequestError, Route,
        SessionKind, State, Usage,
    },
    normalize::{arguments, label, message_text, native_id, shell, timestamp},
    redaction::{Label, Redacted},
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Vec<Event> {
    identify(v, s);
    let typ = v.get("type").and_then(Value::as_str).unwrap_or("");
    // Claude diagnostic attachments: tests/fixtures/output-prefix.json.
    if typ == "attachment"
        && v.pointer("/attachment/type").and_then(Value::as_str) == Some("diagnostics")
        && let Some(e) = diagnostics(v)
    {
        return vec![e];
    }
    // Claude compaction boundaries: tests/fixtures/usage-claude.jsonl.
    if typ == "system" && v["subtype"] == "compact_boundary" {
        let metadata = &v["compactMetadata"];
        let mut e = Event::context(v);
        e.compaction = Some(Compaction {
            trigger: match metadata["trigger"].as_str() {
                Some("auto") => "auto",
                Some("manual") => "manual",
                _ => "unknown",
            },
            pre_tokens: metadata["preTokens"].as_i64(),
            post_tokens: metadata["postTokens"].as_i64(),
            summary: None,
            request_id: None,
        });
        return vec![e];
    }
    // An attempt Claude Code retries: tests/fixtures/usage-claude.jsonl.
    if typ == "system" && v["subtype"] == "api_error" {
        let mut e = Event::context(v);
        e.error = Some(RequestError {
            source: "api_error",
            status: v.pointer("/error/status").and_then(Value::as_i64),
            code: v
                .pointer("/error/cause/code")
                .and_then(Value::as_str)
                .map(Label::new),
            retry_attempt: v["retryAttempt"].as_i64(),
            route: id_route(v.pointer("/error/requestId"), "error.requestId"),
        });
        return vec![e];
    }
    if !matches!(typ, "user" | "assistant") {
        return Vec::new();
    }
    let m = &v["message"];
    // Claude model fields and synthetic errors: tests/fixtures/claude.jsonl.
    let model = label(m, "model");
    if let Some(model) = model.as_ref().filter(|m| &***m != "<synthetic>") {
        s.model = Some(model.clone());
        s.model_source = "message.model".into();
    }
    let content = &m["content"];
    let mut events = Vec::new();
    // Claude fragments, empty replies, and compaction: tests/fixtures/structure.jsonl.
    let summary = v.get("isCompactSummary").and_then(Value::as_bool) == Some(true);
    if summary {
        return Vec::new();
    }
    let only_results = content
        .as_array()
        .is_some_and(|a| !a.is_empty() && a.iter().all(|b| b["type"] == "tool_result"));
    if typ == "assistant" || !only_results {
        events.push(message(v, typ, model.as_ref()));
    }
    if let Some(blocks) = content.as_array() {
        for b in blocks {
            let bt = b["type"].as_str().unwrap_or("");
            let mut e = match bt {
                "tool_use" | "server_tool_use" => tool_use(b, bt),
                "tool_result" | "advisor_tool_result" => tool_result(v, b, bt),
                _ => continue,
            };
            e.native_id = e.call_id.clone();
            e.model = model.clone();
            e.ts = timestamp(v);
            events.push(e);
        }
    }
    events
}

fn identify(v: &Value, s: &mut State) {
    // A subagent's records carry its agentId; a main transcript is named after
    // its session ID, which the indexer already holds. Until the first such
    // record, a resumed transcript holds history copied from another session.
    let agent_id = label(v, "agentId");
    let session_id = label(v, "sessionId");
    if !s.identified {
        if let Some(agent_id) = &agent_id {
            s.session_id = agent_id.clone();
            s.session_kind = SessionKind::Subagent;
            s.session_kind_source = "agentId".into();
            s.identified = true;
        } else if session_id.as_ref() == Some(&s.session_id) {
            s.identified = true;
        }
    }
    // A subagent's records carry its parent's session ID, though its first
    // record can omit it.
    if s.session_kind == SessionKind::Subagent && s.parent_id.is_none() {
        s.parent_id = session_id.clone();
    }
    // A main transcript's own records carry its session ID and no agent's ID.
    let own = match s.session_kind {
        SessionKind::Subagent => agent_id,
        SessionKind::Main | SessionKind::Unknown => {
            session_id.filter(|_| v.get("agentId").is_none())
        }
    };
    if own.as_ref() == Some(&s.session_id) {
        if s.session_kind == SessionKind::Unknown
            && let Some(sidechain) = v.get("isSidechain").and_then(Value::as_bool)
        {
            s.session_kind = if sidechain {
                SessionKind::Subagent
            } else {
                SessionKind::Main
            };
            s.session_kind_source = "isSidechain".into();
        }
        if s.cwd.is_empty()
            && let Some(cwd) = label(v, "cwd")
        {
            s.cwd = cwd;
        }
    }
}

fn diagnostics(v: &Value) -> Option<Event> {
    let messages: Vec<Value> = v
        .pointer("/attachment/files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|f| f["diagnostics"].as_array().into_iter().flatten())
        .filter_map(|d| {
            let message = d
                .get("message")?
                .as_str()
                .filter(|s| !s.trim().is_empty())?;
            let source = d
                .get("source")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty());
            Some(Value::String(
                source
                    .map(|s| format!("{s}: {message}"))
                    .unwrap_or_else(|| message.into()),
            ))
        })
        .collect();
    if messages.is_empty() {
        return None;
    }
    let mut e = Event::new("tool", EventKind::ToolOutput, "attachment.type");
    crate::normalize::output_prefix(&mut e, &Value::Array(messages));
    e.native_id = native_id(v, "uuid");
    e.ts = timestamp(v);
    Some(e)
}

fn message(v: &Value, typ: &str, model: Option<&Label>) -> Event {
    let m = &v["message"];
    let role = m.get("role").and_then(Value::as_str).unwrap_or(typ);
    let content = &m["content"];
    let mut e = Event::new(
        role,
        EventKind::Message,
        if m["role"].is_string() {
            "message.role"
        } else {
            "root.type"
        },
    );
    e.kind_source = "root.type,message.content";
    e.text = Some(Redacted::new(&message_text(content)));
    e.native_id = if typ == "assistant" {
        native_id(m, "id")
    } else {
        None
    }
    .or_else(|| native_id(v, "uuid"));
    e.model = model.cloned();
    e.ts = timestamp(v);
    // Claude request usage and failures: tests/fixtures/usage-claude.jsonl.
    if e.model.as_deref() != Some("<synthetic>") {
        e.request = usage(&m["usage"]).map(|top| Request {
            finished: !m["stop_reason"].is_null(),
            stop_reason: label(m, "stop_reason"),
            route: id_route(m.get("id"), "message.id"),
            usage: top,
            // One pass is the request itself; a pass list that does not
            // parse whole is not partly counted.
            passes: m
                .pointer("/usage/iterations")
                .and_then(Value::as_array)
                .filter(|passes| passes.len() > 1)
                .and_then(|passes| {
                    passes
                        .iter()
                        .map(|p| {
                            Some(Pass {
                                kind: label(p, "type")?,
                                model: label(p, "model").or_else(|| model.cloned()),
                                usage: usage(p)?,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        });
    } else if v["isApiErrorMessage"] == true {
        e.error = Some(RequestError {
            source: "api_error_message",
            status: v["apiErrorStatus"].as_i64(),
            code: label(v, "error"),
            retry_attempt: None,
            route: id_route(v.get("requestId"), "requestId"),
        });
    }
    e
}

fn tool_use(b: &Value, bt: &str) -> Event {
    let mut e = Event::new("assistant", EventKind::ToolCall, "content.tool_use");
    if bt == "server_tool_use" {
        e.kind = EventKind::ServerToolCall;
        e.role_source = "content.server_tool_use";
        e.kind_source = e.role_source;
    }
    e.tool = label(b, "name");
    e.call_id = native_id(b, "id");
    let args = arguments(&b["input"]);
    e.command = e.tool.as_deref().and_then(|t| shell(t, &args));
    e.text = Some(Redacted::new(&args.to_string()));
    e
}

fn tool_result(v: &Value, b: &Value, bt: &str) -> Event {
    let mut e = Event::new("tool", EventKind::ToolResult, "content.tool_result");
    if bt == "advisor_tool_result" {
        e.kind = EventKind::ServerToolResult;
        e.role_source = "content.advisor_tool_result";
        e.kind_source = e.role_source;
    }
    e.call_id = native_id(b, "tool_use_id");
    e.ok = b.get("is_error").and_then(Value::as_bool).map(|v| !v);
    if e.ok.is_some() {
        e.ok_source = "flag";
    }
    crate::outcomes::classify(&mut e, &b["content"], Agent::Claude);
    crate::normalize::output_prefix(&mut e, &b["content"]);
    tool_denial(&mut e, v);
    e
}

/// Claude Code 2.1.296 writes toolDenialKind from a fixed set and labels hook
/// denials permission-rule; permissionDecision (2.1.293+) names the deciding
/// channel. Rules: docs/formats.md `native_denial_kind` and
/// `permission_decision`.
fn tool_denial(e: &mut Event, v: &Value) {
    if let Some(kind) = v.get("toolDenialKind").and_then(Value::as_str) {
        // Claude Code itself maps interrupted and cancelled to an error
        // outcome, not a denial; is_error still marks the result failed.
        if !matches!(kind, "interrupted" | "cancelled") {
            e.denials.retain(|d| d.source != "native_denial");
            let hook = v.get("permissionDecision").is_some_and(|d| {
                d.get("decision").and_then(Value::as_str) == Some("reject")
                    && d.get("source").and_then(Value::as_str) == Some("hook")
            });
            let source = if hook {
                "hook"
            } else {
                match kind {
                    "automode-blocked" => "classifier",
                    "automode-unavailable" | "automode-parsing-error" => "classifier_unavailable",
                    "permission-rule" => "permission_rule",
                    "user-rejected" => "user_rejected",
                    _ => "native_denial",
                }
            };
            if e.denials.is_empty() {
                e.denials.push(Denial::channel(source));
            }
        }
    }
}

/// The route an Anthropic message or request ID states by its prefix, the
/// part before the last `_`, as `msg_vrtx` in `msg_vrtx_01…`. Rule:
/// docs/formats.md `request_route`.
fn id_route(id: Option<&Value>, source: &'static str) -> Option<Route> {
    let (prefix, _) = id?.as_str()?.rsplit_once('_')?;
    Some(Route {
        value: Label::new(prefix),
        source,
    })
}

/// The tokens a Claude `usage` object or one of its iterations states. Rule:
/// docs/formats.md `usage_columns`.
fn usage(u: &Value) -> Option<Usage> {
    let n = |pointer: &str| u.pointer(pointer).and_then(Value::as_i64);
    Some(Usage {
        input: n("/input_tokens")?,
        output: n("/output_tokens")?,
        cache_read: n("/cache_read_input_tokens"),
        cache_write: n("/cache_creation_input_tokens"),
        cache_write_5m: n("/cache_creation/ephemeral_5m_input_tokens"),
        cache_write_1h: n("/cache_creation/ephemeral_1h_input_tokens"),
        reasoning: n("/output_tokens_details/thinking_tokens"),
    })
}
