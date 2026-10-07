use regex::Regex;
use std::{collections::HashMap, sync::LazyLock};

static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
    r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
    r"(?i)\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=\-]+",
    r"\b(?:sk-[A-Za-z0-9_\-]{8,}|(?:gh[pousr]_|github_pat_|xox[baprs]-|AKIA|ASIA)[A-Za-z0-9_\-]{8,})",
].into_iter().map(|p| Regex::new(p).unwrap()).collect()
});
static ASSIGNMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']?([A-Za-z0-9_\-]+)["']?\s*[=:]\s*"#).unwrap());
static ASSIGNED_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,"'<>}]+)"#).unwrap()
});
static RUNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=.!@$%:&?\-]{16,}").unwrap());
static CREDENTIAL_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(api[_-]?key|secret|password|passwd|token|credential|authorization)|^key$|(?:private[_-]?key|auth|sig|signature|bearer)$")
        .unwrap()
});
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9.!#$%&*+/=?^_`{|}~\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}$").unwrap()
});
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://[A-Za-z0-9.\-]+(?::[0-9]+)?(?:[/?][^@]*)?$").unwrap()
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
        let identifier = run.trim_end_matches(['.', '!', '?', ':']);
        if URL.is_match(identifier) {
            return redact_url(run);
        }
        // Body identifiers and credential-context controls: testdata/identifiers.jsonl.
        let path = identifier.starts_with('/')
            && identifier.split('/').filter(|s| !s.is_empty()).count() >= 2
            && identifier.split('/').skip(1).all(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-.~".contains(&b))
            });
        if path || EMAIL.is_match(identifier) {
            return run.to_owned();
        }
        redact_run(run)
    })
    .into_owned()
}

fn redact_url(run: &str) -> String {
    let scheme = run.find("://").unwrap() + 3;
    let end = run[scheme..]
        .find(['/', '?'])
        .map_or(run.len(), |i| scheme + i);
    let mut out = run[..end].to_owned();
    let (path, query) = run[end..]
        .split_once('?')
        .map_or((&run[end..], None), |(path, query)| (path, Some(query)));
    let component = |text: &str| {
        RUNS.replace_all(text, |caps: &regex::Captures<'_>| redact_run(&caps[0]))
            .into_owned()
    };
    for (i, segment) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&component(segment));
    }
    if let Some(query) = query {
        out.push('?');
        for (i, pair) in query.split('&').enumerate() {
            if i > 0 {
                out.push('&');
            }
            if let Some((name, value)) = pair.split_once('=') {
                out.push_str(name);
                out.push('=');
                out.push_str(&component(value));
            } else {
                out.push_str(&component(pair));
            }
        }
    }
    out
}

fn redact_run(run: &str) -> String {
    let identifier = run.trim_end_matches(['.', '!', '?', ':']);
    let hex = identifier
        .strip_prefix("0x")
        .or_else(|| identifier.strip_prefix("0X"))
        .unwrap_or(identifier);
    let uuid = identifier.len() == 36
        && identifier
            .split('-')
            .zip([8, 4, 4, 4, 12])
            .all(|(s, n)| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit()));
    if hex.bytes().all(|b| b.is_ascii_hexdigit()) || uuid || identifier_shaped(identifier) {
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
}

fn identifier_shaped(run: &str) -> bool {
    let run = run
        .strip_prefix("--")
        .or_else(|| run.strip_prefix('-'))
        .unwrap_or(run);
    run.split(['.', '/', '-'])
        .flat_map(|segment| segment.split("::"))
        .all(|segment| {
            let letters = segment.trim_end_matches(|c: char| c.is_ascii_digit());
            !letters.is_empty()
                && letters
                    .bytes()
                    .all(|b| b.is_ascii_alphabetic() || b == b'_')
        })
}

pub fn redact_metadata(text: &str) -> String {
    let mut out = text.to_owned();
    for pattern in PATTERNS.iter() {
        out = pattern.replace_all(&out, "[REDACTED]").into_owned();
    }
    let mut redacted = String::new();
    let mut offset = 0;
    for caps in ASSIGNMENTS.captures_iter(&out) {
        let assignment = caps.get(0).unwrap();
        if assignment.start() >= offset
            && CREDENTIAL_NAME.is_match(&caps[1])
            && let Some(value) = ASSIGNED_VALUE.find(&out[assignment.end()..])
        {
            redacted.push_str(&out[offset..assignment.start()]);
            redacted.push_str("[REDACTED]");
            offset = assignment.end() + value.end();
        }
    }
    redacted.push_str(&out[offset..]);
    redacted
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
