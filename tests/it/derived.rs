use rusqlite::OptionalExtension;
use sessidx::{discovery::Root, model::Agent, store::Store};
use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

/// `canonical` as `canonical_events` defined it before it was stored: the first
/// copy of each native event by its session's start, path, and position.
const FIRST_COPIES: &str = "SELECT r.id FROM event_rows r JOIN sessions s ON s.file_id=r.file_id
 WHERE r.native_id IS NULL OR NOT EXISTS (
  SELECT 1 FROM event_rows r2 JOIN sessions s2 ON s2.file_id=r2.file_id
  WHERE r2.native_id=r.native_id AND r2.kind=r.kind AND r2.id<>r.id AND s2.agent=s.agent
  AND (coalesce(s2.started_at,'9999'),s2.path,r2.line_no,r2.ordinal) < (coalesce(s.started_at,'9999'),s.path,r.line_no,r.ordinal))";

/// `call_ok` and `call_ok_source` as `call_outcomes` defined them before they
/// were stored, for every tool call rather than only first copies.
const OUTCOMES: &str = "SELECT c.id,
 (SELECT CASE WHEN sum(r.ok=0)>0 THEN 0 WHEN sum(r.ok=1)>0 THEN 1 END
  FROM sessions rs JOIN event_rows r ON r.file_id=rs.file_id
  WHERE rs.agent=cs.agent AND rs.session_id=cs.session_id AND r.call_id=c.call_id AND r.kind='tool_result'),
 (SELECT CASE WHEN sum(r.ok=0 AND r.ok_source='flag')>0 THEN 'flag'
              WHEN sum(r.ok=0 AND r.ok_source='text')>0 THEN 'text'
              WHEN sum(r.ok_source='flag')>0 THEN 'flag'
              WHEN sum(r.ok_source='text')>0 THEN 'text' ELSE 'none' END
  FROM sessions rs JOIN event_rows r ON r.file_id=rs.file_id
  WHERE rs.agent=cs.agent AND rs.session_id=cs.session_id AND r.call_id=c.call_id AND r.kind='tool_result')
 FROM event_rows c JOIN sessions cs ON cs.file_id=c.file_id WHERE c.kind='tool_call'";

/// `daily_counts` as the sums over each file, UTC day, and grouping fields of
/// its events, their stored derived columns, shell sites, and denials.
const DAILY_COUNTS: &str =
    "SELECT e.file_id,substr(e.ts,1,10),e.kind,e.role,e.model,e.canonical,count(*),
 sum(e.call_ok IS 0),sum(e.call_ok IS 1),
 sum(EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id)),
 sum(EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown')),
 sum((SELECT count(*) FROM denials d WHERE d.event_id=e.id)),
 sum((SELECT count(*) FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown')),
 sum((SELECT count(*) FROM commands c WHERE c.event_id=e.id AND c.parsed=1)),
 sum((SELECT count(*) FROM commands c WHERE c.event_id=e.id AND c.parsed=1 AND c.program IS NULL)),
 sum(EXISTS(SELECT 1 FROM commands c WHERE c.event_id=e.id)),
 sum(EXISTS(SELECT 1 FROM commands c WHERE c.event_id=e.id AND c.parsed=0)),
 sum(EXISTS(SELECT 1 FROM commands c WHERE c.event_id=CASE e.kind WHEN 'tool_call' THEN e.id WHEN 'tool_result' THEN e.call_event_id END
  AND (c.parsed=0 OR c.program IS NULL)))
 FROM event_rows e GROUP BY 1,2,3,4,5,6";

/// `call_event_id` as each tool result's call: the first row, by transcript
/// path and position, of a call or server call with its ID in its session.
const CALL_EVENTS: &str = "SELECT r.id,(SELECT c.id FROM event_rows c JOIN sessions cs ON cs.file_id=c.file_id
  WHERE cs.agent=rs.agent AND cs.session_id=rs.session_id AND c.call_id=r.call_id AND c.kind IN ('tool_call','server_tool_call')
  ORDER BY cs.path,c.line_no,c.ordinal LIMIT 1)
 FROM event_rows r JOIN sessions rs ON rs.file_id=r.file_id WHERE r.kind='tool_result'";

/// `daily_program_counts` as the sums of `daily_counts` over first-copy tool
/// calls and results, once per program their call's sites name.
const PROGRAM_COUNTS: &str =
    "SELECT e.file_id,substr(e.ts,1,10),e.kind,e.role,e.model,p.program,count(*),
 sum(e.call_ok IS 0),sum(e.call_ok IS 1),
 sum(EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id)),
 sum(EXISTS(SELECT 1 FROM denials d WHERE d.event_id=e.id AND d.reason_id='unknown')),
 sum(EXISTS(SELECT 1 FROM commands c WHERE c.event_id=p.event_id AND (c.parsed=0 OR c.program IS NULL))),
 sum(CASE WHEN e.kind='tool_call' THEN (SELECT count(*) FROM commands c WHERE c.event_id=e.id AND c.program=p.program) ELSE 0 END)
 FROM event_rows e JOIN (SELECT DISTINCT event_id,program FROM commands WHERE program IS NOT NULL) p
  ON p.event_id=CASE e.kind WHEN 'tool_call' THEN e.id WHEN 'tool_result' THEN e.call_event_id END
 WHERE e.canonical=1 GROUP BY 1,2,3,4,5,6";

/// `selected` as the usage row of each request that `requests` reports: of the
/// records of a native request, a finished one before one that is not, then
/// the first by its session's start and path, then the last in its transcript.
const SELECTED: &str = "SELECT u.event_id FROM usage_rows u JOIN event_rows r ON r.id=u.event_id JOIN sessions s ON s.file_id=r.file_id
 WHERE r.native_id IS NULL OR NOT EXISTS (
  SELECT 1 FROM usage_rows u2 JOIN event_rows r2 ON r2.id=u2.event_id JOIN sessions s2 ON s2.file_id=r2.file_id
  WHERE r2.native_id=r.native_id AND r2.kind=r.kind AND r2.id<>r.id AND s2.agent=s.agent
  AND (-u2.finished,coalesce(s2.started_at,'9999'),s2.path,-r2.line_no,-r2.ordinal) < (-u.finished,coalesce(s.started_at,'9999'),s.path,-r.line_no,-r.ordinal))";

/// Each session's timestamp bounds as the range of its events' timestamps.
const BOUNDS: &str = "SELECT s.file_id,(SELECT min(ts) FROM event_rows WHERE file_id=s.file_id),
 (SELECT max(ts) FROM event_rows WHERE file_id=s.file_id) FROM sessions s";

fn assert_derived(store: &Store, step: &str) {
    let differ = |stored: &str, defined: &str| -> i64 {
        store
            .db
            .query_row(
                // Compound operators apply left to right, so each difference
                // is a subquery of its own.
                &format!("SELECT count(*) FROM (SELECT * FROM (SELECT * FROM ({stored}) EXCEPT SELECT * FROM ({defined})) UNION ALL SELECT * FROM (SELECT * FROM ({defined}) EXCEPT SELECT * FROM ({stored})))"),
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        differ("SELECT id FROM event_rows WHERE canonical=1", FIRST_COPIES),
        0,
        "first copies after {step}"
    );
    assert_eq!(
        differ(
            "SELECT id,call_ok,call_ok_source FROM event_rows WHERE kind='tool_call'",
            OUTCOMES
        ),
        0,
        "outcomes after {step}"
    );
    assert_eq!(
        differ(
            "SELECT id,call_event_id FROM event_rows WHERE kind='tool_result'",
            CALL_EVENTS
        ),
        0,
        "calls of results after {step}"
    );
    assert_eq!(
        differ("SELECT * FROM daily_counts", DAILY_COUNTS),
        0,
        "daily counts after {step}"
    );
    assert_eq!(
        differ("SELECT * FROM daily_program_counts", PROGRAM_COUNTS),
        0,
        "daily program counts after {step}"
    );
    assert_eq!(
        differ("SELECT event_id FROM usage_rows WHERE selected=1", SELECTED),
        0,
        "selected usage after {step}"
    );
    assert_eq!(
        differ(
            "SELECT event_id,file_id,ts,model FROM usage_rows",
            "SELECT r.id,r.file_id,r.ts,r.model FROM usage_rows u JOIN event_rows r ON r.id=u.event_id"
        ),
        0,
        "usage event fields after {step}"
    );
    assert_eq!(
        differ("SELECT file_id,min_ts,max_ts FROM sessions", BOUNDS),
        0,
        "session bounds after {step}"
    );
}

fn write(path: &Path, lines: &[&str]) {
    fs::write(
        path,
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
}

fn append(path: &Path, lines: &[&str]) {
    let mut text = fs::read_to_string(path).unwrap();
    text.extend(lines.iter().map(|l| format!("{l}\n")));
    fs::write(path, text).unwrap();
}

/// What the steps move: the file holding the first copy of Claude call `t1`,
/// the outcome of Codex call `c1`, the outcomes of the calls in the two
/// Claude transcripts named `agent-x`, and the file of the record that
/// request `msg_1` reports.
type Seen = (String, Option<i64>, Option<i64>, Option<i64>, String);

/// Refreshes and checks the derived columns.
fn refresh(store: &mut Store, roots: &[Root], step: &str) -> Seen {
    store.refresh(roots, false, None).unwrap();
    assert_derived(store, step);
    let one = |sql: &str| -> Option<Option<i64>> {
        store
            .db
            .query_row(sql, [], |r| r.get(0))
            .optional()
            .unwrap()
    };
    let file = |sql: &str| -> String {
        let path: String = store.db.query_row(sql, [], |r| r.get(0)).unwrap();
        Path::new(&path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    (
        file("SELECT s.path FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE r.tool='Bash' AND r.canonical=1 AND s.agent='claude'"),
        one("SELECT call_ok FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE s.agent='codex' AND r.kind='tool_call'").flatten(),
        one("SELECT call_ok FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE s.path LIKE '%/claude/agent-x.jsonl' AND r.tool='Bash'").flatten(),
        one("SELECT call_ok FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE s.path LIKE '%/other/agent-x.jsonl' AND r.tool='Read'").flatten(),
        file("SELECT path FROM requests WHERE agent='claude'"),
    )
}

const ORIG_CALL: &str = r#"{"type":"assistant","uuid":"u2","sessionId":"a-orig","timestamp":"2026-10-01T00:00:01Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
const UNTIMED_COPY: &str = r#"{"type":"assistant","uuid":"u2","sessionId":"a-orig","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}"#;
const ORIG_FAILURE: &str = r#"{"type":"user","uuid":"u3","sessionId":"a-orig","timestamp":"2026-10-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"failed"}]}}"#;
const PARTIAL_REQUEST: &str = r#"{"type":"assistant","uuid":"m1","sessionId":"a-orig","timestamp":"2026-10-01T00:00:03Z","message":{"id":"msg_1","role":"assistant","model":"claude-model","content":[{"type":"thinking","thinking":"","signature":"x"}],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}}"#;
const FINISHED_REQUEST: &str = r#"{"type":"assistant","uuid":"m2","sessionId":"a-orig","timestamp":"2026-10-01T00:00:04Z","message":{"id":"msg_1","role":"assistant","model":"claude-model","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":2}}}"#;
const CODEX_META: &str = r#"{"type":"session_meta","timestamp":"2026-10-02T00:00:00Z","payload":{"id":"thread","source":"cli"}}"#;
const CODEX_CALL: &str = r#"{"type":"response_item","timestamp":"2026-10-02T00:00:00Z","payload":{"type":"function_call","call_id":"c1","name":"exec_command","arguments":"{\"cmd\":\"rg word\"}"}}"#;
const CODEX_RESULT: &str = r#"{"type":"response_item","timestamp":"2026-10-02T00:00:01Z","payload":{"type":"function_call_output","call_id":"c1","output":"Process exited with code 0"}}"#;

/// The largest file ID and that of `other/agent-x.jsonl`.
fn newest(store: &Store) -> (i64, i64) {
    store.db.query_row("SELECT (SELECT max(id) FROM files),(SELECT id FROM files WHERE path LIKE '%/other/agent-x.jsonl')", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
}

#[test]
fn derived_columns_equal_their_definitions_after_every_commit() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    let codex = dir.path().join("codex");
    let pi = dir.path().join("pi");
    for root in [&claude, &codex, &pi] {
        fs::create_dir(root).unwrap();
    }
    // Same native and call IDs under another agent must stay apart.
    write(
        &pi.join("p.jsonl"),
        &[
            r#"{"type":"session","id":"a-orig","cwd":"/synthetic"}"#,
            r#"{"type":"message","id":"u2","timestamp":"2026-09-01T00:00:00Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"t1","name":"bash","arguments":{"command":"ls"}}]}}"#,
            r#"{"type":"message","id":"t1","timestamp":"2026-09-01T00:00:01Z","message":{"role":"toolResult","toolCallId":"t1","isError":false,"content":"fine"}}"#,
            r#"{"type":"message","id":"msg_1","timestamp":"2026-09-01T00:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"input":1,"output":2}}}"#,
        ],
    );
    let roots = [
        Root {
            agent: Agent::Claude,
            path: claude.clone(),
        },
        Root {
            agent: Agent::Codex,
            path: codex.clone(),
        },
        Root {
            agent: Agent::Pi,
            path: pi.clone(),
        },
    ];
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();

    // A resumed transcript holding copied history is indexed before the
    // transcript it copied, which ranks first by path.
    write(
        &claude.join("b-copy.jsonl"),
        &[
            ORIG_CALL,
            ORIG_FAILURE,
            r#"{"type":"user","uuid":"u4","sessionId":"b-copy","timestamp":"2026-10-02T00:00:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"ok"}]}}"#,
            r#"{"type":"assistant","uuid":"u5","sessionId":"b-copy","timestamp":"2026-10-02T00:00:01Z","message":{"role":"assistant","content":[{"type":"server_tool_use","id":"t1","name":"web_search","input":{"query":"q"}}]}}"#,
            FINISHED_REQUEST,
        ],
    );
    // A Codex result recorded before its call, in another transcript of the
    // call's session; the call's transcript names its session only later.
    write(&codex.join("r1.jsonl"), &[CODEX_META, CODEX_RESULT]);
    write(&codex.join("r2.jsonl"), &[CODEX_CALL]);
    let mut seen = vec![refresh(&mut store, &roots, "indexing the copy first")];

    write(
        &claude.join("a-orig.jsonl"),
        &[
            r#"{"type":"user","uuid":"u1","sessionId":"a-orig","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"start"}}"#,
            ORIG_CALL,
            ORIG_FAILURE,
            PARTIAL_REQUEST,
        ],
    );
    seen.push(refresh(
        &mut store,
        &roots,
        "indexing the original after its copy",
    ));

    // A subagent transcript copies the call before it is identified or
    // timestamped, then ranks first once its start is known. Until then it
    // shares its placeholder session with another transcript of that name,
    // whose call reads its result and whose result its call reads.
    write(
        &claude.join("agent-x.jsonl"),
        &[
            UNTIMED_COPY,
            r#"{"type":"user","uuid":"x2","sessionId":"a-orig","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"failed"}]}}"#,
        ],
    );
    fs::create_dir(claude.join("other")).unwrap();
    write(
        &claude.join("other/agent-x.jsonl"),
        &[
            r#"{"type":"user","uuid":"o1","sessionId":"a-orig","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"ok"}]}}"#,
            r#"{"type":"assistant","uuid":"o2","sessionId":"a-orig","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"x"}}]}}"#,
            // Not a tool result, so not evidence for call `t1`.
            r#"{"type":"user","uuid":"o3","sessionId":"a-orig","message":{"role":"user","content":[{"type":"advisor_tool_result","tool_use_id":"t1","is_error":true,"content":"no"}]}}"#,
        ],
    );
    // A transcript named after its session, whose start alone arrives later.
    let untimed =
        |line: &str| line.replace(r#""sessionId":"a-orig""#, r#""sessionId":"c-untimed""#);
    write(
        &claude.join("c-untimed.jsonl"),
        &[
            &untimed(UNTIMED_COPY),
            &untimed(&FINISHED_REQUEST.replace(r#""timestamp":"2026-10-01T00:00:04Z","#, "")),
        ],
    );
    // The original finishes its request, which it ranks first for.
    append(&claude.join("a-orig.jsonl"), &[FINISHED_REQUEST]);
    seen.push(refresh(
        &mut store,
        &roots,
        "indexing untimed copies and finishing the original's request",
    ));
    append(
        &claude.join("agent-x.jsonl"),
        &[
            r#"{"type":"user","uuid":"s2","agentId":"sub","sessionId":"a-orig","timestamp":"2026-09-30T00:00:00Z","message":{"role":"user","content":"go"}}"#,
        ],
    );
    append(
        &claude.join("c-untimed.jsonl"),
        &[
            r#"{"type":"user","uuid":"c2","sessionId":"c-untimed","timestamp":"2026-09-29T00:00:00Z","message":{"role":"user","content":"later"}}"#,
        ],
    );
    append(&codex.join("r2.jsonl"), &[CODEX_META]);
    seen.push(refresh(
        &mut store,
        &roots,
        "a start time and session IDs that arrive in appends",
    ));

    // Replacing a file gives it a new file ID, never one used before.
    let replaced = newest(&store);
    assert_eq!(replaced.0, replaced.1);
    write(
        &claude.join("other/agent-x.jsonl"),
        &[
            r#"{"type":"user","uuid":"o0","sessionId":"a-orig","timestamp":"2026-11-01T00:00:00Z","message":{"role":"user","content":"rewritten"}}"#,
        ],
    );
    seen.push(refresh(&mut store, &roots, "replacing the newest file"));
    let replacement = newest(&store);
    assert!(replacement.1 > replaced.0 && replacement.0 == replacement.1);
    for file in [
        claude.join("a-orig.jsonl"),
        claude.join("c-untimed.jsonl"),
        codex.join("r1.jsonl"),
    ] {
        fs::remove_file(file).unwrap();
    }
    seen.push(refresh(
        &mut store,
        &roots,
        "deleting the first copy and the only result",
    ));
    // The result's sites come from a call in another transcript, which goes
    // first.
    write(&codex.join("r1.jsonl"), &[CODEX_META, CODEX_RESULT]);
    seen.push(refresh(&mut store, &roots, "restoring the result"));
    fs::remove_file(codex.join("r2.jsonl")).unwrap();
    seen.push(refresh(
        &mut store,
        &roots,
        "deleting a call whose result stays",
    ));

    assert_eq!(
        seen,
        [
            (
                "b-copy.jsonl".into(),
                None,
                None,
                None,
                "b-copy.jsonl".into()
            ),
            (
                "a-orig.jsonl".into(),
                None,
                None,
                None,
                "b-copy.jsonl".into()
            ),
            (
                "a-orig.jsonl".into(),
                None,
                Some(1),
                Some(0),
                "a-orig.jsonl".into()
            ),
            (
                "c-untimed.jsonl".into(),
                Some(1),
                None,
                None,
                "c-untimed.jsonl".into()
            ),
            (
                "c-untimed.jsonl".into(),
                Some(1),
                None,
                None,
                "c-untimed.jsonl".into()
            ),
            (
                "agent-x.jsonl".into(),
                None,
                None,
                None,
                "b-copy.jsonl".into()
            ),
            (
                "agent-x.jsonl".into(),
                Some(1),
                None,
                None,
                "b-copy.jsonl".into()
            ),
            (
                "agent-x.jsonl".into(),
                None,
                None,
                None,
                "b-copy.jsonl".into()
            ),
        ]
    );

    let mut clean = Store::open(&dir.path().join("clean.db")).unwrap();
    clean.refresh(&roots, true, None).unwrap();
    let derived = |store: &Store| -> Vec<String> {
        store.db.prepare("SELECT s.path||':'||r.line_no||':'||r.ordinal||':'||r.canonical||':'||coalesce(r.call_ok,'')||':'||coalesce(r.call_ok_source,'')||':'||coalesce((SELECT cs.path||':'||c.line_no||':'||c.ordinal FROM event_rows c JOIN sessions cs ON cs.file_id=c.file_id WHERE c.id=r.call_event_id),'')||':'||coalesce((SELECT selected FROM usage_rows WHERE event_id=r.id),'') FROM event_rows r JOIN sessions s ON s.file_id=r.file_id ORDER BY 1").unwrap()
            .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(derived(&store), derived(&clean));
}

#[test]
fn deriving_a_new_file_reads_only_the_keys_it_touches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("claude");
    fs::create_dir(&root).unwrap();
    write(
        &root.join("a-orig.jsonl"),
        &[ORIG_CALL, ORIG_FAILURE, PARTIAL_REQUEST],
    );
    let roots = [Root {
        agent: Agent::Claude,
        path: root.clone(),
    }];
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    store.refresh(&roots, false, None).unwrap();
    // Unrelated history, each row its own native event and call.
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<200000) INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,native_id,ts,role,role_source,kind,kind_source,tool,call_id,model_source,ok_source,line_role_first,canonical,call_ok_source) SELECT r.file_id,n.i+100,r.byte_off,r.byte_len,0,r.raw_hash,randomblob(32),r.ts,r.role,r.role_source,r.kind,r.kind_source,r.tool,randomblob(32),r.model_source,r.ok_source,1,1,r.call_ok_source FROM n JOIN event_rows r ON r.id=1;
     INSERT INTO usage_rows(event_id,file_id,ts,model,finished,selected,input,output) SELECT id,file_id,ts,model,1,1,1,1 FROM event_rows WHERE line_no>100;").unwrap();
    let steps = Arc::new(AtomicUsize::new(0));
    let counter = steps.clone();
    store
        .db
        .progress_handler(
            100,
            Some(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                false
            }),
        )
        .unwrap();
    // A copy of the call and its result, whose flags flip to second copy, a
    // finished record of a request that the original records unfinished, and
    // an event of its own: one native group per record and one call key.
    write(
        &root.join("b-copy.jsonl"),
        &[
            UNTIMED_COPY,
            ORIG_FAILURE,
            FINISHED_REQUEST,
            r#"{"type":"assistant","uuid":"u9","sessionId":"a-orig","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
        ],
    );
    // A refresh before a query, which never compacts the history.
    store
        .refresh(&roots, false, Some(std::time::Duration::from_secs(60)))
        .unwrap();
    let work = steps.load(Ordering::Relaxed);
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM event_rows WHERE tool='Bash' AND call_ok=0",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM event_rows r JOIN sessions s ON s.file_id=r.file_id WHERE s.path LIKE '%/b-copy.jsonl' AND r.canonical=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert!(
        store
            .db
            .query_row(
                "SELECT path LIKE '%/b-copy.jsonl' FROM requests WHERE line_no<100",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
    assert!(work < 1000, "{work} progress steps");
}

#[test]
fn appended_rows_enter_the_rollups_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("claude");
    fs::create_dir(&root).unwrap();
    let path = root.join("a-orig.jsonl");
    write(
        &path,
        &[
            r#"{"type":"user","uuid":"u1","sessionId":"a-orig","timestamp":"2026-10-01T00:00:00Z","message":{"role":"user","content":"start"}}"#,
        ],
    );
    let roots = [Root {
        agent: Agent::Claude,
        path: root.clone(),
    }];
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    store.refresh(&roots, false, None).unwrap();
    let first: i64 = store
        .db
        .query_row("SELECT max(id)+1 FROM event_rows", [], |r| r.get(0))
        .unwrap();
    let rollups = [sessidx::store::DAILY_COUNTS, sessidx::store::PROGRAM_COUNTS];
    store
        .db
        .execute_batch("CREATE TEMP TABLE upserts(n INTEGER); INSERT INTO upserts VALUES (0);")
        .unwrap();
    for rollup in &rollups {
        for event in ["INSERT", "UPDATE"] {
            store.db.execute_batch(&format!("CREATE TEMP TRIGGER {table}_{event} AFTER {event} ON main.{table} BEGIN UPDATE upserts SET n=n+1; END;", table = rollup.table)).unwrap();
        }
    }
    // Calls and their results in one append: settling gives every new call an
    // outcome and every new result its call, so all of them change.
    let lines: Vec<String> = (1..=3)
        .flat_map(|i| {
            [
                format!(r#"{{"type":"assistant","uuid":"c{i}","sessionId":"a-orig","timestamp":"2026-10-01T00:00:0{i}Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t{i}","name":"Bash","input":{{"command":"rg word{i}"}}}}]}}}}"#),
                format!(r#"{{"type":"user","uuid":"r{i}","sessionId":"a-orig","timestamp":"2026-10-01T00:00:0{i}Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t{i}","is_error":false,"content":"fine"}}]}}}}"#),
            ]
        })
        .collect();
    append(&path, &lines.iter().map(String::as_str).collect::<Vec<_>>());
    store
        .refresh(&roots, false, Some(std::time::Duration::from_secs(60)))
        .unwrap();
    assert_derived(&store, "append");
    assert_eq!(
        store
            .db
            .query_row(
                "SELECT count(*) FROM event_rows WHERE id>=? AND (call_ok=1 OR call_event_id IS NOT NULL)",
                [first],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        6
    );
    // One upsert per rollup row the new rows fall in, as one pass over them
    // with their settled values makes.
    let groups: i64 = rollups
        .iter()
        .map(|rollup| {
            store
                .db
                .query_row(
                    &format!(
                        "SELECT count(*) FROM ({})",
                        rollup.grouped(&format!("e.id>={first}"))
                    ),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
        })
        .sum();
    let upserts: i64 = store
        .db
        .query_row("SELECT n FROM upserts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(upserts, groups);
}
