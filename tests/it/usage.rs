use crate::common::indexed_files;
use rusqlite::types::Value;
use sessidx::{
    counting::{self, By, Metric},
    discovery::Root,
    model::Agent,
    query::Filters,
    store::Store,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// The rows of a query as text, one cell after another.
fn rows(store: &Store, sql: &str) -> Vec<String> {
    let mut stmt = store.db.prepare(sql).unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| {
        Ok((0..n)
            .map(|i| match r.get::<_, Value>(i).unwrap() {
                Value::Null => "-".to_owned(),
                Value::Integer(v) => v.to_string(),
                Value::Text(v) => v,
                v => format!("{v:?}"),
            })
            .collect::<Vec<_>>()
            .join(" "))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn claude() -> (tempfile::TempDir, Store, Vec<sessidx::discovery::Root>) {
    indexed_files(
        Agent::Claude,
        &[(
            "usage-claude",
            include_str!("../fixtures/usage-claude.jsonl"),
        )],
    )
}

#[test]
fn claude_requests_take_their_final_record_and_list_passes() {
    let (_dir, store, _) = claude();
    // The finished record of `msg_a`, not its first nor the larger value of
    // each counter, and the last of `msg_e`, which no record finishes; the
    // synthetic placeholder is no request.
    assert_eq!(
        rows(
            &store,
            "SELECT line_no,model,input,cache_read,cache_write,cache_write_5m,cache_write_1h,output,reasoning,finished FROM requests ORDER BY line_no"
        ),
        [
            "3 claude-model 3 100 50 30 10 40 12 1",
            "4 claude-model 5 10 0 - - 7 - 1",
            "5 gateway/other-model 2 - - - - 3 - 1",
            "10 claude-model 1 - - - - 6 - 0",
            "11 claude-model 2 - - - - 4 - 1",
        ]
    );
    assert_eq!(
        rows(&store, "SELECT count(*) FROM usage_rows"),
        ["7"],
        "every record of a message keeps its usage row"
    );
    // A pass without a model is the executor's, and its tokens are its own,
    // not the request's; a single pass, or a pass list with a pass that does
    // not parse, is the request.
    assert_eq!(
        rows(
            &store,
            "SELECT r.line_no,p.pass,p.kind,p.model,p.input,p.cache_read,p.cache_write,p.cache_write_5m,p.cache_write_1h,p.output,p.reasoning FROM request_passes p JOIN requests r ON r.id=p.request_id ORDER BY 1,2"
        ),
        [
            "3 0 - claude-model 3 100 50 30 10 40 12",
            "4 0 message claude-model 4 10 6 4 2 5 -",
            "4 1 advisor_message advisor-model 30 0 0 - - 9 -",
            "4 2 message claude-model 1 0 0 - - 2 -",
            "5 0 - gateway/other-model 2 - - - - 3 -",
            "10 0 - claude-model 1 - - - - 6 -",
            "11 0 - claude-model 2 - - - - 4 -",
        ]
    );
}

#[test]
fn codex_requests_come_from_records_or_moved_totals() {
    let (_dir, store, _) = indexed_files(
        Agent::Codex,
        &[("usage-codex", include_str!("../fixtures/usage-codex.jsonl"))],
    );
    // Moved running totals before the first record, then one request per
    // record; a repeated total, an empty one, and a total after the first
    // record are not requests.
    assert_eq!(
        rows(
            &store,
            "SELECT line_no,model,input,cache_read,cache_write,output,reasoning,native_id IS NOT NULL FROM requests ORDER BY line_no"
        ),
        [
            "4 gpt-model 60 20 - 20 5 0",
            "6 gpt-model 10 30 - 10 0 0",
            "7 gpt-model 90 100 10 30 10 1",
            "9 gpt-model 50 250 0 5 0 1",
        ]
    );
}

#[test]
fn pi_entries_are_requests_and_cache_warm_is_not() {
    let (_dir, store, _) = indexed_files(
        Agent::Pi,
        &[("usage-pi", include_str!("../fixtures/usage-pi.jsonl"))],
    );
    // An errored attempt records zeros; reasoning above output is kept as
    // stated.
    assert_eq!(
        rows(
            &store,
            "SELECT line_no,model,input,cache_read,cache_write,cache_write_5m,cache_write_1h,output,reasoning FROM requests ORDER BY line_no"
        ),
        [
            "3 pi-model 10 100 20 - 20 4 3",
            "4 pi-model 0 0 0 - - 0 -",
            "5 pi-model 1 0 0 - - 3 9",
        ]
    );
}

#[test]
fn compactions_count_once_with_their_trigger_and_sizes() {
    let claude = include_str!("../fixtures/usage-claude.jsonl");
    // A resumed transcript copies the boundaries it continues from; another
    // system record is no compaction.
    let copied: String = claude
        .lines()
        .filter(|l| l.contains("compact_boundary"))
        .map(|l| {
            l.replace(
                "\"sessionId\":\"usage-claude\"",
                "\"sessionId\":\"usage-resumed\"",
            ) + "\n"
        })
        .collect();
    let (_dir, store, _) = indexed_files(
        Agent::Claude,
        &[("usage-claude", claude), ("usage-resumed", &copied)],
    );
    assert_eq!(
        rows(
            &store,
            "SELECT trigger,pre_tokens,post_tokens,model,summary_input FROM compactions ORDER BY id"
        ),
        [
            "auto 150000 20000 gateway/other-model -",
            "manual 90000 - gateway/other-model -"
        ]
    );
    let codex = include_str!("../fixtures/usage-codex.jsonl");
    let window = codex.lines().find(|l| l.contains("window-1")).unwrap();
    let (_dir, store, _) = indexed_files(
        Agent::Codex,
        &[
            ("usage-codex", codex),
            (
                "usage-fork",
                &format!(
                    "{}\n{window}\n",
                    r#"{"type":"session_meta","timestamp":"2026-10-06T00:00:00Z","payload":{"id":"codex-fork","source":"cli"}}"#
                ),
            ),
        ],
    );
    // The window ID dedupes the copy; a compaction item is not another one.
    assert_eq!(
        rows(
            &store,
            "SELECT line_no,trigger,pre_tokens FROM compactions ORDER BY id"
        ),
        ["10 unknown -", "12 unknown -"]
    );
    let (_dir, store, _) = indexed_files(
        Agent::Pi,
        &[("usage-pi", include_str!("../fixtures/usage-pi.jsonl"))],
    );
    assert_eq!(
        rows(
            &store,
            "SELECT trigger,pre_tokens,post_tokens,summary_input,summary_output,summary_cache_read,summary_cache_write FROM compactions"
        ),
        ["unknown 120000 - 500 50 7 3"]
    );
}

#[test]
fn usage_counts_equal_the_public_views() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("index.db")).unwrap();
    let roots: Vec<Root> = [
        (
            Agent::Claude,
            "usage-claude",
            include_str!("../fixtures/usage-claude.jsonl"),
        ),
        (
            Agent::Codex,
            "usage-codex",
            include_str!("../fixtures/usage-codex.jsonl"),
        ),
        (
            Agent::Pi,
            "usage-pi",
            include_str!("../fixtures/usage-pi.jsonl"),
        ),
    ]
    .into_iter()
    .map(|(agent, name, content)| {
        let path = dir.path().join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join(format!("{name}.jsonl")), content).unwrap();
        Root { agent, path }
    })
    .collect();
    store.refresh(&roots, false, None).unwrap();
    let count = |metric: Metric, filters: &Filters| -> Vec<serde_json::Value> {
        counting::count(&store.db, metric, &metric.default_by(), None, filters)
            .unwrap()
            .into_iter()
            .map(|mut row| {
                row.as_object_mut().unwrap().remove("metric");
                row
            })
            .collect()
    };
    let grouped = |fields: &str, records: &str| -> Vec<serde_json::Value> {
        rows(
            &store,
            &format!(
                "SELECT json_object('agent',agent,'model',m,'week',w,{fields})
                 FROM (SELECT *,coalesce(model,'unknown') AS m,coalesce(strftime('%G-W%V',substr(ts,1,10)),'unknown') AS w FROM ({records}))
                 GROUP BY agent,m,w ORDER BY agent,m,w"
            ),
        )
        .iter()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
    };
    for (filters, window) in [
        (Filters::default(), "1=1"),
        (
            Filters {
                since: Some("2026-10-05T00:00:03Z".parse().unwrap()),
                until: Some("2026-10-06T00:00:03Z".parse().unwrap()),
                ..Filters::default()
            },
            "ts>='2026-10-05T00:00:03.000Z' AND ts<'2026-10-06T00:00:03.000Z'",
        ),
    ] {
        // Requests under their model, and tokens under each pass's, grouped as
        // the counts are by default.
        assert_eq!(
            count(Metric::Usage, &filters),
            grouped(
                "'requests',sum(requests),'passes',sum(passes),
                 'input',coalesce(sum(input),0),'cache_read',coalesce(sum(cache_read),0),'cache_write',coalesce(sum(cache_write),0),
                 'cache_write_5m',coalesce(sum(cache_write_5m),0),'cache_write_1h',coalesce(sum(cache_write_1h),0),
                 'output',coalesce(sum(output),0),'reasoning',coalesce(sum(reasoning),0),
                 'cache_read_recorded',count(cache_read),'cache_write_recorded',count(cache_write),
                 'cache_write_5m_recorded',count(cache_write_5m),'cache_write_1h_recorded',count(cache_write_1h),
                 'reasoning_recorded',count(reasoning)",
                &format!(
                    "SELECT agent,model,ts,1 AS requests,0 AS passes,NULL AS input,NULL AS cache_read,NULL AS cache_write,NULL AS cache_write_5m,
                      NULL AS cache_write_1h,NULL AS output,NULL AS reasoning FROM requests WHERE {window}
                     UNION ALL SELECT agent,model,ts,0,1,input,cache_read,cache_write,cache_write_5m,cache_write_1h,output,reasoning FROM request_passes WHERE {window}"
                )
            ),
            "{window}"
        );
        assert_eq!(
            count(Metric::Compactions, &filters),
            grouped(
                "'compactions',count(*),'auto',sum(trigger='auto'),'manual',sum(trigger='manual'),'trigger_unknown',sum(trigger='unknown'),
                 'pre_tokens',coalesce(sum(pre_tokens),0),'pre_tokens_recorded',count(pre_tokens),
                 'post_tokens',coalesce(sum(post_tokens),0),'post_tokens_recorded',count(post_tokens)",
                &format!("SELECT * FROM compactions WHERE {window}")
            ),
            "{window}"
        );
    }
    // The CLI checks before refreshing; the library refuses on its own too.
    assert!(
        counting::count(
            &store.db,
            Metric::Usage,
            &[By::Role],
            None,
            &Filters::default()
        )
        .is_err()
    );
}

#[test]
fn usage_count_work_follows_its_records() {
    let (_dir, store, _) = indexed_files(
        Agent::Claude,
        &[
            (
                "usage-claude",
                include_str!("../fixtures/usage-claude.jsonl"),
            ),
            (
                "usage-other",
                concat!(
                    r#"{"type":"user","uuid":"o1","sessionId":"usage-other","timestamp":"2026-10-05T00:00:00Z","message":{"role":"user","content":"start"}}"#,
                    "\n"
                ),
            ),
        ],
    );
    // Earlier events of another session, each a request, outside the window.
    store.db.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<200000) INSERT INTO event_rows(file_id,line_no,byte_off,byte_len,ordinal,raw_hash,ts,role,role_source,kind,kind_source,model_source,ok_source,line_role_first,canonical) SELECT r.file_id,n.i+100,r.byte_off,r.byte_len,0,r.raw_hash,'2020-01-01T00:00:00.000Z',r.role,r.role_source,r.kind,r.kind_source,r.model_source,r.ok_source,1,1 FROM n JOIN event_rows r ON r.id=(SELECT min(id) FROM event_rows WHERE file_id=(SELECT file_id FROM sessions WHERE session_id='usage-other'));
     INSERT INTO usage_rows(event_id,file_id,ts,model,finished,selected,input,output) SELECT id,file_id,ts,'old-model',1,1,1,1 FROM event_rows WHERE line_no>100;").unwrap();
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
    let work = |metric, by: &[By], filters: &Filters| {
        steps.store(0, Ordering::Relaxed);
        let rows = counting::count(&store.db, metric, by, None, filters).unwrap();
        (rows, steps.load(Ordering::Relaxed))
    };
    let window = Filters {
        since: Some("2026-10-01T00:00:00Z".parse().unwrap()),
        ..Filters::default()
    };
    let (rows, windowed) = work(Metric::Usage, &[By::Model], &window);
    assert!(rows.iter().all(|row| row["model"] != "old-model"));
    assert!(windowed < 100, "{windowed} progress steps");
    let session = Filters {
        session: Some("usage-claude".into()),
        ..Filters::default()
    };
    let (rows, narrowed) = work(Metric::Usage, &[By::Model], &session);
    assert!(rows.iter().all(|row| row["model"] != "old-model"));
    assert!(narrowed < 100, "{narrowed} progress steps");
    // Compactions are found from their own rows, not among the events.
    let (rows, compactions) = work(Metric::Compactions, &[], &Filters::default());
    assert_eq!(rows[0]["compactions"], 2);
    assert!(compactions < 100, "{compactions} progress steps");
    // The whole history reads each request once, with its session by row ID:
    // fewer than 100 instructions per request, where indexing the requests by
    // session first takes more.
    let (_, whole) = work(Metric::Usage, &[], &Filters::default());
    assert!(whole < 200_000, "{whole} progress steps");
}
