use crate::common::{serial, sessidx, snapshot, stream};
use sessidx::{discovery::Root, model::Harness, store::Store};
use std::{fs, os::unix::fs::PermissionsExt};

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
        harness: Harness::Codex,
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
    assert_eq!(upgrade.db.query_row("SELECT count(*) FROM locations WHERE typeof(raw_hash)!='blob' OR length(raw_hash)!=32 OR (native_id IS NOT NULL AND length(native_id)!=32)", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(upgrade.db.query_row("SELECT count(*) FROM event_details WHERE call_id IS NOT NULL AND (typeof(call_id)!='blob' OR length(call_id)!=32)", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(
        upgrade
            .db
            .query_row("SELECT count(*) FROM locations", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        upgrade
            .db
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap()
    );
    assert_eq!(
        upgrade
            .db
            .query_row("SELECT count(*) FROM event_details", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        upgrade
            .db
            .query_row(
                "SELECT count(*) FROM events WHERE kind!='context'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
    );
}

#[test]
fn previous_compact_schema_rebuilds_without_reading_new_columns() {
    for schema in [
        include_str!("../fixtures/schema-v2.sql"),
        include_str!("../fixtures/schema-v3.sql"),
        include_str!("../fixtures/schema-v4.sql"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old-compact.db");
        let old = rusqlite::Connection::open(&path).unwrap();
        old.execute_batch(schema).unwrap();
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
        harness: Harness::Codex,
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
