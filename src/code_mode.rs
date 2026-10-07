use crate::shell::{self, Site};
use regex::Regex;
use std::sync::LazyLock;

const STRING: &str = r#""(?:\\[\s\S]|[^"\\])*"|'(?:\\[\s\S]|[^'\\])*'|`(?:\\[\s\S]|[^`\\])*`"#;
const COMMENTS: &str = r"/\*[\s\S]*?\*/|//[^\r\n]*";
const REGEX: &str = r"(?:^|[=(:,;!?\[{])\s*/(?:\\[\s\S]|\[(?:\\[\s\S]|[^\]\\\r\n])*\]|[^*/\\\[\r\n])(?:\\[\s\S]|\[(?:\\[\s\S]|[^\]\\\r\n])*\]|[^/\\\[\r\n])*/[A-Za-z]*";

fn nested_object() -> String {
    format!(r#"\{{(?:[^{{}}"'`]|{STRING})*\}}"#)
}

static CODE: LazyLock<Regex> = LazyLock::new(|| {
    let object = nested_object();
    Regex::new(&format!(
        r#"(?P<call>\btools\s*\.\s*exec_command\s*\(\s*\{{(?P<body>(?:[^{{}}"'`]|{STRING}|{object})*)\}}\s*,?\s*\))|{STRING}|{COMMENTS}|{REGEX}|(?P<shell>\b(?:exec_command|shell_command)\b|\[\s*["'](?:exec_command|shell_command)["']\s*\]\s*\()"#
    ))
    .expect("static regex is valid")
});
static FIELDS: LazyLock<Regex> = LazyLock::new(|| {
    let object = nested_object();
    let array = format!(r#"\[(?:[^\[\]"'`]|{STRING})*\]"#);
    Regex::new(&format!(
        r#"(?P<key>(?:^|,)\s*(?:cmd|"cmd"|'cmd')\s*:)|(?P<override>(?:^|,)\s*(?:(?:{COMMENTS})\s*)*(?:\.\.\.|\[))|{object}|{array}|{STRING}|{COMMENTS}|{REGEX}"#
    ))
    .expect("static regex is valid")
});
static LITERAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(STRING).expect("static regex is valid"));

fn command(body: &str) -> Option<&str> {
    let mut cmd = None;
    for caps in FIELDS.captures_iter(body) {
        if cmd.is_some() && caps.name("override").is_some() {
            return None;
        }
        if let Some(key) = caps.name("key") {
            if cmd.is_some() {
                return None;
            }
            let rest = body[key.end()..].trim_start();
            let value = LITERAL.find(rest).filter(|m| m.start() == 0)?;
            let tail = rest[value.end()..].trim_start();
            if !tail.is_empty() && !tail.starts_with(',') {
                return None;
            }
            cmd = Some(value.as_str());
        }
    }
    cmd
}

fn literal(raw: &str) -> Option<String> {
    let quote = raw.as_bytes()[0];
    let mut chars = raw[1..raw.len() - 1].chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if quote == b'`' && c == '$' && chars.peek() == Some(&'{') {
            return None;
        }
        let decoded = if c == '\\' {
            match chars.next()? {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'v' => '\u{b}',
                '\n' | '\u{2028}' | '\u{2029}' => continue,
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    continue;
                }
                '0' if !chars.peek().is_some_and(|c| c.is_ascii_digit()) => '\0',
                '0'..='9' => return None,
                escape @ ('x' | 'u') => {
                    let mut digits = String::new();
                    for _ in 0..if escape == 'x' { 2 } else { 4 } {
                        let digit = chars.next().filter(|c| c.is_ascii_hexdigit())?;
                        digits.push(digit);
                    }
                    out.push(char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?);
                    continue;
                }
                other => other,
            }
        } else {
            if matches!(c, '\n' | '\r') && quote != b'`' {
                return None;
            }
            if c == '\r' {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                '\n'
            } else {
                c
            }
        };
        out.push(decoded);
    }
    Some(out)
}

// Codex code-mode cmd literals and opaque expressions: testdata/code-mode.json.
pub fn sites(source: &str) -> Vec<Site> {
    let opaque = || Site {
        program: None,
        argv: Vec::new(),
        parsed: false,
    };
    if source.len() > 256 * 1024 {
        return vec![opaque()];
    }
    let mut out = Vec::new();
    let mut referenced = false;
    let mut unparsed = false;
    for caps in CODE.captures_iter(source) {
        if let Some(call) = caps.name("call") {
            referenced = true;
            if source[..call.start()].trim_end().ends_with('.') {
                unparsed = true;
                continue;
            }
            if let Some(cmd) = command(&caps["body"]).and_then(literal) {
                for site in shell::sites(&cmd) {
                    if site.parsed {
                        out.push(site);
                    } else {
                        unparsed = true;
                    }
                }
            } else {
                unparsed = true;
            }
        } else if caps.name("shell").is_some() {
            referenced = true;
            unparsed = true;
        }
    }
    if unparsed || referenced && out.is_empty() {
        out.push(opaque());
    }
    out
}
