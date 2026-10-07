mod claude;
mod codex;
mod pi;

use crate::{
    model::{Record, State},
    redaction::{redact, redact_metadata},
};
use serde_json::Value;

pub fn parse(harness: &str, v: &Value, state: &mut State) -> Record {
    if let Some(ts) = crate::normalize::timestamp(v)
        && state.first_ts.is_none()
    {
        state.first_ts = Some(ts);
    }
    let mut r = match harness {
        "claude" => claude::parse(v, state),
        "codex" => codex::parse(v, state),
        "pi" => pi::parse(v, state),
        _ => Record {
            events: Vec::new(),
            known: false,
        },
    };
    if r.events.is_empty() {
        let mut e = crate::model::Event::new("unknown", "context", "root.type");
        e.role_source = "none".into();
        e.ts = crate::normalize::timestamp(v);
        e.native_id =
            crate::normalize::string(v, "uuid").or_else(|| crate::normalize::string(v, "id"));
        r.events.push(e);
    }
    state.session_id = redact_metadata(&state.session_id);
    state.cwd = redact_metadata(&state.cwd);
    state.model = state.model.as_deref().map(redact_metadata);
    state.parent_id = state.parent_id.as_deref().map(redact_metadata);
    for e in &mut r.events {
        e.native_id = e
            .native_id
            .as_deref()
            .map(|id| crate::normalize::hash(id.as_bytes()));
        e.model = e.model.as_deref().map(redact_metadata);
        e.tool = e.tool.as_deref().map(redact_metadata);
        e.call_id = e
            .call_id
            .as_deref()
            .map(|id| crate::normalize::hash(id.as_bytes()));
        if let Some(command) = &e.command {
            e.sites = crate::shell::sites(command);
        } else if e.kind == "tool_call"
            && e.tool
                .as_deref()
                .is_some_and(crate::normalize::is_shell_tool)
        {
            e.sites = vec![crate::shell::Site {
                program: None,
                argv: Vec::new(),
                parsed: false,
            }];
        }
        for site in &mut e.sites {
            site.program = site.program.as_deref().map(redact_metadata);
            site.argv = site.argv.iter().map(|s| redact(s)).collect();
        }
        e.text = e.text.as_deref().map(crate::redaction::redact_serialized);
        e.command = e.command.as_deref().map(redact);
        e.role = redact(&e.role);
    }
    r
}
