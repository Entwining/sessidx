//! What the adapters read from a record, which the store writes as read. Text
//! that reaches the index carries its redaction in its type: `Redacted` for
//! free text, `Label` for labels and identities, `Digest` for native IDs. A
//! plain `String` that reaches it is a normalized timestamp, a name from this
//! parser's own vocabulary, or a reason marker's parts, which
//! `Denial::from_marker` keeps only when redaction leaves them unchanged.
//! Rule: docs/formats.md `stored_strings`.

use crate::{
    normalize::Digest,
    redaction::{Label, Redacted},
};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Pi,
}

/// The command-line spelling of a value, which is also the text stored and reported for it.
pub fn name(value: impl ValueEnum) -> String {
    value
        .to_possible_value()
        .expect("ValueEnum variants are not skipped")
        .get_name()
        .to_owned()
}

/// Whether a session was started on its own or spawned by another session of
/// the same agent. Rule: docs/formats.md `session_kind`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Main,
    Subagent,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Message,
    ToolCall,
    ServerToolCall,
    ToolResult,
    ServerToolResult,
    ToolOutput,
    Communication,
    Context,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::ToolCall => "tool_call",
            Self::ServerToolCall => "server_tool_call",
            Self::ToolResult => "tool_result",
            Self::ServerToolResult => "server_tool_result",
            Self::ToolOutput => "tool_output",
            Self::Communication => "communication",
            Self::Context => "context",
        }
    }
}

/// What a transcript has established so far, carried across appends.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub first_ts: Option<String>,
    /// The transcript's own identity is fixed by the first record that states
    /// it; copied history later in the file carries other sessions' identities.
    /// Rule: docs/formats.md `transcript_identity`.
    pub identified: bool,
    pub session_id: Label,
    pub parent_id: Option<Label>,
    pub session_kind: SessionKind,
    pub session_kind_source: String,
    /// The directory the session started in.
    pub cwd: Label,
    pub model: Option<Label>,
    pub model_source: String,
    pub instruction_hash: Option<Digest>,
    /// Whether this Codex rollout has written a `token_usage_record`, and the
    /// running total of its last `token_count`. Rule: docs/formats.md
    /// `codex_token_count_request`.
    pub codex_usage_records: bool,
    pub codex_total_tokens: Option<i64>,
    /// The provider this Codex rollout's own header names. Rule:
    /// docs/formats.md `request_route`.
    pub codex_provider: Option<Label>,
    /// The latest Pi entry that called tools, whose record ID scopes the call
    /// IDs of the results after it. Rule: docs/formats.md
    /// `native_call_identity`.
    pub pi_tool_record: Option<Digest>,
}

/// The tokens one model request or pass used, as its record states them.
/// `input` excludes cached and cache-written tokens, and `output` includes
/// `reasoning`. Rule: docs/formats.md `usage_columns`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_read: Option<i64>,
    pub cache_write: Option<i64>,
    pub cache_write_5m: Option<i64>,
    pub cache_write_1h: Option<i64>,
    pub reasoning: Option<i64>,
}

/// One model pass of a request that lists several, such as an advisor's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pass {
    pub kind: Label,
    pub model: Option<Label>,
    pub usage: Usage,
}

/// What one record states about the token usage of the model request its
/// event belongs to. `finished` is whether the record states that the
/// response ended, and `stop_reason` why, in the agent's own words. Rules:
/// docs/formats.md `usage_snapshot`, `request_stop_reason`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub finished: bool,
    pub stop_reason: Option<Label>,
    pub route: Option<Route>,
    pub usage: Usage,
    pub passes: Vec<Pass>,
}

/// Evidence of where a request was sent, as the field named by `source`
/// states it, which does not establish how it was billed. Rule:
/// docs/formats.md `request_route`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub value: Label,
    pub source: &'static str,
}

/// A record in which the agent states that a model request failed. `source`
/// names that record. Rule: docs/formats.md `request_errors`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestError {
    pub source: &'static str,
    pub status: Option<i64>,
    pub code: Option<Label>,
    pub retry_attempt: Option<i64>,
    pub route: Option<Route>,
}

/// A compaction of a session's context, the context sizes its record states,
/// the usage of the calls that wrote its summary, and the native ID of the
/// request that wrote it. Rules: docs/formats.md `compaction_records`,
/// `codex_compaction_request`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    pub trigger: &'static str,
    pub pre_tokens: Option<i64>,
    pub post_tokens: Option<i64>,
    pub summary: Option<Usage>,
    pub request_id: Option<Digest>,
}

/// One recognized denial of a tool call: the channel that recorded it, and
/// the class, producer, and rule ID a reason marker or a frozen legacy
/// sentence names. Values: docs/cli.md `denials`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denial {
    pub source: &'static str,
    pub class: &'static str,
    pub producer: Option<String>,
    pub reason_id: String,
}

impl Denial {
    /// A denial known only by its channel.
    pub fn channel(source: &'static str) -> Self {
        Self {
            source,
            class: "unknown",
            producer: None,
            reason_id: "unknown".into(),
        }
    }

    /// A denial classified by the frozen guard sentence table.
    pub fn legacy(source: &'static str, class: &'static str, reason_id: &'static str) -> Self {
        Self {
            source,
            class,
            producer: None,
            reason_id: reason_id.into(),
        }
    }

    /// A denial whose reason a reason marker declares. A marker that
    /// redaction would change keeps only its class. Rule: docs/formats.md
    /// `reason_marker`.
    pub fn from_marker(source: &'static str, marker: crate::outcomes::Marker) -> Self {
        let text = format!("[{}:{}/{}]", marker.class, marker.producer, marker.reason);
        let kept = Label::verbatim(&text).is_some();
        Self {
            source,
            class: marker.class,
            producer: kept.then_some(marker.producer),
            reason_id: if kept {
                marker.reason
            } else {
                "unknown".into()
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub native_id: Option<Digest>,
    pub ts: Option<String>,
    pub model: Option<Label>,
    pub role: Label,
    pub role_source: &'static str,
    pub kind: EventKind,
    pub kind_source: &'static str,
    pub text: Option<Redacted>,
    pub text_truncated: bool,
    pub tool: Option<Label>,
    pub call_id: Option<Digest>,
    pub command: Option<String>,
    pub sites: Vec<crate::shell::Site>,
    pub ok: Option<bool>,
    pub ok_source: &'static str,
    pub exit_code: Option<i64>,
    pub denials: Vec<Denial>,
    pub request: Option<Request>,
    pub error: Option<RequestError>,
    pub compaction: Option<Compaction>,
}

impl Event {
    /// The event of a record that holds no message or tool, which keeps its
    /// location and native ID.
    pub fn context(v: &serde_json::Value) -> Self {
        let mut e = Self::new("unknown", EventKind::Context, "root.type");
        e.role_source = "none";
        e.ts = crate::normalize::timestamp(v);
        e.native_id =
            crate::normalize::native_id(v, "uuid").or_else(|| crate::normalize::native_id(v, "id"));
        e
    }

    pub fn new(role: &str, kind: EventKind, source: &'static str) -> Self {
        Self {
            native_id: None,
            ts: None,
            model: None,
            role: Label::new(role),
            role_source: source,
            kind,
            kind_source: source,
            text: None,
            text_truncated: false,
            tool: None,
            call_id: None,
            command: None,
            sites: Vec::new(),
            ok: None,
            ok_source: "none",
            exit_code: None,
            denials: Vec::new(),
            request: None,
            error: None,
            compaction: None,
        }
    }
}
