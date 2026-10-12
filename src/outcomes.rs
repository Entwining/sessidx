use crate::{
    model::{Agent, Event},
    normalize::text,
};
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
pub fn classify(e: &mut Event, output: &Value, agent: Agent) {
    if agent != Agent::Codex {
        flag_denials(output, agent, e, &mut true);
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
        denials(s, agent, e, true);
    }
    failed |= !e.denials.is_empty() || codes.iter().any(|n| *n != 0);
    // Mixed shell/MCP outcomes: tests/fixtures/outcomes.jsonl.
    if failed || e.ok_source == "none" && !codes.is_empty() {
        e.ok = Some(!failed);
        e.ok_source = "text";
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
    let depth = depth + 1;
    match v {
        Value::Array(a) => {
            for v in a {
                structured(v, texts, e, depth);
            }
        }
        Value::Object(m) => {
            if let Some(error) = m.get("isError").and_then(Value::as_bool) {
                e.ok = Some(!error && e.ok != Some(false));
                e.ok_source = "text";
            }
            // Codex Promise outcomes and shell transports: tests/fixtures/outcomes.jsonl.
            if let Some(status) = m.get("status").and_then(Value::as_str) {
                if status == "rejected" {
                    let reason = m
                        .get("reason")
                        .or_else(|| m.get("error"))
                        .map(|r| r.get("message").map(text).unwrap_or_else(|| text(r)))
                        .unwrap_or_default();
                    e.ok = Some(false);
                    e.ok_source = "text";
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
                    structured(v, texts, e, depth);
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
                    structured(v, texts, e, depth);
                }
                return;
            }
            // Codex {i,result}/{index,result} envelopes: tests/fixtures/outcomes.jsonl.
            for key in ["content", "results", "items", "result"] {
                if let Some(v) = m.get(key) {
                    structured(v, texts, e, depth);
                }
            }
            // Labeled shell/MCP transports: tests/fixtures/envelopes.json.
            for (key, v) in m {
                if !matches!(
                    key.as_str(),
                    "content" | "results" | "items" | "result" | "output" | "stdout" | "stderr"
                ) && (v.get("exit_code").and_then(Value::as_i64).is_some()
                    && v.get("output").is_some()
                    || v.get("isError").and_then(Value::as_bool).is_some()
                        && v.get("content").is_some())
                {
                    structured(v, texts, e, depth);
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
                    structured(&v, texts, e, depth);
                    parsed = true;
                } else {
                    break;
                }
            }
            // Truncated Codex batches retain complete rejection values: tests/fixtures/truncated.jsonl.
            if truncated {
                for capture in PARTIAL_REJECTION.captures_iter(&candidate[values.byte_offset()..]) {
                    if let Ok(reason) = serde_json::from_str::<Value>(&capture[1]) {
                        structured(
                            &serde_json::json!({"status":"rejected","reason":reason}),
                            texts,
                            e,
                            depth,
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

/// Published guard sentences with their reason ID and the producer commit,
/// file and line that printed them.
pub const GUARD_REASONS: &[(&str, &str, &str)] = &[
    (
        "The agent guard cannot inspect this shell syntax.",
        "UnsupportedShellSyntax",
        "Entwining/agent-guard@3d74318573e3ee4cd9342dc359f2de7241411711:src/reasons.ts:2",
    ),
    (
        "This reads a protected macOS app-data directory.",
        "AppData",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:2",
    ),
    (
        "A scan rooted at the home directory or ~/Library reaches every app-data entry.",
        "Broad",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:3",
    ),
    (
        "This reads a credential or environment file.",
        "File",
        "Entwining/agent-guard@97b0ed550bcd997f8757bc3acb5fc47a90139221:src/reasons.ts:6",
    ),
    (
        "reading .env file contents would expose secrets.",
        "File",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:26",
    ),
    (
        "reading sensitive file contents.",
        "File",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:33",
    ),
    (
        "Credential and environment files are opaque client inputs; do not display their contents.",
        "File",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:90",
    ),
    (
        "This inline code names a credential or environment file.",
        "CodeFile",
        "Entwining/agent-guard@97b0ed550bcd997f8757bc3acb5fc47a90139221:src/reasons.ts:8",
    ),
    (
        "A recursive search that includes hidden files can read credentials.",
        "HiddenSearch",
        "Entwining/agent-guard@3d74318573e3ee4cd9342dc359f2de7241411711:src/reasons.ts:7",
    ),
    (
        "This dumps environment or shell variables, including secrets.",
        "Dump",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:5",
    ),
    (
        "dumps environment variables including secrets.",
        "Dump",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:12",
    ),
    (
        "dumps shell variables including secrets.",
        "Dump",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:19",
    ),
    (
        "Do not dump the process environment.",
        "Dump",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:68",
    ),
    (
        "Do not dump an envchain namespace.",
        "Dump",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:76",
    ),
    (
        "Do not dump exported variables.",
        "Dump",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:100",
    ),
    (
        "Do not dump shell variables.",
        "Dump",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:108",
    ),
    (
        "This prints the value of a credential variable.",
        "Variable",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:6",
    ),
    (
        "printing secret variable values.",
        "Variable",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:55",
    ),
    (
        "Do not print credential environment variables.",
        "Variable",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:96",
    ),
    (
        "Do not print credential shell variables.",
        "Variable",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:114",
    ),
    (
        "This prints a Git hosting token.",
        "Token",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:7",
    ),
    (
        "gh auth token prints GitHub credentials.",
        "Token",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:50",
    ),
    (
        "Do not print Git hosting credentials.",
        "Token",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:122",
    ),
    (
        "Token-display flags are not allowed in gh auth status.",
        "Token",
        "liby/dotfiles@e0bc28ae5873e672eb4123e5588ceac5276717bc:dot_claude/hooks/executable_pre-tool-guard.py:21",
    ),
    (
        "This extracts a password from the macOS Keychain.",
        "Keychain",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:8",
    ),
    (
        "Do not extract credentials from the macOS Keychain.",
        "Keychain",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:136",
    ),
    (
        "This prints a stored secret or access token.",
        "StoredSecret",
        "Entwining/agent-guard@7500ceae31a2db76850056ed7c351b946d08acf2:src/reasons.ts:13",
    ),
    (
        "curl verbose or trace output can print HTTP headers including Authorization.",
        "Trace",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:9",
    ),
    (
        "curl verbose prints HTTP headers including Authorization.",
        "Trace",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:45",
    ),
    (
        "Authenticated curl tracing can print credential headers.",
        "Trace",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:155",
    ),
    (
        "This sends the contents of a credential file.",
        "Upload",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:10",
    ),
    (
        "This reads private material in the named .ssh directory or its filesystem alias.",
        "Ssh",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/policy.rs:26",
    ),
    (
        "This reads private material under ~/.ssh.",
        "Ssh",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:11",
    ),
    (
        "reading SSH private key.",
        "Ssh",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:39",
    ),
    (
        "reading private material under ~/.ssh.",
        "Ssh",
        "liby/dotfiles@b61320361b40cc8c94d755d12b4ccbd085cbca9c:dot_claude/hooks/executable_pre-bash-guard-secrets.sh:29",
    ),
    (
        "Grep would search private ~/.ssh material.",
        "GrepSsh",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:12",
    ),
    (
        "Grep would search private material in the named .ssh directory or its filesystem alias.",
        "GrepSsh",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/policy.rs:27",
    ),
    (
        "Grep requires a verifiable search scope outside private ~/.ssh material.",
        "GrepSsh",
        "liby/dotfiles@e0bc28ae5873e672eb4123e5588ceac5276717bc:dot_claude/hooks/executable_pre-tool-guard.py:157",
    ),
    (
        "The agent guard could not complete its symlink check.",
        "Symlink",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:13",
    ),
    (
        "rg -r means --replace.",
        "RgReplace",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:15",
    ),
    (
        "rg is not grep: a clustered -r flag invokes --replace, which silently rewrites every match in the output while exiting 0.",
        "RgReplace",
        "liby/dotfiles@8db3bad01bdd7a1edac9b8101efbed6510790bce:dot_claude/hooks/executable_pre-bash-policy.sh:51",
    ),
    (
        "rg has no --include flag.",
        "RgInclude",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:16",
    ),
    (
        "rg regex is not grep BRE: a\\|b matches a literal pipe.",
        "RgBre",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:17",
    ),
    (
        "rg regex is not grep BRE: \\| is an escaped literal pipe, so a\\|b matches the literal text a|b and silently finds nothing.",
        "RgBre",
        "liby/dotfiles@c75e67980ce61fe2efdf18fe777edac879255e9c:dot_claude/hooks/executable_pre-bash-policy.sh:77",
    ),
    (
        "rg regex is not grep BRE: a\\|b matches literal a|b.",
        "RgBre",
        "liby/dotfiles@e0bc28ae5873e672eb4123e5588ceac5276717bc:dot_claude/hooks/executable_pre-tool-guard.py:26",
    ),
    (
        "find is harder to scope for this repository's searches.",
        "Find",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:14",
    ),
    (
        "Use fd instead of find.",
        "Find",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-policy.sh:12",
    ),
    (
        "An explicit path or wrapper bypasses the provider-aware Claude launcher.",
        "Launcher",
        "Entwining/agent-guard@8c14bc20cf5245bd00078aa2b8b81de631393d3d:src/reasons.ts:18",
    ),
    (
        "Run Claude as `envchain claude-gateway claude ...`.",
        "Launcher",
        "liby/dotfiles@9f5c2a59d298039d7628a6285cf0e15115c98e97:dot_codex/managed-hooks/executable_guard-bash:79",
    ),
    (
        "Run Claude as `claude ...`; the provider-aware launcher selects the persisted mode.",
        "Launcher",
        "liby/dotfiles@2df53d4a6affcf10c0bbc4597dabdeceb08be041:dot_codex/managed-hooks/executable_guard-bash:48",
    ),
    (
        "Do not run dev/build/start/serve commands.",
        "Server",
        "liby/dotfiles@ec3933b6ba3466bdffca26bb6fe1d53914a53292:dot_claude/hooks/executable_pre-bash-policy.sh:17",
    ),
    (
        "Do not run dev/start/serve commands.",
        "Server",
        "liby/dotfiles@ccc3b3049ec88d152d502ef7ed9dc3665db88d8b:dot_claude/hooks/executable_pre-bash-policy.sh:17",
    ),
    (
        "Do not run dev/start/serve commands, even when explicitly asked; do not retry.",
        "Server",
        "liby/dotfiles@8a87467c8f823545b1175f93b9c6962aabf399bb:dot_claude/hooks/executable_pre-bash-policy.sh:17",
    ),
    (
        "Shell nesting exceeds the supported depth of 64.",
        "NestingLimit",
        "Entwining/agent-guard@00fe9158737f92247ce3e54997ae4b7fdf066660:src/policy.rs:56",
    ),
    (
        "The agent guard cannot inspect this unsupported or unresolved operation.",
        "CoverageGap",
        "Entwining/agent-guard@00fe9158737f92247ce3e54997ae4b7fdf066660:src/policy.rs:71",
    ),
    (
        "The agent guard cannot verify the execution owner for this operation.",
        "ExecutionOwnerUnavailable",
        "Entwining/agent-guard@00fe9158737f92247ce3e54997ae4b7fdf066660:src/policy.rs:68",
    ),
    (
        "The agent guard could not complete this check before its deadline, so the call is blocked.",
        "Deadline",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/lib.rs:210",
    ),
    (
        "The agent guard could not complete this check within its resource limit, so the call is blocked.",
        "ResourceLimit",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/lib.rs:213",
    ),
    (
        "The agent guard could not complete this check, so the call is blocked.",
        "CheckError",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/lib.rs:224",
    ),
    (
        "The event is not valid UTF-8.",
        "InvalidEncoding",
        "Entwining/agent-guard@00fe9158737f92247ce3e54997ae4b7fdf066660:src/policy.rs:62",
    ),
    (
        "The event requires an absolute cwd.",
        "AbsoluteCwdRequired",
        "Entwining/agent-guard@00fe9158737f92247ce3e54997ae4b7fdf066660:src/policy.rs:59",
    ),
    (
        "The guard cannot inspect an inode-addressed /.vol path.",
        "InodeAlias",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/policy.rs:56",
    ),
    (
        "The guard cannot resolve this path through bounded or cyclic aliases.",
        "IdentityBound",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/policy.rs:62",
    ),
    (
        "This changes a protected credential, environment or SSH resource.",
        "ResourceChange",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/policy.rs:33",
    ),
    (
        "This relocates protected credential, environment or SSH content to an unprotected name.",
        "ResourceChange",
        "Entwining/agent-guard@98ddc80e6ca761ecb8f242c033f3a67a7a9998d9:src/policy/messages.rs:24",
    ),
    (
        "This command exceeds the guard's inspection budget.",
        "InspectionBudget",
        "Entwining/agent-guard@9a5334c00b42959ac5b37a05c62b6fda6942c452:src/policy.rs:59",
    ),
    (
        "This event exceeds the input byte limit.",
        "InputByteLimit",
        "Entwining/agent-guard@00fe9158737f92247ce3e54997ae4b7fdf066660:src/policy.rs:53",
    ),
];

pub fn reason_id(s: &str) -> &'static str {
    GUARD_REASONS
        .iter()
        .find(|(sentence, _, _)| s.contains(sentence))
        .map_or("unknown", |(_, id, _)| id)
}

fn starts_with_guard_reason(line: &str) -> bool {
    let reason = line.strip_prefix("Blocked: ").unwrap_or(line);
    GUARD_REASONS
        .iter()
        .any(|(sentence, _, _)| reason.starts_with(sentence))
}

fn flag_denials(v: &Value, agent: Agent, e: &mut Event, leading: &mut bool) {
    match v {
        Value::String(s) => {
            denials(s, agent, e, *leading);
            if !s.trim().is_empty() {
                *leading = false;
            }
        }
        Value::Array(a) => {
            for v in a {
                flag_denials(v, agent, e, leading);
            }
        }
        Value::Object(m) => {
            if let Some(v) = m.get("text").or_else(|| m.get("content")) {
                flag_denials(v, agent, e, leading);
            }
        }
        _ => {}
    }
}

fn denials(s: &str, agent: Agent, e: &mut Event, leading: bool) {
    // Native success and hook-check errors: tests/fixtures/hook-check.jsonl.
    if agent != Agent::Codex && e.ok == Some(true) {
        return;
    }
    if agent == Agent::Codex {
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
        } else if agent == Agent::Claude
            && e.ok == Some(false)
            && s.trim_start().starts_with(line)
            && line
                .strip_prefix("Error: ")
                .unwrap_or(line)
                .starts_with("Permission to use ")
            && line.contains("denied")
        {
            Some("native_denial")
        } else if agent == Agent::Pi
            && leading
            && e.ok == Some(false)
            && s.trim_start().starts_with(line)
            && starts_with_guard_reason(line)
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
