use serde_json::{Value, json};
use sessidx::{
    adapters,
    discovery::Root,
    model::{Event, Harness, State, name},
    query::{self, Filters},
    store::Store,
};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
    sync::{Mutex, MutexGuard, PoisonError},
};

/// Hold for the whole test when it launches the CLI or takes one database's
/// writer lock more than once. A child inherits every open descriptor until it
/// execs, so a writer lock released in that window stays held and the next
/// refresh of that database reports writer_busy.
pub fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Launches the CLI only under [`serial`], since spawning is the hazard. HOME is
/// the database's scratch directory, so default roots never reach real sessions.
pub fn sessidx(_serial: &MutexGuard<()>, db: &Path, roots: &[Root], args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sessidx"));
    cmd.env("HOME", db.parent().unwrap());
    cmd.arg("--db").arg(db);
    for r in roots {
        cmd.arg("--root")
            .arg(format!("{}={}", name(r.harness), r.path.display()));
    }
    cmd.args(args);
    cmd
}

pub fn stream(out: &Output, exit: i32) -> Vec<Value> {
    assert_eq!(
        out.status.code(),
        Some(exit),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let records: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(!records.is_empty());
    assert!(records.iter().all(|v| v["type"].is_string()));
    for record in records.iter().filter(|v| v["type"] == "session") {
        assert_eq!(record["path"], record["hits"][0]["path"]);
    }
    assert_eq!(records.iter().filter(|v| v["type"] == "end").count(), 1);
    let end = records.last().unwrap();
    assert_eq!(end["type"], "end");
    let searched = &end["searched"];
    let returned = searched["returned"].as_u64().unwrap();
    match searched["unit"].as_str() {
        Some("sql_rows" | "aggregate_rows") => assert_eq!(searched["records"], returned),
        Some("source_records") => assert_eq!(searched["records"], records[0]["records"]),
        Some("source_ranges") => {
            assert!(searched["records"].as_u64().unwrap() >= returned);
            if returned > end["unavailable_ranges"].as_u64().unwrap() {
                assert!(searched["bytes"].as_u64().unwrap() > 0);
            }
        }
        _ => {}
    }
    records
}

pub fn indexed(harness: Harness, content: &str) -> (tempfile::TempDir, Store, Vec<Root>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("logs");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("session.jsonl"), content).unwrap();
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    let roots = vec![Root {
        harness,
        path: root,
    }];
    store.refresh(&roots, false, None).unwrap();
    (dir, store, roots)
}

pub fn three_harnesses() -> (tempfile::TempDir, Store, Vec<Root>) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    let mut roots = Vec::new();
    for harness in [Harness::Claude, Harness::Codex, Harness::Pi] {
        let path = dir.path().join(format!("{}.jsonl", name(harness)));
        let mut records: Vec<Value> = (0..2).map(|i| match harness {
            Harness::Claude => json!({"type":"user","uuid":format!("c-{i}"),"sessionId":"claude-session","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}}),
            Harness::Codex => json!({"type":"response_item","timestamp":"2026-10-01T00:00:00Z","payload":{"type":"message","id":format!("x-{i}"),"role":"user","content":[{"type":"input_text","text":"sharedneedle"}]}}),
            Harness::Pi => json!({"type":"message","id":format!("p-{i}"),"timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"sharedneedle"}}),
        }).collect();
        if harness == Harness::Claude {
            records.push(json!({"type":"assistant","uuid":"c-call","sessionId":"claude-session","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"Bash","input":{"command":"rg needle ."}}]}}));
        }
        fs::write(
            &path,
            records.iter().map(|v| format!("{v}\n")).collect::<String>(),
        )
        .unwrap();
        roots.push(Root { harness, path });
    }
    store.refresh(&roots, false, None).unwrap();
    (dir, store, roots)
}

pub fn search_hits(
    db: &rusqlite::Connection,
    query: &str,
    filters: &Filters,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Vec<query::Hit>> {
    Ok(query::search(db, query, filters, limit, offset)?
        .sessions
        .into_iter()
        .flat_map(|s| s.hits)
        .collect())
}

pub fn snapshot(store: &Store) -> Vec<String> {
    store.db.prepare("SELECT session_id,line_no,byte_off,byte_len,ordinal,role,kind,coalesce(model,''),coalesce(text,''),ok_source FROM events ORDER BY line_no,ordinal").unwrap()
        .query_map([], |r| { Ok((0..10).map(|i| match r.get_ref(i).unwrap() { rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(), rusqlite::types::ValueRef::Integer(n) => n.to_string(), _ => String::new() }).collect::<Vec<_>>().join("|")) }).unwrap().map(Result::unwrap).collect()
}

pub fn events(h: Harness, fixture: &str) -> (State, Vec<Event>) {
    let mut s = State::default();
    let es = fixture
        .lines()
        .flat_map(|l| adapters::parse(h, &serde_json::from_str(l).unwrap(), &mut s).events)
        .collect();
    (s, es)
}
