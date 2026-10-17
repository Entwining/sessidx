mod claude;
mod codex;
mod pi;

use crate::model::{Agent, Event, EventKind, State};
use serde_json::Value;

pub fn parse(agent: Agent, v: &Value, state: &mut State) -> Vec<Event> {
    if let Some(ts) = crate::normalize::timestamp(v)
        && state.first_ts.is_none()
    {
        state.first_ts = Some(ts);
    }
    let mut events = match agent {
        Agent::Claude => claude::parse(v, state),
        Agent::Codex => codex::parse(v, state),
        Agent::Pi => pi::parse(v, state),
    };
    // A record no rule reads keeps only its location: docs/formats.md
    // `context_pointers`.
    if events.is_empty() {
        events.push(Event::context(v));
    }
    for e in &mut events {
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
    }
    events
}
