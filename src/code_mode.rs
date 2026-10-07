use crate::{
    normalize::is_shell_tool,
    shell::{self, Site},
};

enum Token {
    Word(String),
    Literal(Option<String>),
    Symbol(char),
    Regex,
}

impl Token {
    fn name(&self) -> Option<&str> {
        match self {
            Self::Word(s) | Self::Literal(Some(s)) => Some(s),
            _ => None,
        }
    }
    fn symbol(&self, c: char) -> bool {
        matches!(self, Self::Symbol(s) if *s == c)
    }
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

impl Lexer {
    fn next(&mut self, regex_allowed: bool, depth: usize) -> Result<Option<Token>, ()> {
        if depth > 64 {
            return Err(());
        }
        loop {
            let Some(&c) = self.chars.get(self.pos) else {
                return Ok(None);
            };
            if c.is_whitespace() {
                self.pos += 1;
                continue;
            }
            if c == '/' && self.chars.get(self.pos + 1) == Some(&'/') {
                while self
                    .chars
                    .get(self.pos)
                    .is_some_and(|c| !matches!(c, '\n' | '\r'))
                {
                    self.pos += 1;
                }
                continue;
            }
            if c == '/' && self.chars.get(self.pos + 1) == Some(&'*') {
                self.pos += 2;
                while !self
                    .chars
                    .get(self.pos..self.pos + 2)
                    .is_some_and(|s| s == ['*', '/'])
                {
                    if self.pos >= self.chars.len() {
                        return Err(());
                    }
                    self.pos += 1;
                }
                self.pos += 2;
                continue;
            }
            break;
        }
        let c = self.chars[self.pos];
        if matches!(c, '\'' | '"' | '`') {
            return Ok(Some(Token::Literal(self.literal(depth + 1)?)));
        }
        if c == '/' && regex_allowed {
            self.pos += 1;
            let mut class = false;
            loop {
                let c = *self.chars.get(self.pos).ok_or(())?;
                self.pos += 1;
                match c {
                    '\\' => {
                        self.pos += 1;
                    }
                    '[' => class = true,
                    ']' => class = false,
                    '/' if !class => break,
                    '\n' | '\r' => return Err(()),
                    _ => {}
                }
            }
            while self
                .chars
                .get(self.pos)
                .is_some_and(|c| c.is_ascii_alphabetic())
            {
                self.pos += 1;
            }
            return Ok(Some(Token::Regex));
        }
        if c.is_alphanumeric() || matches!(c, '_' | '$') {
            let start = self.pos;
            while self
                .chars
                .get(self.pos)
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
            {
                self.pos += 1;
            }
            return Ok(Some(Token::Word(
                self.chars[start..self.pos].iter().collect(),
            )));
        }
        self.pos += 1;
        Ok(Some(Token::Symbol(c)))
    }

    fn literal(&mut self, depth: usize) -> Result<Option<String>, ()> {
        if depth > 64 {
            return Err(());
        }
        let quote = self.chars[self.pos];
        self.pos += 1;
        let mut units = Vec::new();
        let mut exact = true;
        loop {
            let c = *self.chars.get(self.pos).ok_or(())?;
            self.pos += 1;
            if c == quote {
                break;
            }
            if quote == '`' && c == '$' && self.chars.get(self.pos) == Some(&'{') {
                exact = false;
                self.pos += 1;
                let mut braces = 1;
                let mut regex_allowed = true;
                while braces > 0 {
                    let t = self.next(regex_allowed, depth + 1)?.ok_or(())?;
                    if t.symbol('{') {
                        braces += 1;
                    }
                    if t.symbol('}') {
                        braces -= 1;
                    }
                    if braces > 64 {
                        return Err(());
                    }
                    regex_allowed = starts_expression(&t);
                }
                continue;
            }
            if c == '\\' {
                let escape = *self.chars.get(self.pos).ok_or(())?;
                self.pos += 1;
                let decoded = match escape {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'v' => '\u{b}',
                    '\n' | '\u{2028}' | '\u{2029}' => continue,
                    '\r' => {
                        if self.chars.get(self.pos) == Some(&'\n') {
                            self.pos += 1;
                        }
                        continue;
                    }
                    '0' if !self.chars.get(self.pos).is_some_and(|c| c.is_ascii_digit()) => '\0',
                    '0'..='9' => {
                        exact = false;
                        continue;
                    }
                    'x' | 'u' => {
                        if escape == 'u' && self.chars.get(self.pos) == Some(&'{') {
                            self.pos += 1;
                            let start = self.pos;
                            while self
                                .chars
                                .get(self.pos)
                                .is_some_and(|c| c.is_ascii_hexdigit())
                            {
                                self.pos += 1;
                            }
                            let digits = self.pos - start;
                            if self.chars.get(self.pos) != Some(&'}') || !(1..=6).contains(&digits)
                            {
                                exact = false;
                                continue;
                            }
                            let n = u32::from_str_radix(
                                &self.chars[start..self.pos].iter().collect::<String>(),
                                16,
                            )
                            .map_err(|_| ())?;
                            self.pos += 1;
                            if n <= u16::MAX as u32 {
                                units.push(n as u16);
                            } else if let Some(c) = char::from_u32(n) {
                                units.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
                            } else {
                                exact = false;
                            }
                        } else {
                            let n = if escape == 'x' { 2 } else { 4 };
                            if let Some(digits) = self
                                .chars
                                .get(self.pos..self.pos + n)
                                .filter(|s| s.iter().all(|c| c.is_ascii_hexdigit()))
                            {
                                units.push(
                                    u16::from_str_radix(&digits.iter().collect::<String>(), 16)
                                        .map_err(|_| ())?,
                                );
                                self.pos += n;
                            } else {
                                exact = false;
                            }
                        }
                        continue;
                    }
                    other => other,
                };
                units.extend(decoded.encode_utf16(&mut [0; 2]).iter().copied());
            } else {
                if matches!(c, '\n' | '\r') && quote != '`' {
                    exact = false;
                }
                let c = if c == '\r' && quote == '`' {
                    if self.chars.get(self.pos) == Some(&'\n') {
                        self.pos += 1;
                    }
                    '\n'
                } else {
                    c
                };
                units.extend(c.encode_utf16(&mut [0; 2]).iter().copied());
            }
        }
        Ok(if exact {
            String::from_utf16(&units).ok()
        } else {
            None
        })
    }
}

fn starts_expression(t: &Token) -> bool {
    matches!(
        t,
        Token::Symbol('=' | '(' | '[' | '{' | ',' | ':' | ';' | '!' | '?' | '&' | '|')
    ) || matches!(t, Token::Word(s) if matches!(s.as_str(), "return" | "throw" | "yield" | "await"))
}

fn command(tokens: &[Token]) -> Option<String> {
    if !tokens.first()?.symbol('{') {
        return None;
    }
    let mut stack = Vec::new();
    let mut fields = Vec::new();
    let mut start = 1;
    let mut end = None;
    for (i, t) in tokens.iter().enumerate() {
        if let Token::Symbol(c) = t {
            match c {
                '{' | '(' | '[' => {
                    stack.push(*c);
                    if stack.len() > 64 {
                        return None;
                    }
                }
                '}' | ')' | ']' => {
                    let open = stack.pop()?;
                    if !matches!((open, c), ('{', '}') | ('(', ')') | ('[', ']')) {
                        return None;
                    }
                    if stack.is_empty() {
                        fields.push(&tokens[start..i]);
                        end = Some(i);
                        break;
                    }
                }
                ',' if stack.len() == 1 => {
                    fields.push(&tokens[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    let end = end?;
    if !tokens.get(end + 1)?.symbol(')')
        && !(tokens.get(end + 1)?.symbol(',') && tokens.get(end + 2)?.symbol(')'))
    {
        return None;
    }
    let mut cmd = None;
    for field in fields.into_iter().filter(|f| !f.is_empty()) {
        let name = field[0].name()?;
        if name == "cmd" {
            if cmd.is_some() || field.len() != 3 || !field[1].symbol(':') {
                return None;
            }
            let Token::Literal(Some(value)) = &field[2] else {
                return None;
            };
            cmd = Some(value.clone());
        } else if field.len() != 1 && !field.get(1)?.symbol(':') {
            return None;
        }
    }
    cmd
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
    let mut lexer = Lexer {
        chars: source.chars().collect(),
        pos: 0,
    };
    let mut tokens = Vec::new();
    let mut regex_allowed = true;
    loop {
        match lexer.next(regex_allowed, 0) {
            Ok(Some(t)) => {
                regex_allowed = starts_expression(&t);
                tokens.push(t);
            }
            Ok(None) => break,
            Err(()) => {
                return if source.contains("exec_command") || source.contains("shell_command") {
                    vec![opaque()]
                } else {
                    Vec::new()
                };
            }
        }
    }
    let mut out = Vec::new();
    let mut unparsed = false;
    for i in 0..tokens.len() {
        if tokens[i].name() != Some("tools")
            || !matches!(&tokens[i], Token::Word(_))
            || !tokens.get(i + 1).is_some_and(|t| t.symbol('.'))
        {
            continue;
        }
        let Some(name) = tokens
            .get(i + 2)
            .and_then(Token::name)
            .filter(|t| is_shell_tool(t))
        else {
            continue;
        };
        if i > 0 && tokens[i - 1].symbol('.') || !matches!(tokens[i + 2], Token::Word(_)) {
            unparsed = true;
            continue;
        }
        if name == "exec_command" && tokens.get(i + 3).is_some_and(|t| t.symbol('(')) {
            if let Some(cmd) = command(&tokens[i + 4..]) {
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
        } else {
            unparsed = true;
        }
    }
    if unparsed {
        out.push(opaque());
    }
    out
}
