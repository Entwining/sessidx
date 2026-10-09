use crate::common::snapshot;
use sessidx::{
    discovery::Root,
    model::Harness,
    query::{self, Filters},
    source,
    store::Store,
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const FIXTURE: &str = include_str!("../fixtures/codex.jsonl");

fn compress(data: &[u8]) -> Vec<u8> {
    zstd::stream::encode_all(data, 3).unwrap()
}

fn codex(dir: &Path) -> (Store, Vec<Root>, PathBuf) {
    let root = dir.join("sessions");
    fs::create_dir(&root).unwrap();
    let store = Store::open(&dir.join("index.db")).unwrap();
    let plain = root.join("rollout.jsonl");
    (
        store,
        vec![Root {
            harness: Harness::Codex,
            path: root,
        }],
        plain,
    )
}

fn zst(plain: &Path) -> PathBuf {
    plain.with_extension("jsonl.zst")
}

fn files(store: &Store) -> Vec<String> {
    store
        .db
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The same records indexed from a plain file outside the roots under test.
fn reference(dir: &Path) -> Store {
    let path = dir.join("reference.jsonl");
    fs::write(&path, FIXTURE).unwrap();
    let mut store = Store::open(&dir.join("reference.db")).unwrap();
    store
        .refresh(
            &[Root {
                harness: Harness::Codex,
                path,
            }],
            false,
            None,
        )
        .unwrap();
    store
}

fn grepped(store: &Store, pattern: &str) -> Vec<String> {
    let filters = Filters {
        harness: vec![Harness::Codex],
        ..Filters::default()
    };
    let (hits, coverage) = query::scan(
        &store.db,
        pattern,
        &filters,
        20,
        0,
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(coverage.unavailable_ranges, 0);
    hits.into_iter().map(|h| h.snippet).collect()
}

fn shown(store: &Store, target: &str) -> Vec<String> {
    let (hits, coverage) = query::show(&store.db, target, 0, 100, 0).unwrap();
    assert_eq!(coverage.unavailable_ranges, 0);
    hits.into_iter().map(|h| h.snippet).collect()
}

#[test]
fn a_compressed_rollout_is_indexed_shown_and_grepped_under_its_plain_path() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, roots, plain) = codex(dir.path());
    fs::write(zst(&plain), compress(FIXTURE.as_bytes())).unwrap();
    let refresh = store.refresh(&roots, false, None).unwrap();
    assert_eq!(refresh.parse_errors, 0);
    assert_eq!(files(&store), [plain.to_str().unwrap()]);
    let reference = reference(dir.path());
    assert_eq!(snapshot(&store), snapshot(&reference));
    let records = shown(&store, "codex-fixture");
    assert!(records.iter().any(|r| r.contains("太长")));
    assert_eq!(records, shown(&reference, "codex-fixture"));
    let hits = grepped(&store, "太长");
    assert!(!hits.is_empty());
    assert_eq!(hits, grepped(&reference, "太长"));
    // Decoded size differs from the compressed file's size, which must not make
    // every later refresh reindex it.
    assert_eq!(store.refresh(&roots, false, None).unwrap().files_changed, 0);
}

#[test]
fn compression_and_materialization_keep_one_file_that_equals_a_clean_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, roots, plain) = codex(dir.path());
    fs::write(&plain, FIXTURE).unwrap();
    store.refresh(&roots, false, None).unwrap();
    let before = snapshot(&store);
    // Codex publishes the compressed form before removing the plain one.
    fs::write(zst(&plain), compress(FIXTURE.as_bytes())).unwrap();
    assert_eq!(store.refresh(&roots, false, None).unwrap().files_changed, 0);
    fs::remove_file(&plain).unwrap();
    store.refresh(&roots, false, None).unwrap();
    assert_eq!(files(&store), [plain.to_str().unwrap()]);
    assert_eq!(snapshot(&store), before);
    // Resuming the thread restores the plain file and appends to it.
    let appended = format!(
        "{FIXTURE}{}\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"resumedneedle"}]}}"#
    );
    fs::write(&plain, &appended).unwrap();
    fs::remove_file(zst(&plain)).unwrap();
    store.refresh(&roots, false, None).unwrap();
    assert_eq!(files(&store), [plain.to_str().unwrap()]);
    let mut clean = Store::open(&dir.path().join("clean.db")).unwrap();
    clean.refresh(&roots, true, None).unwrap();
    assert_eq!(snapshot(&store), snapshot(&clean));
    assert!(
        shown(&store, "codex-fixture")
            .iter()
            .any(|r| r.contains("resumedneedle"))
    );
}

#[test]
fn every_frame_of_a_multi_frame_rollout_is_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, roots, plain) = codex(dir.path());
    let (head, tail) = FIXTURE.split_at(FIXTURE.find("{\"type\":\"response_item\"").unwrap());
    let mut frames = compress(head.as_bytes());
    frames.extend(compress(tail.as_bytes()));
    fs::write(zst(&plain), frames).unwrap();
    store.refresh(&roots, false, None).unwrap();
    assert_eq!(snapshot(&store), snapshot(&reference(dir.path())));
}

#[test]
fn an_undecodable_rollout_is_a_parse_error_that_does_not_stop_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, roots, plain) = codex(dir.path());
    let compressed = compress(FIXTURE.as_bytes());
    fs::write(zst(&plain), b"not a zstd frame\n").unwrap();
    let truncated = plain.with_file_name("truncated.jsonl");
    fs::write(zst(&truncated), &compressed[..compressed.len() - 8]).unwrap();
    let readable = plain.with_file_name("readable.jsonl");
    fs::write(&readable, FIXTURE).unwrap();
    let refresh = store.refresh(&roots, false, None).unwrap();
    assert_eq!(refresh.parse_errors, 2);
    assert!(!refresh.stale);
    assert_eq!(files(&store).len(), 3);
    let invalid: u64 = store
        .db
        .query_row(
            "SELECT sum(n) FROM shapes WHERE signature='invalid_zstd' AND known=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(invalid, 2);
    assert!(
        shown(&store, &format!("{}:5", readable.display()))[0].contains("太长"),
        "the readable rollout after the undecodable ones is indexed"
    );
    // An undecodable file is not decoded again until it changes.
    assert_eq!(store.refresh(&roots, false, None).unwrap().files_changed, 0);
}

#[test]
fn only_jsonl_and_jsonl_zst_files_are_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, roots, plain) = codex(dir.path());
    let compressed = compress(FIXTURE.as_bytes());
    // Codex's in-flight temporary files and unrelated compressed files.
    for name in [
        "rollout-compress-0.tmp",
        "rollout.jsonl.decompress.1.2.tmp",
        ".rollout.jsonl.paginated.tmp",
        "notes.zst",
    ] {
        fs::write(plain.with_file_name(name), &compressed).unwrap();
    }
    store.refresh(&roots, false, None).unwrap();
    assert!(files(&store).is_empty());
}

#[test]
fn decoded_records_resume_at_an_offset_and_ranges_reread_backwards() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("rollout.jsonl");
    assert!(source::open(&plain).unwrap().is_none());
    fs::write(zst(&plain), compress(FIXTURE.as_bytes())).unwrap();
    let offset = FIXTURE.find('\n').unwrap() + 1;
    let mut rest = String::new();
    source::open(&plain)
        .unwrap()
        .unwrap()
        .records_from(offset as u64)
        .unwrap()
        .read_to_string(&mut rest)
        .unwrap();
    assert_eq!(rest, FIXTURE[offset..]);
    let mut ranges = source::open(&plain).unwrap().unwrap().ranges();
    for (start, len) in [(offset, 20), (3, 10), (offset + 40, 5)] {
        let mut bytes = vec![0; len];
        ranges.read_exact_at(&mut bytes, start as u64).unwrap();
        assert_eq!(bytes, FIXTURE.as_bytes()[start..start + len]);
    }
    let mut past_end = [0; 4];
    assert!(
        ranges
            .read_exact_at(&mut past_end, FIXTURE.len() as u64 - 2)
            .is_err()
    );
    // An offset past the decoded end means the stored progress does not belong
    // to this file, which must not read as a file with nothing left to index.
    assert!(
        source::open(&plain)
            .unwrap()
            .unwrap()
            .records_from(FIXTURE.len() as u64 + 1)
            .is_err()
    );
}

#[test]
fn a_rollout_restored_between_two_opens_is_not_missing() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("rollout.jsonl");
    fs::write(zst(&plain), compress(FIXTURE.as_bytes())).unwrap();
    let mut opens = 0;
    let source = source::open_with(&plain, |path| {
        opens += 1;
        let opened = fs::File::open(path);
        if opens == 1 {
            // Codex resumes the thread after the plain file was found missing:
            // it publishes the plain file, then removes the compressed one.
            fs::write(&plain, FIXTURE).unwrap();
            fs::remove_file(zst(&plain)).unwrap();
        }
        opened
    })
    .unwrap();
    assert!(source.is_some_and(|s| !s.compressed));
}

#[test]
fn an_unreadable_plain_file_is_an_error_not_a_missing_one() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("rollout.jsonl");
    fs::write(&plain, FIXTURE).unwrap();
    fs::write(zst(&plain), compress(FIXTURE.as_bytes())).unwrap();
    fs::set_permissions(&plain, fs::Permissions::from_mode(0o000)).unwrap();
    // Falling back to the sibling or to "missing" would index or delete the
    // session on the strength of a file the harness has not removed.
    assert_eq!(
        source::open(&plain).err().map(|e| e.kind()),
        Some(std::io::ErrorKind::PermissionDenied)
    );
}
