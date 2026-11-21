use crate::common::{indexed_files, serial, sessidx, stream};
use serde_json::{Value, json};
use sessidx::{model::Agent, query::EXCERPT_BYTES, redaction};
use std::fs;

/// The page size docs/cli.md promises.
const PAGE: usize = 24 * 1024;

/// A record too large for a page in both its strings and its structure, a
/// short record, and two that fit a page only apart and that `[cd]{100}`
/// matches.
fn transcript() -> String {
    let answer = "词语 ".repeat(15_000) + "searchneedle";
    let output = "b".repeat(60_000) + "tailneedle";
    let items = vec![json!({"type":"text","text":"x"}); 4000];
    let lines = [
        json!({"type":"assistant","uuid":"a1","sessionId":"pages","timestamp":"2026-10-01T00:00:00Z","message":{"id":"m1","role":"assistant","model":"claude-x","content":[{"type":"text","text":answer}]},"toolUseResult":{"stdout":output,"items":items}}),
        json!({"type":"user","uuid":"u2","sessionId":"pages","timestamp":"2026-10-01T00:00:01Z","message":{"role":"user","content":"short searchneedle"}}),
        json!({"type":"user","uuid":"u3","sessionId":"pages","timestamp":"2026-10-01T00:00:02Z","message":{"role":"user","content":format!("pairneedle {}", "c".repeat(14 * 1024))}}),
        json!({"type":"user","uuid":"u4","sessionId":"pages","timestamp":"2026-10-01T00:00:03Z","message":{"role":"user","content":"d".repeat(14 * 1024)}}),
    ];
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// Asserts the data lines between `begin` and `end` fit a page and returns
/// their total with newlines.
fn within_page(stdout: &[u8]) -> usize {
    let text = String::from_utf8_lossy(stdout);
    let lines: Vec<&str> = text.lines().collect();
    // An error has no `begin`; `stream` checks where a page needs one.
    let first = usize::from(lines[0].contains(r#""type":"begin""#));
    let total = lines[first..lines.len() - 1]
        .iter()
        .map(|l| l.len() + 1)
        .sum();
    assert!(total <= PAGE, "{total} bytes");
    total
}

#[test]
fn show_pages_continue_inside_a_record_and_move_whole_records() {
    let serial = serial();
    let text = transcript();
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("pages", &text)]);
    let target = format!("{}:1", roots[0].path.join("pages.jsonl").display());
    let page = |cursor: Option<&str>, exit| {
        let mut args = vec!["show", &target, "--around", "3"];
        args.extend(cursor.iter().flat_map(|c| ["--cursor", c]));
        let out = sessidx(&serial, &store.path, &roots, &args)
            .output()
            .unwrap();
        (within_page(&out.stdout), stream(&out, exit))
    };
    let mut joined = String::new();
    let mut whole = None;
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        assert!(pages.len() < 20, "show does not advance");
        let (bytes, rows) = page(cursor.as_deref(), 0);
        let end = rows.last().unwrap().clone();
        // A record that continues fills its page up to one character.
        if end["next"]
            .as_str()
            .is_some_and(|n| n.contains(r#""within""#))
        {
            assert!(bytes > PAGE - 4, "{bytes} bytes");
        }
        for row in &rows[..rows.len() - 1] {
            let Some(part) = row["part"].as_str() else {
                assert!(row.get("cut").is_none(), "{row}");
                continue;
            };
            assert_eq!(row["line_no"], 1);
            assert!(row.get("record").is_none());
            let cut = &row["cut"][0];
            assert_eq!(cut["field"], "/part");
            assert_eq!(cut["offset"], joined.len());
            assert_eq!(cut["kept"], part.len());
            joined.push_str(part);
            assert_eq!(*whole.get_or_insert(cut["bytes"].clone()), cut["bytes"]);
        }
        pages.push(rows);
        assert_eq!(end["complete"], end["next"].is_null());
        match end["next"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(whole.unwrap(), joined.len());
    let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&joined).unwrap(), first);
    // Lines 3 and 4 fit a page only apart.
    let page_of = |line: u64| {
        pages
            .iter()
            .position(|p| {
                p.iter()
                    .any(|r| r["type"] == "record" && r["line_no"] == line)
            })
            .unwrap()
    };
    assert!(page_of(3) < page_of(4));

    let continuing: Value =
        serde_json::from_str(pages[0].last().unwrap()["next"].as_str().unwrap()).unwrap();
    let inside_a_character = joined.find('词').unwrap() + 1;
    for within in [inside_a_character, joined.len() + 1] {
        let mut edited = continuing.clone();
        edited["within"] = json!(within);
        let (_, rows) = page(Some(&edited.to_string()), 2);
        assert!(
            rows[0]["error"]
                .as_str()
                .unwrap()
                .contains("restart without --cursor")
        );
    }
}

/// The redacted JSON text grep shows a part of for a source line.
fn displayed(line: &str) -> String {
    let mut record: Value = serde_json::from_str(line).unwrap();
    redaction::redact_value(&mut record);
    record.to_string()
}

/// Asserts that a located grep hit shows `display` whole when it fits an
/// excerpt, and otherwise the excerpt around `needle` with its `cut`.
fn assert_excerpt(hit: &Value, display: &str, needle: &str) {
    assert!(hit.get("record").is_none() && hit.get("part").is_none());
    assert!(hit.get("match_located").is_none(), "{hit}");
    let text = hit["text"].as_str().unwrap();
    if display.len() <= EXCERPT_BYTES {
        assert_eq!(text, display);
        assert!(hit.get("cut").is_none(), "{hit}");
        return;
    }
    assert!(text.contains(needle), "{text}");
    let cut = &hit["cut"];
    assert_eq!(cut.as_array().unwrap().len(), 1);
    assert_eq!(cut[0]["field"], "/text");
    assert_eq!(cut[0]["bytes"], display.len());
    assert_eq!(cut[0]["kept"], text.len());
    assert!(text.len() <= EXCERPT_BYTES && text.len() > EXCERPT_BYTES - 4);
    let offset = cut[0]["offset"].as_u64().unwrap() as usize;
    assert_eq!(display.get(offset..offset + text.len()), Some(text));
}

#[test]
fn grep_shows_a_record_larger_than_a_page_around_its_match() {
    let serial = serial();
    let text = transcript();
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("pages", &text)]);
    let path = roots[0].path.join("pages.jsonl").display().to_string();
    let out = sessidx(
        &serial,
        &store.path,
        &roots,
        &["grep", "tailneedle", "--session", &path],
    )
    .output()
    .unwrap();
    within_page(&out.stdout);
    let rows = stream(&out, 0);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["line_no"], 1);
    let display = displayed(text.lines().next().unwrap());
    assert!(display.len() > PAGE);
    assert_excerpt(&rows[0], &display, "tailneedle");
    assert_eq!(rows[1]["complete"], true);
}

#[test]
fn grep_shows_a_short_record_whole_and_cuts_on_character_boundaries() {
    let serial = serial();
    // Three-byte characters surround each long record's match, and the
    // window's lead, 64 bytes, starts inside one in two of them.
    let lines: Vec<String> = [
        "short utfneedle".to_owned(),
        format!("{}utfneedle{}", "词".repeat(1000), "语".repeat(1000)),
        format!("{} utfneedle{}", "词".repeat(1001), "语".repeat(1001)),
        format!("{}utfneedle{}", "词".repeat(1002), "语".repeat(1002)),
    ]
    .iter()
    .enumerate()
    .map(|(i, content)| json!({"type":"user","uuid":format!("u{i}"),"sessionId":"utf","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":content}}).to_string())
    .collect();
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("utf", &(lines.join("\n") + "\n"))]);
    let path = roots[0].path.join("utf.jsonl").display().to_string();
    let rows = stream(
        &sessidx(
            &serial,
            &store.path,
            &roots,
            &["grep", "utfneedle", "--session", &path],
        )
        .output()
        .unwrap(),
        0,
    );
    assert_eq!(rows.len(), lines.len() + 1);
    for (row, line) in rows.iter().zip(&lines) {
        assert_excerpt(row, &displayed(line), "utfneedle");
    }
    assert!(rows[0].get("cut").is_none());
    // A long record's excerpt leads into its match by an eighth of a window,
    // less the part of a character that does not fit.
    for row in &rows[1..lines.len()] {
        let lead = row["text"].as_str().unwrap().find("utfneedle").unwrap();
        assert!(
            lead <= EXCERPT_BYTES / 8 && lead > EXCERPT_BYTES / 8 - 3,
            "{lead}"
        );
    }
}

#[test]
fn grep_moves_a_hit_that_does_not_fit_to_the_next_page() {
    let serial = serial();
    let text: String = (0..60)
        .map(|i| {
            let record = json!({"type":"user","uuid":format!("u{i}"),"sessionId":"pages","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":format!("pairneedle {}", "c".repeat(1000))}});
            format!("{record}\n")
        })
        .collect();
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("pages", &text)]);
    let path = roots[0].path.join("pages.jsonl").display().to_string();
    let pages = |args: &[&str], exit| {
        // Each page's rows with the byte length of each line.
        let mut pages: Vec<(Vec<Value>, Vec<usize>)> = Vec::new();
        let mut cursor = None;
        loop {
            assert!(pages.len() < 50, "paging does not advance");
            let mut args = args.to_vec();
            args.extend(
                cursor
                    .iter()
                    .flat_map(|c: &String| ["--cursor", c.as_str()]),
            );
            let out = sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap();
            within_page(&out.stdout);
            let rows = stream(&out, exit);
            let sizes = String::from_utf8_lossy(&out.stdout)
                .lines()
                .skip(1)
                .map(|l| l.len() + 1)
                .collect();
            cursor = rows.last().unwrap()["next"].as_str().map(str::to_owned);
            pages.push((rows, sizes));
            if cursor.is_none() {
                return pages;
            }
        }
    };
    let searched = |pages: &[(Vec<Value>, Vec<usize>)], key: &str| -> u64 {
        pages
            .iter()
            .map(|(p, _)| p.last().unwrap()["searched"][key].as_u64().unwrap())
            .sum()
    };
    let grep = pages(
        &["grep", "pairneedle", "--session", &path, "--limit", "1000"],
        0,
    );
    assert!(grep.len() > 2);
    let lines: Vec<u64> = grep
        .iter()
        .flat_map(|(p, _)| p.iter().filter_map(|r| r["line_no"].as_u64()))
        .collect();
    assert_eq!(lines, (1..=60).collect::<Vec<_>>());
    // A page ends only where the next page's first hit would not fit it.
    for pair in grep.windows(2) {
        let (rows, sizes) = &pair[0];
        let data = sizes[..rows.len() - 1].iter().sum::<usize>();
        assert!(data + pair[1].1[0] > PAGE, "{data} bytes");
    }
    // Each source range is counted on the page that returns or passes it.
    let all = pages(&["grep", "absentneedle", "--session", &path], 1);
    for key in ["records", "bytes"] {
        assert_eq!(searched(&grep, key), searched(&all, key), "{key}");
    }
}

#[test]
fn a_page_of_exactly_24_kib_stays_whole() {
    let serial = serial();
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("exact", "")]);
    let file = roots[0].path.join("exact.jsonl");
    let path = file.display().to_string();
    let target = format!("{path}:1");
    let record = |n: usize, filler: usize| {
        json!({"type":"user","uuid":format!("x{n:02}"),"sessionId":"exact","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":format!("fitneedle {}", "x".repeat(filler))}}).to_string()
    };
    let page = |args: &[&str], fillers: &[usize]| {
        let lines: String = fillers
            .iter()
            .enumerate()
            .map(|(n, &filler)| record(n, filler) + "\n")
            .collect();
        fs::write(&file, lines).unwrap();
        let out = sessidx(&serial, &store.path, &roots, args)
            .output()
            .unwrap();
        (within_page(&out.stdout), stream(&out, 0))
    };
    // One show record fills the page.
    let show = ["show", &target, "--around", "0"];
    let (short, rows) = page(&show, &[1000]);
    let (exact, filled) = page(&show, &[1000 + PAGE - short]);
    assert_eq!(exact, PAGE);
    assert_eq!(filled.len(), rows.len());
    assert!(filled.iter().all(|r| r.get("cut").is_none()));
    // Grep hits fill it, each shown whole up to a full excerpt.
    let grep = ["grep", "fitneedle", "--session", &path, "--limit", "1000"];
    let room = EXCERPT_BYTES - displayed(&record(0, 0)).len();
    let (short, rows) = page(&grep, &[0; 40]);
    let mut missing = PAGE - short;
    let fillers: Vec<usize> = (0..40)
        .map(|_| {
            let filler = missing.min(room);
            missing -= filler;
            filler
        })
        .collect();
    assert_eq!(missing, 0);
    assert!(fillers.contains(&room));
    let (exact, filled) = page(&grep, &fillers);
    assert_eq!(exact, PAGE);
    assert_eq!(filled.len(), rows.len());
    assert!(filled.iter().all(|r| r.get("cut").is_none()));
}

#[test]
fn search_moves_a_session_that_does_not_fit_and_keeps_an_exact_page_whole() {
    let serial = serial();
    // One long token keeps every session's BM25 score equal, so a longer
    // filler changes the page's size but not the order: newest first.
    let message = |session: &str, ts: &str, filler: usize| {
        format!(
            "{}\n",
            json!({"type":"user","uuid":format!("{session}-1"),"sessionId":session,"timestamp":ts,"message":{"role":"user","content":format!("fitneedle {}", "x".repeat(filler))}})
        )
    };
    let older: Vec<(String, String)> = (0..80)
        .map(|i| {
            let id = format!("s{i:03}");
            let ts = format!("2026-10-01T00:{:02}:{:02}Z", i / 60, i % 60);
            let line = message(&id, &ts, 1);
            (id, line)
        })
        .collect();
    let mut sessions: Vec<(&str, &str)> = older
        .iter()
        .map(|(id, line)| (id.as_str(), line.as_str()))
        .collect();
    sessions.push(("exact", ""));
    let (_dir, store, roots) = indexed_files(Agent::Claude, &sessions);
    let file = roots[0].path.join("exact.jsonl");
    let search = |filler: usize, cursor: Option<&str>| {
        fs::write(&file, message("exact", "2026-10-02T00:00:00Z", filler)).unwrap();
        let mut args = vec!["search", "fitneedle", "--limit", "1000"];
        args.extend(cursor.iter().flat_map(|c| ["--cursor", *c]));
        let out = sessidx(&serial, &store.path, &roots, &args)
            .output()
            .unwrap();
        (within_page(&out.stdout), stream(&out, 0))
    };
    let ids = |rows: &[Value]| -> Vec<String> {
        rows.iter()
            .filter_map(|r| r["session_id"].as_str().map(str::to_owned))
            .collect()
    };
    let (short, first) = search(1, None);
    let next = first.last().unwrap()["next"].as_str().unwrap().to_owned();
    let (_, second) = search(1, Some(&next));
    assert_eq!(second.last().unwrap()["complete"], true);
    let mut all = [ids(&first), ids(&second)].concat();
    assert_eq!(all[0], "exact");
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 81);
    // The session that did not fit opens the next page whole.
    assert!(first.iter().chain(&second).all(|r| r.get("cut").is_none()));
    let (exact, filled) = search(1 + PAGE - short, None);
    assert_eq!(exact, PAGE);
    assert_eq!(ids(&filled), ids(&first));
    assert!(filled.iter().all(|r| r.get("cut").is_none()));
}

#[test]
fn search_shows_a_session_larger_than_a_page_alone() {
    let serial = serial();
    // Only the session's own fields, here its directory, can pass a page.
    let line = format!(
        "{}\n",
        json!({"type":"user","uuid":"big-1","sessionId":"big","cwd":format!("/{}", "d".repeat(PAGE)),"timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"bigneedle"}})
    );
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("big", &line)]);
    let out = sessidx(&serial, &store.path, &roots, &["search", "bigneedle"])
        .output()
        .unwrap();
    let rows = stream(&out, 0);
    assert_eq!(rows[0]["session_id"], "big");
    assert_eq!(rows.last().unwrap()["complete"], true);
}

#[test]
fn a_lookup_cut_to_its_first_line_still_states_its_status() {
    let serial = serial();
    let text = transcript();
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("pages", &text)]);
    let path = roots[0].path.join("pages.jsonl").display().to_string();
    for (args, more) in [
        (
            vec!["grep", "searchneedle", "--session", &path, "--limit", "1"],
            true,
        ),
        (vec!["show", &path, "--limit", "1"], true),
        (vec!["search", "short"], false),
    ] {
        let out = sessidx(&serial, &store.path, &roots, &args)
            .output()
            .unwrap();
        // What `| head -n 1` leaves an agent.
        let text = String::from_utf8_lossy(&out.stdout);
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first["type"], "begin", "{args:?}");
        assert_eq!(first["complete"], !more, "{args:?}");
        assert_eq!(first["more"], more, "{args:?}");
        assert_eq!(first["returned"], 1, "{args:?}");
        assert!(text.len() > text.lines().next().unwrap().len() + 1);
    }
}
