use regex::Regex;
use serde_json::Value;
use std::{collections::HashMap, ops::Deref, sync::LazyLock};

static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
    r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)",
    r"(?i)\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=\-]+",
    r"\b(?:sk-[A-Za-z0-9_\-]{8,}|(?:gh[pousr]_|github_pat_|xox[baprs]-|AKIA|ASIA)[A-Za-z0-9_\-]{8,})",
    r"\b(?:hf_[A-Za-z]{34}\b|SG\.[A-Za-z0-9_\-]{22}\.[A-Za-z0-9_\-]{43}(?P<end>[^A-Za-z0-9_\-]|$))",
].into_iter().map(|p| Regex::new(p).expect("static regex is valid")).collect()
});
/// `name=value` or `name: value`, with optional quotes around the name, or a
/// `--name value` flag pair, or a Cookie/Set-Cookie header.
static ASSIGNMENTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?P<header>(?:(?P<quote>\\?["'])[ \t]*)?\b(?i:Cookie|Set-Cookie):[ \t]*)|(?:\\?["'])?(?P<name>[A-Za-z0-9_\-]+)(?:\\?["'])?\s*[=:]\s*|(?:^|\s)--?(?P<flag>[A-Za-z0-9][A-Za-z0-9_\-]*)[ \t]+"#,
    )
    .expect("static regex is valid")
});
// A quoted value without its closing quote runs to the end of the text, as in
// a cut output prefix.
static ASSIGNED_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(?:\\"(?s:.*?)(?:\\"|$)|\\'(?s:.*?)(?:\\'|$)|"(?:\\.|[^"\\])*"?|'(?:\\.|[^'\\])*'?|[^\s,"'<>}]+)"#)
        .expect("static regex is valid")
});
// An unquoted value ends where the next URL query pair starts.
static QUERY_PAIR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&[A-Za-z0-9_\-]+=").expect("static regex is valid"));
static RUNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=.!@$%:&?\-]{16,}").expect("static regex is valid"));
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9.!#$%&*+/=?^_`{|}~\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}$")
        .expect("static regex is valid")
});
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^https?://[A-Za-z0-9.\-]+(?::[0-9]+)?(?:[/?][^@]*)?$")
        .expect("static regex is valid")
});
// RFC 3339 and its file-name spelling with `-` between time fields, whose
// `08T15` and `928Z` would otherwise fail the atom shape. The timestamp must
// end before a non-alphanumeric, so that a fraction or offset cannot take the
// digits of a following `-01a2b3c4` UUID group.
static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9]{4}-[0-9]{2}-[0-9]{2}[Tt][0-9]{2}(?:[:\-][0-9]{2}){1,2}(?:[.,\-][0-9]+)?(?:[Zz]|[+\-][0-9]{2}(?::?[0-9]{2})?)?(?P<end>[^A-Za-z0-9]|$)")
        .expect("static regex is valid")
});

/// Text that passed ingest redaction. Only this module constructs it, so
/// stored text is never raw and output never redacts it a second time.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Redacted(String);

impl Redacted {
    /// A JSON object or array is redacted member by member, anything else as text.
    pub fn new(raw: &str) -> Self {
        if let Ok(mut v) = serde_json::from_str::<Value>(raw)
            && (v.is_object() || v.is_array())
        {
            redact_value(&mut v);
            return Self(v.to_string());
        }
        Self(redact(raw))
    }

    /// One command's argv, where a credential-named flag masks the element after it.
    pub fn argv(raw: &[String]) -> Vec<Self> {
        let mut out: Vec<Self> = Vec::with_capacity(raw.len());
        for arg in raw {
            let flag = out.last().and_then(|prev| {
                prev.strip_prefix("--")
                    .or_else(|| prev.strip_prefix('-'))
                    .filter(|name| !name.contains('='))
            });
            out.push(Self(
                if flag.is_some_and(|name| classify(name) == Name::Credential) {
                    "[REDACTED]".into()
                } else {
                    redact(arg)
                },
            ));
        }
        out
    }

    /// Cuts at a UTF-8 boundary at or below `limit` and reports whether anything was cut.
    pub fn truncate(&mut self, limit: usize) -> bool {
        let end = self.0.floor_char_boundary(limit);
        let cut = end < self.0.len();
        self.0.truncate(end);
        cut
    }
}

impl Deref for Redacted {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

/// A label or identity, such as a model, tool, or session ID, that passed
/// credential patterns and named values but not the run verdict, which would
/// erase the identifiers lookups filter and counts group by, so a secret
/// disguised as a label survives. Only this module constructs it;
/// deserializing reads back the `State` this parser version wrote. Rule:
/// docs/formats.md `stored_strings`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Label(String);

impl Label {
    pub fn new(raw: &str) -> Self {
        Self(redact_metadata(raw))
    }

    /// A value cut out of text stored as `Redacted`, kept only when full
    /// redaction leaves it unchanged, so its column never holds what the
    /// stored text lost. Rule: docs/formats.md `values_from_text`.
    pub fn verbatim(raw: &str) -> Option<Self> {
        let redacted = redact(raw);
        (redacted == raw).then_some(Self(redacted))
    }
}

impl Deref for Label {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Name {
    Credential,
    Identifier,
    Ordinary,
}

// Rule: docs/formats.md `credential_name`.
fn classify(name: &str) -> Name {
    let b = name.as_bytes();
    let mut words: Vec<&str> = Vec::new();
    let mut start = None;
    for i in 0..=b.len() {
        let alnum = b.get(i).is_some_and(u8::is_ascii_alphanumeric);
        if let Some(s) = start {
            // An open word means `b[i - 1]` is alphanumeric. An uppercase letter
            // after anything else but uppercase starts a word, as in `sessionId`,
            // and so does the last capital of an acronym, as in `APIKey`.
            let hump = alnum
                && b[i].is_ascii_uppercase()
                && (!b[i - 1].is_ascii_uppercase()
                    || b.get(i + 1).is_some_and(u8::is_ascii_lowercase));
            if !alnum || hump {
                words.push(&name[s..i]);
                start = None;
            }
        }
        if alnum && start.is_none() {
            start = Some(i);
        }
    }
    // A trailing word that names the form of a value, as in `api_key_value` or
    // `password_confirmation`, leaves the decision to the word before it.
    while words.last().is_some_and(|last| {
        [
            "value",
            "values",
            "confirmation",
            "plain",
            "plaintext",
            "raw",
            "encoded",
            "b64",
            "base64",
            "hex",
        ]
        .iter()
        .any(|w| last.eq_ignore_ascii_case(w))
    }) {
        words.pop();
    }
    let Some(&last) = words.last() else {
        return Name::Ordinary;
    };
    let is = |w: &str| last.eq_ignore_ascii_case(w);
    let ends =
        |w: &str| last.len() >= w.len() && last[last.len() - w.len()..].eq_ignore_ascii_case(w);
    let qualified_key = || {
        words.len() == 1
            || [
                "api",
                "private",
                "secret",
                "access",
                "client",
                "signing",
                "encryption",
                "master",
            ]
            .iter()
            .any(|w| words[words.len() - 2].eq_ignore_ascii_case(w))
    };
    if [
        "token",
        "secret",
        "secrets",
        "password",
        "passwords",
        "passwd",
        "passphrase",
        "credential",
        "credentials",
        "authorization",
        "apikey",
        "privatekey",
    ]
    .iter()
    .any(|w| ends(w))
        || [
            "auth",
            "sig",
            "signature",
            "bearer",
            "cookie",
            "cookies",
            "sessionid",
            "phpsessid",
        ]
        .iter()
        .any(|w| is(w))
        || is("key") && qualified_key()
    {
        Name::Credential
    } else if ["id", "ids", "uuid"].iter().any(|w| is(w)) {
        Name::Identifier
    } else {
        Name::Ordinary
    }
}

pub fn redact_value(v: &mut Value) {
    match v {
        Value::String(s) => *s = redact(s),
        Value::Array(a) => a.iter_mut().for_each(redact_value),
        Value::Object(m) => {
            let old = std::mem::take(m);
            let mut next_suffix: HashMap<String, usize> = HashMap::new();
            for (key, mut value) in old {
                match classify(&key) {
                    Name::Credential => value = Value::String("[REDACTED]".into()),
                    Name::Identifier => identifier_value(&mut value),
                    Name::Ordinary => redact_value(&mut value),
                }
                // Rule: docs/formats.md `json_key_collisions`.
                let base = redact_metadata(&key);
                let mut key = base.clone();
                if m.contains_key(&key) {
                    let n = next_suffix.entry(base.clone()).or_insert(1);
                    while m.contains_key(&key) {
                        *n += 1;
                        key = format!("{base}#{n}");
                    }
                }
                m.insert(key, value);
            }
        }
        _ => {}
    }
}

// Rule: docs/formats.md `identifier_values`.
fn identifier_value(v: &mut Value) {
    match v {
        Value::String(s) => *s = redact_metadata(s),
        Value::Array(a) => a.iter_mut().for_each(identifier_value),
        _ => redact_value(v),
    }
}

/// Credential patterns and named values, then the verdict on each remaining run.
pub fn redact(text: &str) -> String {
    redact_named(text, true)
}

/// Credential patterns and named values only, for labels and identities that
/// the run verdict must not touch.
fn redact_metadata(text: &str) -> String {
    redact_named(text, false)
}

/// The characters a run is made of; a cut inside a run can leave part of a secret.
pub fn is_run_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_+/=.!@$%:&?-".contains(c)
}

// Rule: docs/formats.md `credential_assignments`.
fn redact_named(text: &str, judge_runs: bool) -> String {
    let mut out = text.to_owned();
    // A pattern that must see the character after its token captures it as `end`,
    // which the replacement keeps; the regex crate has no lookahead.
    for pattern in PATTERNS.iter() {
        out = pattern.replace_all(&out, "[REDACTED]${end}").into_owned();
    }
    let runs = |s: &str| {
        if judge_runs {
            redact_runs(s)
        } else {
            s.to_owned()
        }
    };
    let mut redacted = String::with_capacity(out.len());
    let mut offset = 0;
    let mut exempt_end = 0;
    let mut unquoted_end = 0;
    for caps in ASSIGNMENTS.captures_iter(&out) {
        let assignment = caps.get_match();
        let header = caps.name("header").is_some();
        let name = caps
            .name("name")
            .or_else(|| caps.name("flag"))
            .map_or("", |m| m.as_str());
        let class = if header {
            Name::Credential
        } else {
            classify(name)
        };
        if assignment.start() < offset
            || class == Name::Ordinary
            || (class == Name::Identifier && assignment.end() < exempt_end)
        {
            continue;
        }
        let tail = &out[assignment.end()..];
        let quote = if tail.starts_with(['"', '\'']) {
            &tail[..1]
        } else if tail.starts_with(r#"\""#) || tail.starts_with(r"\'") {
            &tail[..2]
        } else {
            ""
        };
        let value_end = if header {
            let line_end = assignment.end() + tail.find(['\r', '\n']).unwrap_or(tail.len());
            if let Some(quote) = caps.name("quote") {
                let value = ASSIGNED_VALUE
                    .find(&out[quote.start()..line_end])
                    .expect("header starts with a quote");
                let end = quote.start() + value.end();
                if out[assignment.end()..end].ends_with(quote.as_str()) {
                    end - quote.len()
                } else {
                    end
                }
            } else {
                line_end
            }
        } else if assignment.end() < unquoted_end {
            unquoted_end
        } else {
            let Some(value) = ASSIGNED_VALUE.find(&out[assignment.end()..]) else {
                continue;
            };
            let mut end = assignment.end() + value.end();
            if quote.is_empty() && out[..end].ends_with('\\') && out[end..].starts_with(['"', '\''])
            {
                end -= 1;
            }
            if quote.is_empty() {
                unquoted_end = end;
            }
            end
        };
        let mut value = &out[assignment.end()..value_end];
        if !header
            && quote.is_empty()
            && out[..assignment.start()].ends_with(['?', '&'])
            && let Some(pair) = QUERY_PAIR.find(value)
        {
            value = &value[..pair.start()];
        }
        let protected_end = exempt_end.clamp(offset, assignment.end());
        redacted.push_str(&out[offset..protected_end]);
        redacted.push_str(&runs(&out[protected_end..assignment.end()]));
        offset = assignment.end();
        if class == Name::Identifier {
            // Rule: docs/formats.md `identifier_values`.
            exempt_end = offset + value.len();
        } else {
            if !header {
                redacted.push_str(quote);
            }
            redacted.push_str("[REDACTED]");
            if !header && value.len() > quote.len() && value.ends_with(quote) {
                redacted.push_str(quote);
            }
            offset += value.len();
        }
    }
    let protected_end = exempt_end.max(offset);
    redacted.push_str(&out[offset..protected_end]);
    redacted.push_str(&runs(&out[protected_end..]));
    redacted
}

fn redact_runs(text: &str) -> String {
    RUNS.replace_all(text, |caps: &regex::Captures<'_>| {
        let run = &caps[0];
        let identifier = run.trim_end_matches(['.', '!', '?', ':']);
        if URL.is_match(identifier) {
            return redact_url(run);
        }
        if EMAIL.is_match(identifier) || absolute_path(identifier) {
            return run.to_owned();
        }
        redact_run(run)
    })
    .into_owned()
}

// Rule: docs/formats.md `path_locator`.
fn absolute_path(identifier: &str) -> bool {
    let mut location = identifier;
    for _ in 0..2 {
        if let Some((path, number)) = location.rsplit_once(':')
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
        {
            location = path;
        } else {
            break;
        }
    }
    let location = location.strip_suffix('/').unwrap_or(location);
    location.starts_with('/')
        && location.split('/').filter(|s| !s.is_empty()).count() >= 2
        && location.split('/').skip(1).all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.~".contains(&b))
        })
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
    if identifier_atoms(run.trim_end_matches(['.', '!', '?', ':'])) {
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

// Rule: docs/formats.md `identifier_atoms`.
fn identifier_atoms(run: &str) -> bool {
    let run = TIMESTAMP.replace_all(run, "t${end}");
    let mut letter = false;
    let shaped = run.split(['/', '.', '-', '@', ':', '=', '_']).all(|atom| {
        let atom = atom
            .strip_prefix("0x")
            .or_else(|| atom.strip_prefix("0X"))
            .unwrap_or(atom);
        letter |= atom.bytes().any(|b| b.is_ascii_alphabetic());
        atom.trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .bytes()
            .all(|b| b.is_ascii_digit())
            || atom.bytes().all(|b| b.is_ascii_hexdigit())
    });
    shaped && letter
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
