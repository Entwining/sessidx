use crate::{model::Event, normalize::text};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

static EXIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(?:Process exited with code|Exit code:)\s*(-?\d+)\s*$")
        .expect("static regex is valid")
});
static PARTIAL_REJECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""status"\s*:\s*"rejected"\s*,\s*"reason"\s*:\s*("(?:\\.|[^"\\])*")"#)
        .expect("static regex is valid")
});
pub fn classify(e: &mut Event, output: &Value, harness: &str) {
    if harness != "codex" {
        flag_denials(output, harness, e, &mut true);
        return;
    }
    let mut texts = Vec::new();
    structured(output, &mut texts, e, 0);
    let mut codes = Vec::new();
    let mut failed = e.ok == Some(false);
    for s in &texts {
        for capture in EXIT.captures_iter(s) {
            if let Ok(code) = capture[1].parse::<i64>() {
                codes.push(code);
            }
        }
        if s.trim_start().starts_with("Script failed")
            || s.trim_start().starts_with("Script error:")
        {
            failed = true;
        }
        denials(s, harness, e, true);
    }
    failed |= !e.denials.is_empty() || codes.iter().any(|n| *n != 0);
    // Mixed shell/MCP outcomes: testdata/outcomes.jsonl.
    if failed || e.ok_source == "none" && !codes.is_empty() {
        e.ok = Some(!failed);
        e.ok_source = "text".into();
    }
    e.exit_code = codes
        .iter()
        .copied()
        .find(|n| *n != 0)
        .or_else(|| codes.first().copied());
}

fn structured(v: &Value, texts: &mut Vec<String>, e: &mut Event, depth: usize) {
    // String values are parsed again as JSON, so the parser's own nesting
    // limit does not bound this recursion; deeper values stay unclassified.
    if depth > 32 {
        return;
    }
    match v {
        Value::Array(a) => {
            for v in a {
                structured(v, texts, e, depth + 1);
            }
        }
        Value::Object(m) => {
            if let Some(error) = m.get("isError").and_then(Value::as_bool) {
                e.ok = Some(!error && e.ok != Some(false));
                e.ok_source = "text".into();
            }
            // Codex Promise outcomes and shell transports: testdata/outcomes.jsonl.
            if let Some(status) = m.get("status").and_then(Value::as_str) {
                if status == "rejected" {
                    let reason = m
                        .get("reason")
                        .or_else(|| m.get("error"))
                        .map(|r| r.get("message").map(text).unwrap_or_else(|| text(r)))
                        .unwrap_or_default();
                    e.ok = Some(false);
                    e.ok_source = "text".into();
                    if reason
                        .trim_start()
                        .starts_with("Command blocked by PreToolUse hook:")
                    {
                        e.denials
                            .push(("batch_hook".into(), reason_id(&reason).into()));
                    }
                } else if status == "fulfilled"
                    && let Some(v) = m.get("value")
                {
                    structured(v, texts, e, depth + 1);
                }
                return;
            }
            if let Some(n) = m.get("exit_code").and_then(Value::as_i64) {
                texts.push(format!("Exit code: {n}"));
                return;
            }
            if m.get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "input_text" || t == "text")
            {
                if let Some(v) = m.get("text") {
                    structured(v, texts, e, depth + 1);
                }
                return;
            }
            // Codex {i,result}/{index,result} envelopes: testdata/outcomes.jsonl.
            for key in ["content", "results", "items", "result"] {
                if let Some(v) = m.get(key) {
                    structured(v, texts, e, depth + 1);
                }
            }
            // Labeled shell/MCP transports: testdata/envelopes.json.
            for (key, v) in m {
                if !matches!(
                    key.as_str(),
                    "content" | "results" | "items" | "result" | "output" | "stdout" | "stderr"
                ) && (v.get("exit_code").and_then(Value::as_i64).is_some()
                    && v.get("output").is_some()
                    || v.get("isError").and_then(Value::as_bool).is_some()
                        && v.get("content").is_some())
                {
                    structured(v, texts, e, depth + 1);
                }
            }
        }
        Value::String(s) => {
            let mut candidate = s.trim();
            let truncated = candidate.starts_with("Warning: truncated output");
            if candidate.starts_with("Script completed")
                || candidate.starts_with("Script failed")
                || candidate.starts_with("Script error:")
            {
                texts.push(
                    candidate
                        .lines()
                        .take_while(|l| *l != "Output:")
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                if let Some((_, tail)) = candidate.split_once("\nOutput:\n") {
                    candidate = tail.trim();
                }
            }
            if candidate.starts_with("Warning: truncated output")
                && let Some((_, tail)) = candidate.split_once("\n\n")
            {
                candidate = tail.trim();
            }
            let mut values = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
            let mut parsed = false;
            while let Some(Ok(v)) = values.next() {
                if matches!(v, Value::Object(_) | Value::Array(_)) {
                    structured(&v, texts, e, depth + 1);
                    parsed = true;
                } else {
                    break;
                }
            }
            // Truncated Codex batches retain complete rejection values: testdata/truncated.jsonl.
            if truncated {
                for capture in PARTIAL_REJECTION.captures_iter(&candidate[values.byte_offset()..]) {
                    if let Ok(reason) = serde_json::from_str::<Value>(&capture[1]) {
                        structured(
                            &serde_json::json!({"status":"rejected","reason":reason}),
                            texts,
                            e,
                            depth + 1,
                        );
                    }
                }
            }
            if !parsed {
                texts.push(candidate.into());
            }
        }
        _ => {}
    }
}

pub fn reason_id(s: &str) -> &'static str {
    for (prefix, id) in [
        (
            "The agent guard cannot inspect this shell syntax.",
            "Syntax",
        ),
        (
            "This reads a protected macOS app-data directory.",
            "Appdata",
        ),
        ("A scan rooted at the home directory or ~/Library", "Broad"),
        ("This reads a credential or environment file.", "File"),
        (
            "This inline code names a credential or environment file.",
            "CodeFile",
        ),
        (
            "A recursive search that includes hidden files",
            "HiddenSearch",
        ),
        ("This dumps environment or shell variables", "Dump"),
        (
            "This prints the value of a credential variable.",
            "Variable",
        ),
        ("This prints a Git hosting token.", "Token"),
        (
            "This extracts a password from the macOS Keychain.",
            "Keychain",
        ),
        (
            "This prints a stored secret or access token.",
            "SecretPrint",
        ),
        ("curl verbose or trace output", "Trace"),
        ("This sends the contents of a credential file.", "Upload"),
        ("This reads private material under ~/.ssh.", "Ssh"),
        ("Grep would search private ~/.ssh material.", "GrepSsh"),
        (
            "The agent guard could not complete its symlink check.",
            "Symlink",
        ),
        ("rg -r means --replace.", "Replace"),
        ("rg has no --include flag.", "Include"),
        ("rg regex is not grep BRE:", "Bre"),
    ] {
        if s.contains(prefix) {
            return id;
        }
    }
    "unknown"
}

fn flag_denials(v: &Value, harness: &str, e: &mut Event, leading: &mut bool) {
    match v {
        Value::String(s) => {
            denials(s, harness, e, *leading);
            if !s.trim().is_empty() {
                *leading = false;
            }
        }
        Value::Array(a) => {
            for v in a {
                flag_denials(v, harness, e, leading);
            }
        }
        Value::Object(m) => {
            if let Some(v) = m.get("text").or_else(|| m.get("content")) {
                flag_denials(v, harness, e, leading);
            }
        }
        _ => {}
    }
}

fn denials(s: &str, harness: &str, e: &mut Event, leading: bool) {
    // Native success and hook-check errors: testdata/hook-check.jsonl.
    if harness != "codex" && e.ok == Some(true) {
        return;
    }
    if harness == "codex" {
        let Some(payload) = s.trim_start().strip_prefix("Script error:") else {
            return;
        };
        if payload
            .trim_start()
            .starts_with("Command blocked by PreToolUse hook:")
        {
            let denial = ("hook".into(), reason_id(payload).into());
            if !e.denials.contains(&denial) {
                e.denials.push(denial);
            }
        }
        return;
    }
    for line in s.lines() {
        let line = line.trim();
        let source = if line.starts_with("Script error: Command blocked by PreToolUse hook:")
            || (line.starts_with("PreToolUse:") || line.starts_with("Error: PreToolUse:"))
                && (line.contains("DENIED:") || e.ok == Some(false) && line.contains("hook error:"))
        {
            Some("hook")
        } else if e.ok == Some(false)
            && (line.starts_with("DENIED:") || line.starts_with("Blocked by agent-guard:"))
        {
            Some("guard")
        } else if harness == "claude"
            && e.ok == Some(false)
            && s.trim_start().starts_with(line)
            && line
                .strip_prefix("Error: ")
                .unwrap_or(line)
                .starts_with("Permission to use ")
            && line.contains("denied")
        {
            Some("native_denial")
        } else if harness == "pi"
            && leading
            && e.ok == Some(false)
            && s.trim_start().starts_with(line)
            && reason_id(line) != "unknown"
            && !line.contains("\"")
        {
            Some("guard")
        } else {
            None
        };
        if let Some(source) = source {
            let denial = (source.into(), reason_id(line).into());
            if !e.denials.contains(&denial) {
                e.denials.push(denial);
            }
        }
    }
}
