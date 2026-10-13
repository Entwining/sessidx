use crate::common::{indexed_files, serial, sessidx, stream};
use serde_json::{Value, json};
use sessidx::model::Agent;
use std::fs;

/// The page size docs/cli.md promises.
const PAGE: usize = 24 * 1024;

/// A record too large for a page in both its strings and its structure, then
/// a short record that `searchneedle` also matches, and two that fit a page only apart and that `pairneedle`
/// and `[cd]{100}` match.
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

/// Asserts the data lines fit a page and returns their total with newlines.
fn within_page(stdout: &[u8]) -> usize {
    let text = String::from_utf8_lossy(stdout);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines[..lines.len() - 1].iter().map(|l| l.len() + 1).sum();
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

#[test]
fn grep_and_search_cut_hits_whose_match_lies_past_the_page() {
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
    assert!(rows[0].get("record").is_none());
    assert_eq!(rows[0]["cut"][0]["field"], "/part");
    assert!(!rows[0]["part"].as_str().unwrap().contains("tailneedle"));
    assert_eq!(rows[1]["complete"], true);

    let out = sessidx(&serial, &store.path, &roots, &["search", "searchneedle"])
        .output()
        .unwrap();
    within_page(&out.stdout);
    let rows = stream(&out, 0);
    // Only the long hit text is cut; the short one stays whole.
    let cut = rows[0]["cut"].as_array().unwrap();
    assert_eq!(cut.len(), 1);
    let text = rows[0]
        .pointer(cut[0]["field"].as_str().unwrap())
        .unwrap()
        .as_str()
        .unwrap();
    assert!(text.starts_with("词语") && !text.contains("searchneedle"));
    assert_eq!(rows[0]["hits"].as_array().unwrap().len(), 2);
}

#[test]
fn grep_and_search_move_a_record_that_does_not_fit_to_the_next_page() {
    let serial = serial();
    let text = transcript();
    let other = format!(
        "{}\n",
        json!({"type":"user","uuid":"o1","sessionId":"other","timestamp":"2026-10-01T00:00:04Z","message":{"role":"user","content":format!("pairneedle {}", "e".repeat(14 * 1024))}})
    );
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("pages", &text), ("other", &other)]);
    let path = roots[0].path.join("pages.jsonl").display().to_string();
    let pages = |args: &[&str], exit| {
        let mut pages: Vec<Vec<Value>> = Vec::new();
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
            cursor = rows.last().unwrap()["next"].as_str().map(str::to_owned);
            pages.push(rows);
            if cursor.is_none() {
                return pages;
            }
        }
    };
    let searched = |pages: &[Vec<Value>], key: &str| -> u64 {
        pages
            .iter()
            .map(|p| p.last().unwrap()["searched"][key].as_u64().unwrap())
            .sum()
    };
    let lines = |pages: &[Vec<Value>]| -> Vec<Vec<u64>> {
        pages
            .iter()
            .map(|p| p.iter().filter_map(|r| r["line_no"].as_u64()).collect())
            .collect()
    };
    let grep = pages(&["grep", "[cd]{100}", "--session", &path], 0);
    assert_eq!(lines(&grep), [vec![3], vec![4]]);
    // Each source range is counted on the page that returns or passes it.
    let all = pages(&["grep", "absentneedle", "--session", &path], 1);
    for key in ["records", "bytes"] {
        assert_eq!(searched(&grep, key), searched(&all, key), "{key}");
    }
    let search = pages(&["search", "pairneedle"], 0);
    let sessions: Vec<usize> = search.iter().map(|p| p.len() - 1).collect();
    assert_eq!(sessions, [1, 1]);
    assert!(search.iter().flatten().all(|r| r.get("cut").is_none()));
}

#[test]
fn a_page_of_exactly_24_kib_stays_whole() {
    let serial = serial();
    // A second session that only `search` reaches puts its filled line after another.
    let pair = format!(
        "{}\n",
        json!({"type":"user","uuid":"p1","sessionId":"pair","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"fitneedle"}})
    );
    let (_dir, store, roots) = indexed_files(Agent::Claude, &[("exact", ""), ("pair", &pair)]);
    let file = roots[0].path.join("exact.jsonl");
    let path = file.display().to_string();
    let target = format!("{path}:1");
    for args in [
        vec!["show", &target, "--around", "0"],
        vec!["grep", "fitneedle", "--session", &path],
        vec!["search", "fitneedle"],
    ] {
        let page = |filler: usize| {
            let record = json!({"type":"user","uuid":"x1","sessionId":"exact","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":format!("fitneedle {}", "x".repeat(filler))}});
            fs::write(&file, format!("{record}\n")).unwrap();
            let out = sessidx(&serial, &store.path, &roots, &args)
                .output()
                .unwrap();
            (within_page(&out.stdout), stream(&out, 0))
        };
        let (short, rows) = page(1000);
        let (exact, filled) = page(1000 + PAGE - short);
        assert_eq!(exact, PAGE, "{args:?}");
        assert_eq!(filled.len(), rows.len(), "{args:?}");
        assert!(filled.iter().all(|r| r.get("cut").is_none()), "{args:?}");
    }
}
