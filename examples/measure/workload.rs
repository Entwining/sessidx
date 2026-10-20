//! The workload: the `sessidx` invocations a scratch index recorded, turned
//! back into the arguments they passed.

use anyhow::Result;
use brush_parser::word::WordPiece;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::path::Path;

/// Verbs whose real invocations make up the workload.
const VERBS: [&str; 5] = ["search", "grep", "show", "count", "sql"];

/// Recorded invocations of one verb, most recent first, at most one per transcript.
#[derive(Default)]
pub struct Candidates {
    pub transcripts: usize,
    pub invocations: usize,
    pub replayable: Vec<Vec<String>>,
    pub unreplayable: usize,
}

/// The `sessidx` invocations the index recorded with a tool result that is not
/// a recognized denial, by verb, most recent first, at most one per transcript
/// and without repeats; of one call's commands the last in the line is the most
/// recent. Copied history counts once, and a result counts in whichever
/// transcript holds it: the writer's pairing (`call_pairing` in
/// `docs/formats.md`) can name a copy of the call other than the first.
pub fn workload(
    db: &Path,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<Vec<(&'static str, Candidates)>> {
    let window = sessidx::query::Filters {
        since,
        until,
        ..Default::default()
    };
    let db = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = db.prepare(
        "SELECT e.file_id, c.argv_json FROM commands c JOIN event_rows e ON e.id = c.event_id
         WHERE c.program = 'sessidx' AND e.kind = 'tool_call' AND e.canonical = 1
           AND (?1 IS NULL OR e.ts >= ?1) AND (?2 IS NULL OR e.ts < ?2)
           AND EXISTS (SELECT 1 FROM event_rows r WHERE r.call_id = e.call_id AND r.kind = 'tool_result'
                       AND NOT EXISTS (SELECT 1 FROM denials d WHERE d.event_id = r.id))
         ORDER BY e.ts DESC, e.id DESC, c.site DESC",
    )?;
    let rows = stmt.query_map([window.since(), window.until()], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut by_verb: Vec<(&str, Candidates)> =
        VERBS.iter().map(|v| (*v, Candidates::default())).collect();
    // Per verb, the transcripts recorded and those that supplied a command.
    let mut files = vec![(HashSet::new(), HashSet::new()); VERBS.len()];
    let mut seen = HashSet::new();
    for row in rows {
        let (file, argv) = row?;
        let argv: Vec<String> = serde_json::from_str(&argv)?;
        let Some(i) = verb(&argv[1..]) else {
            continue;
        };
        let list = &mut by_verb[i].1;
        let (recorded, taken) = &mut files[i];
        recorded.insert(file);
        list.invocations += 1;
        match replay(&argv[1..]) {
            Some(args) if !taken.contains(&file) && !seen.contains(&args) => {
                taken.insert(file);
                seen.insert(args.clone());
                list.replayable.push(args);
            }
            Some(_) => {}
            None => list.unreplayable += 1,
        }
    }
    for ((_, list), (recorded, _)) in by_verb.iter_mut().zip(files) {
        list.transcripts = recorded.len();
    }
    Ok(by_verb)
}

/// The position in `VERBS` of an invocation's verb: its first word after the
/// global options `--db` and `--root`.
fn verb(words: &[String]) -> Option<usize> {
    let mut words = words.iter();
    while let Some(w) = words.next() {
        let w = unquote(w)?;
        match w.as_str() {
            "--db" | "--root" => {
                words.next();
            }
            _ if w.starts_with("--db=") || w.starts_with("--root=") => {}
            _ => return VERBS.iter().position(|v| *v == w),
        }
    }
    None
}

/// The arguments a recorded invocation passed, or None when they cannot be
/// replayed faithfully: a word that needed expansion, text redaction replaced,
/// an option that points the command at another index or page, or a help or
/// version request, which prints no `end` record. A double-quoted backslash
/// before a newline is kept, though the shell removed both; no recorded
/// invocation has shown one.
fn replay(words: &[String]) -> Option<Vec<String>> {
    let args = words
        .iter()
        .map(|w| unquote(w))
        .collect::<Option<Vec<_>>>()?;
    let unfit = |a: &String| {
        a.contains("[REDACTED]")
            || ["--db", "--root", "--cursor"]
                .iter()
                .any(|f| a == f || a.starts_with(&format!("{f}=")))
            || ["--help", "-h", "--version", "-V"].contains(&a.as_str())
    };
    (!args.iter().any(unfit)).then_some(args)
}

fn unquote(word: &str) -> Option<String> {
    literal(
        &brush_parser::word::parse(word, &Default::default()).ok()?,
        false,
    )
}

/// The value of a word made only of literal text, quotes, and escapes, with no
/// unquoted glob or brace characters that the shell would have expanded.
fn literal(pieces: &[brush_parser::word::WordPieceWithSource], quoted: bool) -> Option<String> {
    let mut s = String::new();
    for p in pieces {
        match &p.piece {
            WordPiece::Text(t) if quoted || !t.contains(['*', '?', '[', '{']) => s.push_str(t),
            WordPiece::SingleQuotedText(t) => s.push_str(t),
            WordPiece::DoubleQuotedSequence(inner) => s.push_str(&literal(inner, true)?),
            // Inside double quotes brush-parser keeps a backslash that escapes nothing as text.
            WordPiece::EscapeSequence(e) => s.push_str(e.strip_prefix('\\')?),
            _ => return None,
        }
    }
    Some(s)
}

#[cfg(test)]
#[allow(
    clippy::too_many_lines,
    reason = "a scenario test reads as one fixture, its calls, and its assertions"
)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;

    fn words(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn recorded_words_replay_as_the_shell_passed_them() {
        assert_eq!(
            replay(&words(&[
                "search",
                r#"'"connection refused"'"#,
                "--since",
                "2026-10-01"
            ])),
            Some(words(&[
                "search",
                r#""connection refused""#,
                "--since",
                "2026-10-01"
            ]))
        );
        assert_eq!(
            replay(&words(&[
                "sql",
                r#""SELECT json_extract(x, '$.a') FROM t WHERE y = \"it's\"""#
            ])),
            Some(words(&[
                "sql",
                r#"SELECT json_extract(x, '$.a') FROM t WHERE y = "it's""#
            ]))
        );
        assert_eq!(
            replay(&words(&["grep", r"a\ b", r#""c\d""#, r#""a.*[b]{2}?""#])),
            Some(words(&["grep", "a b", r"c\d", "a.*[b]{2}?"]))
        );
    }

    #[test]
    fn invocations_that_cannot_be_replayed_faithfully_are_refused() {
        for ws in [
            &["show", "\"$SID\""][..],
            &["search", "$(cat q)"],
            &["grep", "`x`", "--since", "2026-10-01"],
            &["show", "~/log.jsonl:3"],
            &["grep", "a.*b"],
            &["grep", "why?"],
            &["grep", "[ab]"],
            &["search", "{a,b}"],
            &["sql", "'SELECT [REDACTED]'"],
            &["search", "error", "--db", "/tmp/x.db"],
            &["search", "error", "--root=claude=/tmp/x"],
            &["grep", "x", "--cursor", "abc"],
            &["count", "--help"],
            &["count", "-h"],
            &["search", "--version"],
            &["search", "-V"],
        ] {
            assert_eq!(replay(&words(ws)), None, "{ws:?}");
        }
    }

    #[test]
    fn the_verb_is_the_first_word_after_global_options() {
        assert_eq!(verb(&words(&["'search'", "x"])), Some(0));
        assert_eq!(verb(&words(&["--db", "x.db", "grep", "y"])), Some(1));
        assert_eq!(verb(&words(&["--root=claude=/x", "show", "y"])), Some(2));
        assert_eq!(verb(&words(&["--version"])), None);
        assert_eq!(verb(&words(&["index"])), None);
    }

    #[test]
    fn the_workload_is_one_recorded_invocation_per_transcript_most_recent_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("claude");
        fs::create_dir(&root).unwrap();
        let call = |s: &str, n: u32, command: &str, result: &str| {
            let ts = format!("2026-10-{n:02}T00:00:00Z");
            let mut lines = vec![
                json!({"type":"assistant","uuid":format!("{s}a{n}"),"sessionId":s,"timestamp":ts,"message":{"role":"assistant","content":[{"type":"tool_use","id":format!("{s}c{n}"),"name":"Bash","input":{"command":command}}]}}),
            ];
            match result {
                "ok" => lines.push(json!({"type":"user","uuid":format!("{s}r{n}"),"sessionId":s,"timestamp":ts,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":format!("{s}c{n}"),"content":"ok"}]}})),
                "denied" => lines.push(json!({"type":"user","uuid":format!("{s}r{n}"),"sessionId":s,"timestamp":ts,"toolDenialKind":"permission-rule","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":format!("{s}c{n}"),"is_error":true,"content":"Permission to use Bash has been denied."}]}})),
                _ => {}
            }
            lines
        };
        let records: Vec<Value> = [
            call("s", 1, "sessidx search older", "ok"),
            call("s", 2, "sessidx search unanswered", "none"),
            call("s", 3, "sessidx search newer | jq .", "ok"),
            call("s", 4, "sessidx search newer", "ok"),
            call("s", 5, "sessidx 'count' commands", "ok"),
            call("s", 6, "sessidx show \"$SID\"", "ok"),
            call("s", 7, "sessidx sql 'SELECT 1'", "ok"),
            call("s", 8, "sessidx --db x.db search elsewhere", "ok"),
            call("s", 9, "sessidx grep refused --agent claude", "denied"),
        ]
        .concat();
        let text: String = records.iter().map(|r| format!("{r}\n")).collect();
        // A resumed session repeats its history under the same native IDs.
        fs::write(root.join("a.jsonl"), &text).unwrap();
        fs::write(root.join("b.jsonl"), &text).unwrap();
        let other: String = [
            call("t", 2, "sessidx search second", "ok"),
            call("t", 3, "sessidx search newer", "ok"),
            call("t", 5, "sessidx search again --cursor abc", "ok"),
        ]
        .concat()
        .iter()
        .map(|r| format!("{r}\n"))
        .collect();
        fs::write(root.join("c.jsonl"), other).unwrap();
        let both: String = call("u", 1, "sessidx search first; sessidx search last", "ok")
            .iter()
            .map(|r| format!("{r}\n"))
            .collect();
        fs::write(root.join("d.jsonl"), both).unwrap();
        // Codex continues a thread in a second rollout that copies its history:
        // the copy in the earlier-started rollout is the first, while results
        // are paired with the copy in the rollout first by path.
        let codex = dir.path().join("codex");
        fs::create_dir(&codex).unwrap();
        let rollout = |started: &str| {
            [
                json!({"type":"session_meta","timestamp":started,"payload":{"id":"t","cwd":"/synthetic"}}),
                json!({"type":"response_item","timestamp":"2026-10-01T00:00:01Z","payload":{"type":"function_call","name":"exec_command","call_id":"call-moved","arguments":"{\"cmd\":\"sessidx search moved\"}"}}),
                json!({"type":"response_item","timestamp":"2026-10-01T00:00:02Z","payload":{"type":"function_call_output","call_id":"call-moved","output":"ok"}}),
            ]
            .iter()
            .map(|r| format!("{r}\n"))
            .collect::<String>()
        };
        fs::write(codex.join("a.jsonl"), rollout("2026-10-02T00:00:00Z")).unwrap();
        fs::write(codex.join("b.jsonl"), rollout("2026-10-01T00:00:00Z")).unwrap();
        let db = dir.path().join("index.db");
        sessidx::store::Store::open(&db)
            .unwrap()
            .refresh(
                &[
                    sessidx::discovery::Root {
                        agent: sessidx::model::Agent::Claude,
                        path: root,
                    },
                    sessidx::discovery::Root {
                        agent: sessidx::model::Agent::Codex,
                        path: codex,
                    },
                ],
                true,
                None,
            )
            .unwrap();

        fn get<'a>(by_verb: &'a [(&str, Candidates)], verb: &str) -> &'a Candidates {
            &by_verb.iter().find(|(v, _)| *v == verb).unwrap().1
        }
        let all = workload(&db, None, None).unwrap();
        assert_eq!(
            get(&all, "search").replayable,
            [
                words(&["search", "newer"]),
                words(&["search", "second"]),
                words(&["search", "moved"]),
                words(&["search", "last"])
            ]
        );
        assert_eq!(get(&all, "search").invocations, 10);
        assert_eq!(get(&all, "search").unreplayable, 2);
        assert_eq!(get(&all, "search").transcripts, 4);
        assert_eq!(get(&all, "show").unreplayable, 1);
        assert_eq!(
            get(&all, "count").replayable,
            [words(&["count", "commands"])]
        );
        assert_eq!(get(&all, "sql").replayable, [words(&["sql", "SELECT 1"])]);
        assert!(get(&all, "grep").replayable.is_empty());
        let windowed = workload(
            &db,
            sessidx::query::date("2026-10-04").ok(),
            sessidx::query::date("2026-10-07").ok(),
        )
        .unwrap();
        assert_eq!(
            get(&windowed, "search").replayable,
            [words(&["search", "newer"])]
        );
        assert_eq!(get(&windowed, "search").unreplayable, 1);
        assert_eq!(get(&windowed, "search").transcripts, 2);
        assert!(get(&windowed, "sql").replayable.is_empty());
    }
}
