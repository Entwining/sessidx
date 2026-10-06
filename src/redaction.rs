use regex::Regex;
use std::{collections::HashMap, sync::LazyLock};

static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
    r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
    r"(?i)\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=\-]+",
    r"\b(?:sk-[A-Za-z0-9_\-]{8,}|(?:gh[pousr]_|github_pat_|xox[baprs]-|AKIA|ASIA)[A-Za-z0-9_\-]{8,})",
    r#"(?i)["']?(?:[A-Za-z0-9_\-]*(?:api[_-]?key|secret|password|passwd|token|credential)[A-Za-z0-9_\-]*|authorization|\bkey)["']?\s*[=:]\s*(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,"'<>}]+)"#,
].into_iter().map(|p| Regex::new(p).unwrap()).collect()
});
static RUNS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=\-]{16,}").unwrap());
static CREDENTIAL_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(api[_-]?key|secret|password|passwd|token|credential|authorization)|(?i)^key$")
        .unwrap()
});

pub fn redact_value(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => *s = redact(s),
        serde_json::Value::Array(a) => a.iter_mut().for_each(redact_value),
        serde_json::Value::Object(m) => {
            let old = std::mem::take(m);
            for (key, mut value) in old {
                if CREDENTIAL_NAME.is_match(&key) {
                    value = serde_json::Value::String("[REDACTED]".into());
                } else {
                    redact_value(&mut value);
                }
                m.insert(redact(&key), value);
            }
        }
        _ => {}
    }
}

pub fn redact_serialized(s: &str) -> String {
    if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(s) {
        if v.is_object() || v.is_array() {
            redact_value(&mut v);
            return v.to_string();
        }
    }
    redact(s)
}

pub fn redact(text: &str) -> String {
    let out = redact_metadata(text);
    RUNS.replace_all(&out, |caps: &regex::Captures<'_>| {
        let run = &caps[0];
        let hex = run
            .strip_prefix("0x")
            .or_else(|| run.strip_prefix("0X"))
            .unwrap_or(run);
        let uuid = run.len() == 36
            && run
                .split('-')
                .zip([8, 4, 4, 4, 12])
                .all(|(s, n)| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit()));
        // Body identifiers and credential-context controls: testdata/identifiers.jsonl.
        if hex.bytes().all(|b| b.is_ascii_hexdigit()) || uuid || run.starts_with('/') {
            return run.to_owned();
        }
        let mut counts = HashMap::new();
        for byte in run.bytes() {
            *counts.entry(byte).or_insert(0usize) += 1;
        }
        let entropy: f64 = counts
            .values()
            .map(|&n| {
                let p = n as f64 / run.len() as f64;
                -p * p.log2()
            })
            .sum();
        if entropy >= 3.5 {
            "[REDACTED]".to_owned()
        } else {
            run.to_owned()
        }
    })
    .into_owned()
}

pub fn redact_metadata(text: &str) -> String {
    let mut out = text.to_owned();
    for pattern in PATTERNS.iter() {
        out = pattern.replace_all(&out, "[REDACTED]").into_owned();
    }
    out
}

pub fn spaced_cjk(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c as u32, 0x3400..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af) {
            out.push(' ');
            out.push(c);
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
