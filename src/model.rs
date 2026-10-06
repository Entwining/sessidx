use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub first_ts: Option<String>,
    pub session_id: String,
    pub cwd: String,
    pub model: Option<String>,
    pub model_source: String,
    pub instruction_hash: Option<String>,
    pub parent_id: Option<String>,
    pub kind: String,
    pub kind_source: String,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub native_id: Option<String>,
    pub ts: Option<String>,
    pub model: Option<String>,
    pub role: String,
    pub role_source: String,
    pub kind: String,
    pub kind_source: String,
    pub text: Option<String>,
    pub text_truncated: bool,
    pub tool: Option<String>,
    pub call_id: Option<String>,
    pub command: Option<String>,
    pub sites: Vec<crate::shell::Site>,
    pub ok: Option<bool>,
    pub ok_source: String,
    pub exit_code: Option<i64>,
    pub denials: Vec<(String, String)>,
}

impl Event {
    pub fn new(role: &str, kind: &str, source: &str) -> Self {
        Self {
            native_id: None,
            ts: None,
            model: None,
            role: role.into(),
            role_source: source.into(),
            kind: kind.into(),
            kind_source: source.into(),
            text: None,
            text_truncated: false,
            tool: None,
            call_id: None,
            command: None,
            sites: Vec::new(),
            ok: None,
            ok_source: "none".into(),
            exit_code: None,
            denials: Vec::new(),
        }
    }
}

pub struct Record {
    pub events: Vec<Event>,
    pub known: bool,
}
