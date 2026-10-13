use crate::common::{indexed, serial, sessidx, snapshot, stream};
use sessidx::{
    discovery::Root,
    model::Agent,
    store::{SCHEMA_VERSION, Store},
};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

#[test]
fn table_changes_raise_the_schema_version() {
    let tables = |schema: &str| {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(schema).unwrap();
        db.prepare("SELECT type,name,sql FROM sqlite_master WHERE type NOT IN ('view','index') UNION ALL SELECT 'user_version','',CAST(user_version AS TEXT) FROM pragma_user_version ORDER BY 1,2")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let path = format!(
        "{}/tests/fixtures/schema-v{SCHEMA_VERSION}.sql",
        env!("CARGO_MANIFEST_DIR")
    );
    let fixture = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(
        tables(include_str!("../../src/schema.sql")),
        tables(&fixture)
    );
}

#[test]
fn changed_views_and_indexes_are_rebuilt_without_reindexing() {
    let (_dir, mut store, roots) = indexed(Agent::Claude, include_str!("../fixtures/claude.jsonl"));
    let definitions = |store: &Store| {
        store
            .db
            .prepare("SELECT type,name,sql FROM sqlite_master WHERE type IN ('view','index') AND sql IS NOT NULL ORDER BY 1,2")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let file_ids = |store: &Store| {
        store
            .db
            .prepare("SELECT id FROM files ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let expected = definitions(&store);
    let indexed_ids = file_ids(&store);
    // Definitions as an earlier release created them, and a new transcript.
    store
        .db
        .execute_batch(
            "DROP VIEW call_outcomes; DROP VIEW canonical_events; CREATE VIEW canonical_events AS SELECT 1 AS id;
             DROP INDEX events_call; CREATE INDEX events_extra ON event_rows(model);",
        )
        .unwrap();
    let earlier = definitions(&store);
    fs::write(
        roots[0].path.join("other.jsonl"),
        include_str!("../fixtures/claude.jsonl"),
    )
    .unwrap();
    let error = Store::require_schema(&store.db).unwrap_err().to_string();
    assert!(error.contains("run sessidx index"), "{error}");

    let report = store
        .refresh(&roots, false, Some(Duration::from_secs(60)))
        .unwrap();
    assert!(report.stale && report.continuation.is_some(), "{report:?}");
    assert_eq!(report.files_changed, 0);
    Store::require_schema(&store.db).unwrap();
    let is_view = |d: &&(String, String, String)| d.0 == "view";
    assert_eq!(
        definitions(&store)
            .iter()
            .filter(is_view)
            .collect::<Vec<_>>(),
        expected.iter().filter(is_view).collect::<Vec<_>>()
    );
    assert_eq!(
        definitions(&store)
            .iter()
            .filter(|d| !is_view(d))
            .collect::<Vec<_>>(),
        earlier.iter().filter(|d| !is_view(d)).collect::<Vec<_>>()
    );

    let report = store.refresh(&roots, false, None).unwrap();
    assert!(!report.stale, "{report:?}");
    assert_eq!(report.files_changed, 1);
    assert_eq!(definitions(&store), expected);
    assert_eq!(file_ids(&store)[..indexed_ids.len()], indexed_ids);
    let report = store
        .refresh(&roots, false, Some(Duration::from_secs(60)))
        .unwrap();
    assert!(!report.stale && report.files_changed == 0, "{report:?}");
}

#[test]
fn previous_schema_requires_explicit_locked_rebuild() {
    let serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let old = rusqlite::Connection::open(&path).unwrap();
    old.execute_batch(include_str!("../fixtures/schema-v1.sql"))
        .unwrap();
    for args in [
        vec!["search", "needle"],
        vec!["sql", "SELECT 1"],
        vec!["index"],
    ] {
        let out = sessidx(&serial, &path, &[], &args).output().unwrap();
        let rows = stream(&out, 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["complete"], false);
        assert_eq!(
            rows[0]["error"],
            "database schema changed; run sessidx index --full"
        );
        assert!(out.stderr.is_empty());
    }
    assert!(Store::open(&path).is_err());
    let root = dir.path().join("logs");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("one.jsonl"),
        include_str!("../fixtures/codex.jsonl"),
    )
    .unwrap();
    let roots = [Root {
        agent: Agent::Codex,
        path: root,
    }];
    let mut upgrade = Store::open_for_rebuild(&path).unwrap();
    let lock = upgrade.lock().unwrap().unwrap();
    let report = upgrade.refresh(&roots, true, None).unwrap();
    assert!(report.writer_busy && report.stale);
    assert_eq!(
        old.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(lock);
    assert!(!upgrade.refresh(&roots, true, None).unwrap().stale);
    assert_eq!(
        upgrade
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        sessidx::store::SCHEMA_VERSION
    );
    let mut clean = Store::open(&dir.path().join("clean.db")).unwrap();
    clean.refresh(&roots, true, None).unwrap();
    assert_eq!(snapshot(&upgrade), snapshot(&clean));
    assert_eq!(upgrade.db.query_row("SELECT count(*) FROM event_rows WHERE typeof(raw_hash)!='blob' OR length(raw_hash)!=32 OR (native_id IS NOT NULL AND length(native_id)!=32) OR (call_id IS NOT NULL AND (typeof(call_id)!='blob' OR length(call_id)!=32))", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(
        upgrade
            .db
            .query_row("SELECT count(*) FROM event_rows", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        upgrade
            .db
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap()
    );
}

#[test]
fn previous_compact_schema_rebuilds_without_reading_new_columns() {
    for version in 2..SCHEMA_VERSION {
        let schema = fs::read_to_string(format!(
            "{}/tests/fixtures/schema-v{version}.sql",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old-compact.db");
        let old = rusqlite::Connection::open(&path).unwrap();
        old.execute_batch(&schema).unwrap();
        assert!(Store::open(&path).is_err());
        assert!(Store::require_schema(&old).is_err());
        let mut upgrade = Store::open_for_rebuild(&path).unwrap();
        upgrade.refresh(&[], true, None).unwrap();
        Store::require_schema(&upgrade.db).unwrap();
        let n: i64 = upgrade
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE text_truncated=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }
}

#[test]
fn initial_schema_creation_respects_the_writer_lock() {
    let serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache").join("index.db");
    let store = Store::open(&path).unwrap();
    let mode = fs::metadata(dir.path().join("cache"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    let lock = store.lock().unwrap().unwrap();
    let mut second = Store::open(&path).unwrap();
    let r = second.refresh(&[], false, None).unwrap();
    assert!(r.writer_busy && r.stale);
    let roots = [Root {
        agent: Agent::Codex,
        path: dir.path().into(),
    }];
    let rows = stream(
        &sessidx(&serial, &path, &roots, &["index"])
            .output()
            .unwrap(),
        2,
    );
    assert_eq!(rows[0]["type"], "index");
    assert_eq!(rows[0]["writer_busy"], true);
    assert_eq!(rows[0]["stale"], true);
    assert!(rows.last().unwrap().get("error").is_none());
    assert_eq!(
        second
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(lock);
    second.refresh(&[], false, None).unwrap();
    assert_eq!(
        second
            .db
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        sessidx::store::SCHEMA_VERSION
    );
}
