use brush_parser::{
    ast::*,
    word::{WordPiece, WordPieceWithSource},
};

#[derive(Clone, Debug)]
pub struct Site {
    pub program: Option<String>,
    pub argv: Vec<String>,
    pub parsed: bool,
}

pub fn sites(source: &str) -> Vec<Site> {
    let mut out = Vec::new();
    if source.len() > 256 * 1024 || program(source, &mut out, 0).is_err() {
        return vec![Site {
            program: None,
            argv: Vec::new(),
            parsed: false,
        }];
    }
    out
}

fn program(source: &str, out: &mut Vec<Site>, depth: usize) -> Result<(), ()> {
    if depth > 64 {
        return Err(());
    }
    let parsed = brush_parser::Parser::builder()
        .build(std::io::Cursor::new(source.as_bytes()))
        .parse_program()
        .map_err(|_| ())?;
    for list in parsed.complete_commands {
        walk_list(&list, out, depth + 1)?;
    }
    Ok(())
}

fn walk_list(list: &CompoundList, out: &mut Vec<Site>, depth: usize) -> Result<(), ()> {
    if depth > 64 {
        return Err(());
    }
    for item in &list.0 {
        for pipeline in
            std::iter::once(&item.0.first).chain(item.0.additional.iter().map(|x| match x {
                AndOr::And(p) | AndOr::Or(p) => p,
            }))
        {
            for command in &pipeline.seq {
                walk_command(command, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn words(pieces: &[WordPieceWithSource], out: &mut Vec<Site>, depth: usize) -> Result<(), ()> {
    for piece in pieces {
        match &piece.piece {
            WordPiece::CommandSubstitution(s) | WordPiece::BackquotedCommandSubstitution(s) => {
                program(s, out, depth + 1)?
            }
            WordPiece::DoubleQuotedSequence(p) | WordPiece::GettextDoubleQuotedSequence(p) => {
                words(p, out, depth + 1)?
            }
            _ => {}
        }
    }
    Ok(())
}

fn word(word: &Word, out: &mut Vec<Site>, depth: usize) -> Result<(), ()> {
    let pieces = brush_parser::word::parse(&word.value, &brush_parser::ParserOptions::default())
        .map_err(|_| ())?;
    words(&pieces, out, depth)
}

fn item(item: &CommandPrefixOrSuffixItem, out: &mut Vec<Site>, depth: usize) -> Result<(), ()> {
    match item {
        CommandPrefixOrSuffixItem::Word(w) | CommandPrefixOrSuffixItem::AssignmentWord(_, w) => {
            word(w, out, depth)
        }
        CommandPrefixOrSuffixItem::ProcessSubstitution(_, s) => walk_list(&s.list, out, depth),
        CommandPrefixOrSuffixItem::IoRedirect(r) => redirect(r, out, depth),
    }
}

fn redirect(r: &IoRedirect, out: &mut Vec<Site>, depth: usize) -> Result<(), ()> {
    match r {
        IoRedirect::File(_, _, IoFileRedirectTarget::ProcessSubstitution(_, s)) => {
            walk_list(&s.list, out, depth)
        }
        IoRedirect::File(
            _,
            _,
            IoFileRedirectTarget::Filename(w) | IoFileRedirectTarget::Duplicate(w),
        )
        | IoRedirect::HereString(_, w)
        | IoRedirect::OutputAndError(w, _) => word(w, out, depth),
        IoRedirect::HereDocument(_, d) if d.requires_expansion => {
            let pieces = brush_parser::word::parse_heredoc(
                &d.doc.value,
                &brush_parser::ParserOptions::default(),
            )
            .map_err(|_| ())?;
            words(&pieces, out, depth)
        }
        _ => Ok(()),
    }
}

fn walk_command(c: &Command, out: &mut Vec<Site>, depth: usize) -> Result<(), ()> {
    if depth > 64 {
        return Err(());
    }
    match c {
        Command::Simple(s) => {
            let mut argv = Vec::new();
            if let Some(w) = &s.word_or_name {
                word(w, out, depth)?;
                argv.push(w.value.clone());
            }
            for i in s
                .prefix
                .iter()
                .flat_map(|p| &p.0)
                .chain(s.suffix.iter().flat_map(|p| &p.0))
            {
                item(i, out, depth)?;
                if let CommandPrefixOrSuffixItem::Word(w) = i {
                    argv.push(w.value.clone());
                }
            }
            if let Some(first) = argv.first() {
                let literal = first.chars().all(|c| !matches!(c, '$' | '`'));
                let name = brush_parser::unquote_str(first);
                let program = if literal {
                    Some(name.rsplit('/').next().unwrap_or(&name).into())
                } else {
                    None
                };
                out.push(Site {
                    program,
                    argv,
                    parsed: true,
                });
            }
        }
        Command::Compound(c, r) => {
            if let Some(r) = r {
                for r in &r.0 {
                    redirect(r, out, depth)?;
                }
            }
            compound(c, out, depth)?;
        }
        Command::Function(f) => compound(&f.body.0, out, depth)?,
        Command::ExtendedTest(_, _) => {}
    }
    Ok(())
}

fn compound(c: &CompoundCommand, out: &mut Vec<Site>, depth: usize) -> Result<(), ()> {
    match c {
        CompoundCommand::BraceGroup(g) => walk_list(&g.list, out, depth),
        CompoundCommand::Subshell(g) => walk_list(&g.list, out, depth),
        CompoundCommand::ForClause(g) => {
            for w in g.values.iter().flatten() {
                word(w, out, depth)?;
            }
            walk_list(&g.body.list, out, depth)
        }
        CompoundCommand::ArithmeticForClause(g) => walk_list(&g.body.list, out, depth),
        CompoundCommand::IfClause(g) => {
            walk_list(&g.condition, out, depth)?;
            walk_list(&g.then, out, depth)?;
            for e in g.elses.iter().flatten() {
                if let Some(c) = &e.condition {
                    walk_list(c, out, depth)?;
                }
                walk_list(&e.body, out, depth)?;
            }
            Ok(())
        }
        CompoundCommand::WhileClause(g) | CompoundCommand::UntilClause(g) => {
            walk_list(&g.0, out, depth)?;
            walk_list(&g.1.list, out, depth)
        }
        CompoundCommand::CaseClause(g) => {
            word(&g.value, out, depth)?;
            for c in &g.cases {
                if let Some(c) = &c.cmd {
                    walk_list(c, out, depth)?;
                }
            }
            Ok(())
        }
        CompoundCommand::Coprocess(g) => walk_command(&g.body, out, depth),
        CompoundCommand::Arithmetic(_) => Ok(()),
    }
}
