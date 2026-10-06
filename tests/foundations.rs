use sessidx::{adapters, discovery::Root, model::State, store::Store};
use std::fs;

fn events(h: &str, fixture: &str) -> (State, Vec<sessidx::model::Event>) {
    let mut s = State::default();
    let es = fixture
        .lines()
        .flat_map(|l| adapters::parse(h, &serde_json::from_str(l).unwrap(), &mut s).events)
        .collect();
    (s, es)
}

#[test]
fn claude_blocks_flags_and_synthetic_model() {
    let (s, es) = events("claude", include_str!("../testdata/claude.jsonl"));
    assert_eq!(es.iter().filter(|e| e.kind == "tool_call").count(), 1);
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "flag")
            .count(),
        1
    );
    assert_eq!(s.model.as_deref(), Some("claude-sonnet"));
    assert_eq!(es.iter().filter(|e| e.role == "user").count(), 1);
    assert!(
        es.iter()
            .filter(|e| e.kind == "tool_result")
            .all(|e| e.text.is_none())
    );
}

#[test]
fn codex_context_arguments_and_telemetry() {
    let (s, es) = events("codex", include_str!("../testdata/codex.jsonl"));
    assert_eq!(s.model.as_deref(), Some("gpt-fixture"));
    assert_eq!(
        es.iter()
            .filter(|e| e.command.as_deref() == Some("rg word src"))
            .count(),
        1
    );
    assert_eq!(es.iter().filter(|e| e.kind == "message").count(), 1);
    assert!(s.instruction_hash.is_some());
    assert!(
        es.iter()
            .filter(|e| e.kind == "tool_result")
            .all(|e| e.text.is_none())
    );
}

#[test]
fn pi_model_tool_call_and_camel_case_flag() {
    let (s, es) = events("pi", include_str!("../testdata/pi.jsonl"));
    assert_eq!(s.model.as_deref(), Some("pi-model"));
    assert_eq!(es.iter().filter(|e| e.kind == "tool_call").count(), 1);
    assert_eq!(
        es.iter()
            .filter(|e| e.ok == Some(false) && e.ok_source == "flag")
            .count(),
        1
    );
    assert_eq!(es.iter().filter(|e| e.ok.is_none()).count(), 2);
}

fn snapshot(store: &Store) -> Vec<String> {
    store.db.prepare("SELECT session_id,line_no,byte_off,byte_len,ordinal,role,kind,coalesce(model,''),coalesce(text,''),ok_source FROM events ORDER BY line_no,ordinal").unwrap()
        .query_map([], |r| { Ok((0..10).map(|i| match r.get_ref(i).unwrap() { rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(), rusqlite::types::ValueRef::Integer(n) => n.to_string(), _ => String::new() }).collect::<Vec<_>>().join("|")) }).unwrap().map(Result::unwrap).collect()
}

#[test]
fn incremental_append_truncate_replace_equals_rebuild_and_exact_pointers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("logs");
    fs::create_dir(&root).unwrap();
    let path = root.join("s.jsonl");
    let fixture = include_str!("../testdata/codex.jsonl");
    let roots = [Root {
        harness: "codex".into(),
        path: root,
    }];
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    for (i, data) in [
        fixture.lines().take(2).collect::<Vec<_>>().join("\n") + "\n",
        fixture.into(),
        fixture.lines().take(3).collect::<Vec<_>>().join("\n") + "\n",
        fixture.replace("gpt-fixture", "gpt-replaced"),
    ]
    .into_iter()
    .enumerate()
    {
        if i == 1 {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(
                    data.lines()
                        .skip(2)
                        .collect::<Vec<_>>()
                        .join("\n")
                        .as_bytes(),
                )
                .unwrap();
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"\n")
                .unwrap();
        } else if i == 3 {
            let replacement = path.with_extension("new");
            fs::write(&replacement, &data).unwrap();
            fs::rename(replacement, &path).unwrap();
        } else {
            fs::write(&path, &data).unwrap();
        }
        store.refresh(&roots, false, None).unwrap();
        let got = snapshot(&store);
        let mut clean = Store::open(&dir.path().join(format!("clean-{i}.db"))).unwrap();
        clean.refresh(&roots, true, None).unwrap();
        assert_eq!(got, snapshot(&clean));
        let raw = fs::read(&path).unwrap();
        let ranges: Vec<(usize, usize, usize)> = store
            .db
            .prepare("SELECT line_no,byte_off,byte_len FROM events")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for (line, off, len) in ranges {
            assert_eq!(
                &raw[off..off + len],
                raw.split_inclusive(|b| *b == b'\n').nth(line - 1).unwrap()
            );
        }
    }
}
