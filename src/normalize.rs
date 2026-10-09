use crate::model::Harness;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub fn timestamp(v: &Value) -> Option<String> {
    let value = v.get("timestamp")?;
    if let Some(n) = value.as_i64() {
        return DateTime::<Utc>::from_timestamp_millis(n)
            .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true));
    }
    DateTime::parse_from_rfc3339(value.as_str()?).ok().map(|t| {
        t.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    })
}

pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(m) => m
            .get("text")
            .or_else(|| m.get("content"))
            .map(text)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Claude and Pi message content: a string, or the text blocks of a block array.
pub fn message_text(content: &Value) -> String {
    if content.is_string() {
        return text(content);
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .map(text)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn arguments(v: &Value) -> Value {
    if let Some(s) = v.as_str() {
        serde_json::from_str(s).unwrap_or_else(|_| v.clone())
    } else {
        v.clone()
    }
}

pub fn output_prefix(e: &mut crate::model::Event, output: &Value) {
    const LIMIT: usize = 2048;
    fn append(v: &Value, text: &mut String, truncated: &mut bool) {
        if *truncated {
            return;
        }
        match v {
            Value::String(s) if !s.is_empty() => {
                if !text.is_empty() {
                    if text.len() == LIMIT {
                        *truncated = true;
                        return;
                    }
                    text.push('\n');
                }
                let end = s.floor_char_boundary(LIMIT - text.len());
                text.push_str(&s[..end]);
                *truncated = end < s.len();
            }
            Value::Array(a) => {
                for v in a {
                    append(v, text, truncated);
                    if *truncated {
                        break;
                    }
                }
            }
            Value::Object(m) => {
                if let Some(v) = m
                    .get("text")
                    .or_else(|| m.get("content"))
                    .or_else(|| m.get("output"))
                {
                    append(v, text, truncated);
                } else {
                    append(&Value::String(v.to_string()), text, truncated);
                }
            }
            _ => {}
        }
    }
    // Tool output and UTF-8 clipping: tests/it/output_prefix.rs::tool_output_prefix_is_bounded_and_raw_tail_remains_reachable.
    let mut text = String::with_capacity(LIMIT);
    append(output, &mut text, &mut e.text_truncated);
    let redacted = crate::redaction::redact_serialized(&text);
    let end = redacted.floor_char_boundary(LIMIT);
    e.text_truncated |= end < redacted.len();
    e.text = if end == 0 {
        None
    } else {
        Some(redacted[..end].into())
    };
}

pub fn shell(tool: &str, args: &Value) -> Option<String> {
    if !is_shell_tool(tool) {
        return None;
    }
    if tool.rsplit('.').next() == Some("shell") {
        args.get("command").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| format!("'{}'", s.replace('\'', "'\\''")))
                .collect::<Vec<_>>()
                .join(" ")
        })
    } else {
        string(args, "command").or_else(|| string(args, "cmd"))
    }
}

pub fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool.rsplit('.').next().unwrap_or(tool),
        "Bash" | "bash" | "exec_command" | "shell_command" | "shell"
    )
}

// Pi thinkingSignature and opaque image/thinking blocks: tests/fixtures/opaque.jsonl.
pub fn record_for_index(bytes: &[u8], harness: Harness) -> serde_json::Result<Value> {
    use serde_json::value::RawValue;
    use std::collections::BTreeMap;
    if !matches!(harness, Harness::Claude | Harness::Pi)
        || bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
    {
        return serde_json::from_slice(bytes);
    }
    let fields: BTreeMap<String, &RawValue> = serde_json::from_slice(bytes)?;
    let mut record = serde_json::Map::new();
    for (key, raw) in fields {
        let value = if key == "message" && raw.get().starts_with('{') {
            let fields: BTreeMap<String, &RawValue> = serde_json::from_str(raw.get())?;
            let mut message = serde_json::Map::new();
            for (key, raw) in fields {
                let value = if key == "content" && raw.get().starts_with('[') {
                    let blocks: Vec<&RawValue> = serde_json::from_str(raw.get())?;
                    let mut content = Vec::with_capacity(blocks.len());
                    for raw in blocks {
                        let mut opaque = None;
                        if raw.get().starts_with('{') {
                            let fields: BTreeMap<String, &RawValue> =
                                serde_json::from_str(raw.get())?;
                            if let Some(raw_type) = fields.get("type")
                                && let Ok(typ) = serde_json::from_str::<String>(raw_type.get())
                                && matches!(typ.as_str(), "thinking" | "image" | "fallback")
                            {
                                opaque = Some(serde_json::json!({"type":typ}));
                            }
                        }
                        content.push(match opaque {
                            Some(v) => v,
                            None => serde_json::from_str(raw.get())?,
                        });
                    }
                    Value::Array(content)
                } else {
                    serde_json::from_str(raw.get())?
                };
                message.insert(key, value);
            }
            Value::Object(message)
        } else {
            serde_json::from_str(raw.get())?
        };
        record.insert(key, value);
    }
    Ok(Value::Object(record))
}
