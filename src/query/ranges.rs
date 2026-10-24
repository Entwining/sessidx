use super::page::Hit;
use crate::store::MAX_RECORD;
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::{os::unix::fs::MetadataExt, path::Path};

/// `source` keeps the last opened file with its device and inode: consecutive
/// rows usually share it. It is reused only while the path still names that
/// file, so a source replaced mid-query is read as reopening it would.
pub(super) fn read_range(
    db: &Connection,
    item: &Hit,
    source: &mut Option<((u64, u64), crate::source::Ranges)>,
) -> Result<Vec<u8>> {
    anyhow::ensure!(
        item.byte_len <= MAX_RECORD as u64,
        "source record exceeds display limit"
    );
    let expected: (u64, u64, [u8; 32]) = db
        .prepare_cached(
            "SELECT f.dev,f.inode,r.raw_hash FROM event_rows r JOIN files f ON f.id=r.file_id WHERE r.id=?",
        )?
        .query_row([item.event_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let named = crate::source::metadata(Path::new(&item.path))
        .context("source unreadable; run sessidx index")?;
    let (identity, ranges) = match source {
        Some((identity, ranges)) if *identity == (named.dev(), named.ino()) => (*identity, ranges),
        _ => {
            let opened = crate::source::open(Path::new(&item.path))
                .context("source unreadable; run sessidx index")?
                .context("source removed; run sessidx index")?;
            let identity = (opened.meta.dev(), opened.meta.ino());
            (identity, &mut source.insert((identity, opened.ranges())).1)
        }
    };
    anyhow::ensure!(
        identity == (expected.0, expected.1),
        "source replaced; run sessidx index"
    );
    let mut bytes = vec![0; item.byte_len as usize];
    ranges
        .read_exact_at(&mut bytes, item.byte_off)
        .context("source truncated; run sessidx index")?;
    anyhow::ensure!(
        Sha256::digest(&bytes)[..] == expected.2,
        "source range changed; run sessidx index"
    );
    Ok(bytes)
}

pub(super) fn display_range(bytes: &[u8]) -> Result<Json> {
    let mut value: Json =
        serde_json::from_slice(bytes).context("source range changed; run sessidx index")?;
    crate::redaction::redact_value(&mut value);
    Ok(value)
}
