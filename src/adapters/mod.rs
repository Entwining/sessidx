mod claude;
mod codex;
mod pi;

use crate::{
    model::{Agent, EventKind, Record, State},
    redaction::{redact, redact_argv, redact_metadata},
};
use serde_json::Value;

pub fn parse(agent: Agent, v: &Value, state: &mut State) -> Record {
    if let Some(ts) = crate::normalize::timestamp(v)
        && state.first_ts.is_none()
    {
        state.first_ts = Some(ts);
    }
    let mut r = match agent {
        Agent::Claude => claude::parse(v, state),
        Agent::Codex => codex::parse(v, state),
        Agent::Pi => pi::parse(v, state),
    };
    if r.events.is_empty() {
        r.events.push(crate::model::Event::context(v));
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
        for pass in e.request.iter_mut().flat_map(|r| &mut r.passes) {
            pass.kind = redact_metadata(&pass.kind);
            pass.model = pass.model.as_deref().map(redact_metadata);
        }
        e.tool = e.tool.as_deref().map(redact_metadata);
        e.call_id = e
            .call_id
            .as_deref()
            .map(|id| crate::normalize::hash(id.as_bytes()));
        if let Some(command) = &e.command {
            e.sites = crate::shell::sites(command);
        } else if e.kind == EventKind::ToolCall
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
            site.argv = redact_argv(&site.argv);
        }
        e.role = redact(&e.role);
    }
    r
}
