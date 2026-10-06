pub mod claude;
pub mod codex;
pub mod pi;

use crate::{
    model::{Record, State},
    redaction::redact,
};
use serde_json::Value;

pub fn parse(harness: &str, v: &Value, state: &mut State) -> Record {
    let mut r = match harness {
        "claude" => claude::parse(v, state),
        "codex" => codex::parse(v, state),
        "pi" => pi::parse(v, state),
        _ => Record {
            events: Vec::new(),
            known: false,
        },
    };
    state.session_id = redact(&state.session_id);
    state.cwd = redact(&state.cwd);
    state.model = state.model.as_deref().map(redact);
    state.parent_id = state.parent_id.as_deref().map(redact);
    for e in &mut r.events {
        e.native_id = e.native_id.as_deref().map(redact);
        e.text = e.text.as_deref().map(redact);
        e.tool = e.tool.as_deref().map(redact);
        e.call_id = e.call_id.as_deref().map(redact);
        e.command = e.command.as_deref().map(redact);
        e.role = redact(&e.role);
    }
    r
}
