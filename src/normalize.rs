use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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

pub fn arguments(v: &Value) -> Value {
    if let Some(s) = v.as_str() {
        serde_json::from_str(s).unwrap_or_else(|_| v.clone())
    } else {
        v.clone()
    }
}

pub fn shell(tool: &str, args: &Value) -> Option<String> {
    match tool.rsplit('.').next().unwrap_or(tool) {
        "Bash" | "bash" | "exec_command" | "shell_command" => {
            string(args, "command").or_else(|| string(args, "cmd"))
        }
        "shell" => args.get("command").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| format!("'{}'", s.replace('\'', "'\\''")))
                .collect::<Vec<_>>()
                .join(" ")
        }),
        _ => None,
    }
}
