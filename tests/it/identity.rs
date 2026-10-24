use serde_json::json;
use sessidx::{
    discovery::Root,
    model::Agent,
    query::{self, Filters},
    store::Store,
};
use std::{fs, path::Path};

fn lines(records: &[serde_json::Value]) -> String {
    records.iter().map(|r| format!("{r}\n")).collect()
}

fn write(path: &Path, records: &[serde_json::Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, lines(records)).unwrap();
}

fn store(dir: &Path, roots: &[Root]) -> Store {
    let mut store = Store::open(&dir.join("index.db")).unwrap();
    store.refresh(roots, false, None).unwrap();
    store
}

/// `(session_id, parent_id, session_kind)` of the transcript at `path`, and
/// the session IDs its events report.
fn session(store: &Store, path: &Path) -> ((String, Option<String>, String), Vec<String>) {
    let path = path.to_str().unwrap();
    let row = store
        .db
        .query_row(
            "SELECT session_id,parent_id,session_kind FROM sessions WHERE path=?",
            [path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let events = store
        .db
        .prepare("SELECT DISTINCT session_id FROM events WHERE path=?")
        .unwrap()
        .query_map([path], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    (row, events)
}

#[test]
fn transcript_identity_comes_from_its_own_header_not_copied_history() {
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("codex");
    let child = codex.join("rollout-child.jsonl");
    // A spawned thread's rollout repeats its parent's session_meta after its own.
    write(
        &child,
        &[
            json!({"type":"session_meta","payload":{"id":"child","cwd":"/synthetic/child","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent"}}}}}),
            json!({"type":"session_meta","payload":{"id":"parent","cwd":"/synthetic/parent","source":"cli"}}),
            json!({"type":"response_item","payload":{"type":"message","id":"m","role":"user","content":[{"type":"input_text","text":"hello"}]}}),
        ],
    );
    let claude = dir.path().join("claude");
    let main = claude.join("project/main-id.jsonl");
    // A resumed session's first record can carry the session it resumed, and
    // a later record can carry an agent's ID.
    write(
        &main,
        &[
            json!({"type":"user","uuid":"copied","sessionId":"resumed-from","cwd":"/synthetic/resumed","isSidechain":true,"message":{"role":"user","content":"before"}}),
            json!({"type":"user","uuid":"own","sessionId":"main-id","cwd":"/synthetic/main","message":{"role":"user","content":"after"}}),
            json!({"type":"user","uuid":"agent-progress","agentId":"a9","sessionId":"main-id","isSidechain":true,"message":{"role":"user","content":"progress"}}),
            json!({"type":"user","uuid":"later","sessionId":"main-id","isSidechain":false,"message":{"role":"user","content":"later"}}),
        ],
    );
    // A subagent's records can open with one carrying only its parent's
    // session ID, or one naming the agent without the parent session.
    let subagent = claude.join("project/main-id/subagents/agent-a1.jsonl");
    write(
        &subagent,
        &[
            json!({"type":"system","sessionId":"main-id"}),
            json!({"type":"fork-context-ref","agentId":"a1"}),
            json!({"type":"user","uuid":"sub","agentId":"a1","sessionId":"main-id","isSidechain":true,"message":{"role":"user","content":"task"}}),
        ],
    );
    let store = store(
        dir.path(),
        &[
            Root {
                agent: Agent::Codex,
                path: codex,
            },
            Root {
                agent: Agent::Claude,
                path: claude,
            },
        ],
    );
    assert_eq!(
        session(&store, &child),
        (
            ("child".into(), Some("parent".into()), "subagent".into()),
            vec!["child".to_owned()]
        )
    );
    let cwd = |id: &str| -> String {
        store
            .db
            .query_row("SELECT cwd FROM sessions WHERE session_id=?", [id], |r| {
                r.get(0)
            })
            .unwrap()
    };
    assert_eq!(cwd("child"), "/synthetic/child");
    assert_eq!(cwd("main-id"), "/synthetic/main");
    assert_eq!(
        session(&store, &main),
        (
            ("main-id".into(), None, "main".into()),
            vec!["main-id".to_owned()]
        )
    );
    assert_eq!(
        session(&store, &subagent),
        (
            ("a1".into(), Some("main-id".into()), "subagent".into()),
            vec!["a1".to_owned()]
        )
    );
}

#[test]
fn session_kind_comes_only_from_native_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut roots = Vec::new();
    let mut expected = Vec::new();
    for (agent, file, records, kind) in [
        (
            Agent::Claude,
            "sidechain-false.jsonl",
            vec![
                json!({"type":"user","sessionId":"sidechain-false","isSidechain":false,"message":{"role":"user","content":"a"}}),
            ],
            "main",
        ),
        (
            Agent::Claude,
            "no-evidence.jsonl",
            vec![
                json!({"type":"user","sessionId":"no-evidence","message":{"role":"user","content":"a"}}),
            ],
            "unknown",
        ),
        (
            Agent::Codex,
            "exec.jsonl",
            vec![json!({"type":"session_meta","payload":{"id":"e","source":"exec"}})],
            "main",
        ),
        (
            Agent::Codex,
            "review.jsonl",
            vec![
                json!({"type":"session_meta","payload":{"id":"r","source":{"subagent":"review"}}}),
            ],
            "subagent",
        ),
        (
            Agent::Codex,
            "other-source.jsonl",
            vec![json!({"type":"session_meta","payload":{"id":"o","source":{"synthetic":"x"}}})],
            "unknown",
        ),
        (
            Agent::Codex,
            "no-source.jsonl",
            vec![json!({"type":"session_meta","payload":{"id":"n"}})],
            "unknown",
        ),
        (
            Agent::Pi,
            "pi.jsonl",
            vec![json!({"type":"session","version":3,"id":"p","cwd":"/synthetic"})],
            "main",
        ),
        (
            Agent::Pi,
            "pi-fork.jsonl",
            vec![
                json!({"type":"session","version":3,"id":"f","cwd":"/synthetic","parentSession":"p"}),
            ],
            "main",
        ),
    ] {
        let root = dir.path().join(format!("{file}.root"));
        write(&root.join(file), &records);
        roots.push(Root { agent, path: root });
        expected.push((file, kind));
    }
    let store = store(dir.path(), &roots);
    for (file, kind) in expected {
        let found: String = store
            .db
            .query_row(
                "SELECT session_kind FROM sessions WHERE path LIKE '%/'||?",
                [file],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, kind, "{file}");
    }
}

#[test]
fn claude_workflow_journal_is_not_a_session() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    let run = claude.join("project/main-id/subagents/workflows/wf_1");
    write(
        &run.join("journal.jsonl"),
        &[json!({"type":"started","agentId":"a2","key":"k"})],
    );
    write(
        &run.join("agent-a2.jsonl"),
        &[
            json!({"type":"user","agentId":"a2","sessionId":"main-id","message":{"role":"user","content":"step"}}),
        ],
    );
    let store = store(
        dir.path(),
        &[Root {
            agent: Agent::Claude,
            path: claude,
        }],
    );
    let paths: Vec<String> = store
        .db
        .prepare("SELECT path FROM sessions")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(paths, [run.join("agent-a2.jsonl").to_str().unwrap()]);
}

#[test]
fn a_session_id_shared_by_transcripts_is_refused_with_their_paths() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    // Claude agent IDs are unique only within their parent session.
    let mut paths = Vec::new();
    for parent in ["one", "two"] {
        let path = claude.join(format!("project/{parent}/subagents/agent-a3.jsonl"));
        write(
            &path,
            &[
                json!({"type":"user","uuid":parent,"agentId":"a3","sessionId":parent,"message":{"role":"user","content":"shared"}}),
            ],
        );
        paths.push(path.to_str().unwrap().to_owned());
    }
    let store = store(
        dir.path(),
        &[Root {
            agent: Agent::Claude,
            path: claude,
        }],
    );
    let error = query::show(&store.db, "a3", 0, 20, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("names 2 transcripts"), "{error}");
    let filters = Filters {
        session: Some("a3".into()),
        ..Filters::default()
    };
    assert!(filters.sql(&store.db).is_err());
    for path in &paths {
        assert!(error.contains(path.as_str()), "{error}");
        let (shown, _) = query::show(&store.db, path, 0, 20, None).unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!(&shown[0].path, path);
    }
}

#[test]
fn events_read_bodies_only_when_a_query_names_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("index.db")).unwrap();
    store
        .db
        .execute_batch(include_str!("../../src/schema.sql"))
        .unwrap();
    let plan = |sql: &str| -> String {
        store
            .db
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join("\n")
    };
    for sql in [
        "SELECT kind,count(*) FROM events GROUP BY 1",
        "SELECT agent,model,count(*) FROM events WHERE kind='tool_call' GROUP BY 1,2",
    ] {
        assert!(!plan(sql).contains("event_texts"), "{sql}");
    }
    assert!(plan("SELECT text FROM events").contains("event_texts"));
}
