use regex::Regex;
use std::{collections::HashMap, sync::LazyLock};

static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
    r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
    r"(?i)\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=\-]+",
    r"\b(?:sk-[A-Za-z0-9_\-]{8,}|(?:gh[pousr]_|github_pat_|xox[baprs]-|AKIA|ASIA)[A-Za-z0-9_\-]{8,})",
].into_iter().map(|p| Regex::new(p).expect("static regex is valid")).collect()
});
static ASSIGNMENTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"["']?([A-Za-z0-9_\-]+)["']?\s*[=:]\s*"#).expect("static regex is valid")
});
static ASSIGNED_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,"'<>}]+)"#)
        .expect("static regex is valid")
});
static RUNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=.!@$%:&?\-]{16,}").expect("static regex is valid"));
static CREDENTIAL_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(api[_-]?key|secret|password|passwd|token|credential|authorization)|^key$|(?:private[_-]?key|auth|sig|signature|bearer)$")
        .expect("static regex is valid")
});
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9.!#$%&*+/=?^_`{|}~\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}$")
        .expect("static regex is valid")
});
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://[A-Za-z0-9.\-]+(?::[0-9]+)?(?:[/?][^@]*)?$")
        .expect("static regex is valid")
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
    if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(s)
        && (v.is_object() || v.is_array())
    {
        redact_value(&mut v);
        return v.to_string();
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
        let mut location = identifier;
        for _ in 0..2 {
            if let Some((path, number)) = location.rsplit_once(':')
                && !number.is_empty()
                && number.bytes().all(|b| b.is_ascii_digit())
                && path.contains(['/', '.'])
            {
                location = path;
            } else {
                break;
            }
        }
        // Body identifiers and credential-context controls: tests/fixtures/identifiers.jsonl.
        let path = location.starts_with('/')
            && location.split('/').filter(|s| !s.is_empty()).count() >= 2
            && location.split('/').skip(1).all(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-.~".contains(&b))
            });
        if path || EMAIL.is_match(identifier) {
            return run.to_owned();
        }
        redact_run(run, location)
    })
    .into_owned()
}

fn redact_url(run: &str) -> String {
    let scheme = run.find("://").expect("URL matched before redact_url") + 3;
    let end = run[scheme..]
        .find(['/', '?'])
        .map_or(run.len(), |i| scheme + i);
    let mut out = run[..end].to_owned();
    let (path, query) = run[end..]
        .split_once('?')
        .map_or((&run[end..], None), |(path, query)| (path, Some(query)));
    let component = |text: &str| {
        RUNS.replace_all(text, |caps: &regex::Captures<'_>| {
            let run = &caps[0];
            redact_run(run, run.trim_end_matches(['.', '!', '?', ':']))
        })
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

fn redact_run(run: &str, identifier: &str) -> String {
    let hex = identifier
        .strip_prefix("0x")
        .or_else(|| identifier.strip_prefix("0X"))
        .unwrap_or(identifier);
    let uuid = identifier.len() == 36
        && identifier
            .split('-')
            .zip([8, 4, 4, 4, 12])
            .all(|(s, n)| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit()));
    // Rule: docs/formats.md `rfc3339_timestamp`. Parsed before locator removal,
    // which would take a `+08:00` offset for a line number.
    let timestamp =
        chrono::DateTime::parse_from_rfc3339(run.trim_end_matches(['.', '!', '?', ':'])).is_ok();
    if hex.bytes().all(|b| b.is_ascii_hexdigit())
        || uuid
        || timestamp
        || identifier_shaped(identifier)
    {
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

// Rules: docs/formats.md `code_identifiers`, `relative_path_shape`, `numeric_identifier_pieces`.
fn identifier_shaped(run: &str) -> bool {
    let run = run
        .strip_prefix("--")
        .or_else(|| run.strip_prefix('-'))
        .unwrap_or(run);
    let run = run.strip_prefix('/').unwrap_or(run);
    let mut alphabetic = false;
    let shaped = run.split('/').all(|segment| {
        if matches!(segment, "." | ".." | "@") {
            return true;
        }
        let segment = segment.trim_start_matches('.');
        let segment = segment.strip_prefix('@').unwrap_or(segment);
        segment
            .split(['.', '-', '@'])
            .flat_map(|piece| piece.split("::"))
            .all(|piece| {
                let letters = piece.trim_end_matches(|c: char| c.is_ascii_digit());
                if letters.is_empty() {
                    return !piece.is_empty();
                }
                alphabetic |= letters.bytes().any(|b| b.is_ascii_alphabetic());
                letters
                    .bytes()
                    .all(|b| b.is_ascii_alphabetic() || b == b'_')
            })
    });
    shaped && alphabetic
}

pub fn redact_metadata(text: &str) -> String {
    let mut out = text.to_owned();
    for pattern in PATTERNS.iter() {
        out = pattern.replace_all(&out, "[REDACTED]").into_owned();
    }
    let mut redacted = String::new();
    let mut offset = 0;
    for caps in ASSIGNMENTS.captures_iter(&out) {
        let assignment = caps.get_match();
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
