use crate::{
    model::{Agent, Event, EventKind, Record, SessionKind, State},
    normalize::{arguments, message_text, shell, string, timestamp},
    redaction::Redacted,
};
use serde_json::Value;

pub fn parse(v: &Value, s: &mut State) -> Record {
    // A subagent's records carry its agentId; a main transcript is named after
    // its session ID, which the indexer already holds. Until the first such
    // record, a resumed transcript holds history copied from another session.
    if !s.identified {
        if let Some(agent_id) = string(v, "agentId") {
            s.session_id = agent_id;
            s.session_kind = SessionKind::Subagent;
            s.session_kind_source = "agentId".into();
            s.identified = true;
        } else if string(v, "sessionId").as_deref() == Some(s.session_id.as_str()) {
            s.identified = true;
        }
    }
    // A subagent's records carry its parent's session ID, though its first
    // record can omit it.
    if s.session_kind == SessionKind::Subagent && s.parent_id.is_none() {
        s.parent_id = string(v, "sessionId");
    }
    // A main transcript's own records carry its session ID and no agent's ID.
    let own = match s.session_kind {
        SessionKind::Subagent => string(v, "agentId"),
        SessionKind::Main | SessionKind::Unknown => {
            string(v, "sessionId").filter(|_| v.get("agentId").is_none())
        }
    };
    if own.as_deref() == Some(s.session_id.as_str()) {
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
            && let Some(cwd) = string(v, "cwd")
        {
            s.cwd = cwd;
        }
    }
    let typ = v.get("type").and_then(Value::as_str).unwrap_or("");
    // Claude diagnostic attachments: tests/fixtures/output-prefix.json.
    if typ == "attachment"
        && v.pointer("/attachment/type").and_then(Value::as_str) == Some("diagnostics")
    {
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
        if !messages.is_empty() {
            let mut e = Event::new("tool", EventKind::ToolOutput, "attachment.type");
            crate::normalize::output_prefix(&mut e, &Value::Array(messages));
            e.native_id = string(v, "uuid");
            e.ts = timestamp(v);
            return Record {
                events: vec![e],
                known: true,
            };
        }
    }
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
                    | "agent-name"
                    | "ai-title"
                    | "atis-latch"
                    | "attachment"
                    | "continued-in"
                    | "cost-state"
                    | "failed"
                    | "file-history-delta"
                    | "fork-context-ref"
                    | "launched"
                    | "mode"
                    | "permission-mode"
                    | "pr-link"
                    | "relocated"
                    | "worktree-state"
            ),
        };
    }
    let m = &v["message"];
    // Claude model fields and synthetic errors: tests/fixtures/claude.jsonl.
    if let Some(model) = string(m, "model").filter(|m| m != "<synthetic>") {
        s.model = Some(model);
        s.model_source = "message.model".into();
    }
    let role = m.get("role").and_then(Value::as_str).unwrap_or(typ);
    let content = &m["content"];
    let mut events = Vec::new();
    // Claude fragments, empty replies, and compaction: tests/fixtures/structure.jsonl.
    let summary = v.get("isCompactSummary").and_then(Value::as_bool) == Some(true);
    if summary {
        return Record {
            events: Vec::new(),
            known: true,
        };
    }
    let only_results = content
        .as_array()
        .is_some_and(|a| !a.is_empty() && a.iter().all(|b| b["type"] == "tool_result"));
    if typ == "assistant" || !only_results {
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
            string(m, "id").map(|id| format!("message:{id}"))
        } else {
            None
        }
        .or_else(|| string(v, "uuid"));
        e.model = string(m, "model");
        e.ts = timestamp(v);
        events.push(e);
    }
    if let Some(blocks) = content.as_array() {
        for (i, b) in blocks.iter().enumerate() {
            let bt = b["type"].as_str().unwrap_or("");
            let mut e = match bt {
                "tool_use" | "server_tool_use" => {
                    let mut e = Event::new("assistant", EventKind::ToolCall, "content.tool_use");
                    if bt == "server_tool_use" {
                        e.kind = EventKind::ServerToolCall;
                        e.role_source = "content.server_tool_use";
                        e.kind_source = e.role_source;
                    }
                    e.tool = string(b, "name");
                    e.call_id = string(b, "id");
                    let args = arguments(&b["input"]);
                    e.command = e.tool.as_deref().and_then(|t| shell(t, &args));
                    e.text = Some(Redacted::new(&args.to_string()));
                    e
                }
                "tool_result" | "advisor_tool_result" => {
                    let mut e = Event::new("tool", EventKind::ToolResult, "content.tool_result");
                    if bt == "advisor_tool_result" {
                        e.kind = EventKind::ServerToolResult;
                        e.role_source = "content.advisor_tool_result";
                        e.kind_source = e.role_source;
                    }
                    e.call_id = string(b, "tool_use_id");
                    e.ok = b.get("is_error").and_then(Value::as_bool).map(|v| !v);
                    if e.ok.is_some() {
                        e.ok_source = "flag";
                    }
                    crate::outcomes::classify(&mut e, &b["content"], Agent::Claude);
                    crate::normalize::output_prefix(&mut e, &b["content"]);
                    if let Some(kind) = v.get("toolDenialKind").and_then(Value::as_str) {
                        e.denials.retain(|(source, _)| source != "native_denial");
                        let source = match kind {
                            "classifier" | "automode-blocked" => "classifier",
                            "classifier-unavailable" | "automode-unavailable" => {
                                "classifier_unavailable"
                            }
                            "hook" => "hook",
                            "permission-rule" => "permission_rule",
                            "user-rejected" => "user_rejected",
                            _ => "native_denial",
                        };
                        if e.denials.is_empty() {
                            e.denials.push((source.into(), "unknown".into()));
                        }
                    }
                    e
                }
                _ => continue,
            };
            e.native_id = e
                .call_id
                .as_ref()
                .map(|id| format!("{}:{id}", e.kind.as_str()))
                .or_else(|| string(v, "uuid").map(|id| format!("{id}:{i}")));
            e.model = string(m, "model");
            e.ts = timestamp(v);
            events.push(e);
        }
    }
    Record {
        events,
        known: true,
    }
}
