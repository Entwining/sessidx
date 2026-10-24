mod derived;
mod files;
mod lock;
mod rollups;
mod schema;

use crate::{
    discovery::{self, Root},
    model::{Agent, name},
};
use anyhow::{Context, Result};
use derived::{delete_file, settle};
use files::FileCoverage;
pub use lock::WriterLock;
pub use rollups::{DAILY_COUNTS, PROGRAM_COUNTS, Rollup};
use rusqlite::Connection;
use schema::schema_version;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const MAX_RECORD: usize = 16 * 1024 * 1024;
pub const SCHEMA_VERSION: i64 = 14;
/// The version of how records become rows: adapters, normalization,
/// redaction, outcomes, denials and shell sites. A file indexed by another
/// version is reindexed like a changed file, so that a parser change reaches
/// every row without a rebuild. Rule: docs/formats.md `parser_version`.
pub const PARSER_VERSION: i64 = 5;

#[derive(Default, Debug, Serialize)]
pub struct Refresh {
    pub deferred_tails: usize,
    pub parse_errors: u64,
    pub files_changed: usize,
    pub records: usize,
    pub stale: bool,
    pub continuation: Option<String>,
    pub missing_roots: Vec<String>,
    pub writer_busy: bool,
}

pub struct Store {
    pub db: Connection,
    pub path: PathBuf,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_mode(path, false)
    }

    pub fn open_for_rebuild(path: &Path) -> Result<Self> {
        Self::open_mode(path, true)
    }

    fn open_mode(path: &Path, rebuilding: bool) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(50))?;
        schema_version(&db, rebuilding)?;
        Ok(Self {
            db,
            path: path.into(),
        })
    }

    pub fn lock(&self, deadline: Option<Instant>) -> Result<Option<WriterLock>> {
        WriterLock::acquire(&self.path, false, deadline)
    }

    pub fn read_lock(path: &Path) -> Result<Option<WriterLock>> {
        WriterLock::acquire(path, true, None)
    }

    pub fn refresh(
        &mut self,
        roots: &[Root],
        full: bool,
        budget: Option<Duration>,
    ) -> Result<Refresh> {
        let deadline = budget.map(|d| Instant::now() + d);
        let Some(_lock) = self.lock(deadline)? else {
            return Ok(Refresh {
                stale: true,
                writer_busy: true,
                ..Refresh::default()
            });
        };
        let discovery::Discovered {
            files,
            missing_roots,
        } = discovery::files(roots)?;
        // Read before initialize, which drops these rows on a full rebuild.
        let indexed: Vec<(i64, String)> = if self.db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='files'",
            [],
            |r| r.get::<_, i64>(0),
        )? == 1
        {
            self.db
                .prepare("SELECT id,path FROM files")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        } else {
            Vec::new()
        };
        // A missing root that never held indexed files is one this user does not
        // have; one that did (moved, unmounted) leaves its rows unverified.
        let unverified = roots.iter().find(|r| {
            !r.path.exists()
                && indexed
                    .iter()
                    .any(|(_, path)| Path::new(path).starts_with(&r.path))
        });
        // A rebuild would drop the rows that keep later refreshes stale.
        if let (true, Some(root)) = (full, unverified) {
            anyhow::bail!(
                "{} root {} is missing but holds indexed sessions; restore it, or rebuild with --root for each root that remains",
                name(root.agent),
                root.path.display()
            );
        }
        self.initialize(&_lock, full)?;
        // A refresh that cannot build a changed index writes nothing, because
        // the writer's upserts name unique indexes as their conflict targets;
        // it reports the work left like a spent budget, and `index` builds
        // the indexes.
        let deadline = if self.reconcile_definitions(budget.is_none())? {
            Some(Instant::now())
        } else {
            deadline
        };
        let mut report = Refresh {
            missing_roots,
            stale: unverified.is_some(),
            ..Refresh::default()
        };
        let existing = if full { Vec::new() } else { indexed };
        let discovered: HashSet<_> = files.iter().map(|(_, p)| p.as_path()).collect();
        let present: Vec<&Path> = roots
            .iter()
            .map(|r| r.path.as_path())
            .filter(|root| root.exists())
            .collect();
        let vanished: Vec<(i64, String)> = existing
            .into_iter()
            .filter(|(_, path)| {
                present.iter().any(|root| Path::new(path).starts_with(root))
                    && !discovered.contains(Path::new(path))
            })
            .collect();
        // Files another parser indexed are reindexed after new records and
        // deletions, the most recently changed first. Rule: docs/formats.md
        // `parser_version`.
        let reparse: Vec<String> = self
            .db
            .prepare("SELECT path FROM files WHERE parser<>? ORDER BY CAST(substr(mtime,1,instr(mtime,':')-1) AS INTEGER) DESC,path")?
            .query_map([PARSER_VERSION], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let rank: HashMap<&Path, usize> = reparse
            .iter()
            .enumerate()
            .map(|(i, path)| (Path::new(path), i))
            .collect();
        let (mut stale_files, files): (Vec<_>, Vec<_>) = files
            .into_iter()
            .partition(|(_, path)| rank.contains_key(path.as_path()));
        stale_files.sort_by_key(|(_, path)| rank[path.as_path()]);
        self.index_files(files, deadline, &mut report)?;
        // Rows of vanished files go after new records, which lookups need
        // first, and within the same budget, because each deletion is a
        // transaction that cannot be interrupted. Rule: docs/formats.md
        // `refresh_budget`.
        if report.continuation.is_none() {
            self.delete_files(vanished, deadline, &mut report)?;
        }
        if report.continuation.is_none() {
            self.index_files(stale_files, deadline, &mut report)?;
        }
        // Indexing interleaves the pages of every table, one transaction per
        // file, so cold reads are bound by scattered pages once most of the
        // index was written since it was last compacted, as after a first
        // index, a rebuild, or a parser change; VACUUM rewrites each table into
        // a run of its own. Only an unbudgeted `index` takes that time. Rule:
        // docs/formats.md `index_compaction`.
        if budget.is_none() && compaction_due(&self.db)? {
            self.db
                .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
                .context("the index is built and answers queries, but compacting it failed; compaction needs free disk space up to twice the database size, and `sessidx index` runs it again")?;
            self.db.execute("UPDATE index_identity SET written=0", [])?;
        }
        report.parse_errors =
            self.db
                .query_row("SELECT coalesce(sum(parse_errors),0) FROM files", [], |r| {
                    r.get(0)
                })?;
        Ok(report)
    }

    /// Deletes the rows of `files` in order until the deadline, recording where
    /// it stopped.
    fn delete_files(
        &mut self,
        files: Vec<(i64, String)>,
        deadline: Option<Instant>,
        report: &mut Refresh,
    ) -> Result<()> {
        for (id, path) in files {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                report.stale = true;
                report.continuation = Some(path);
                break;
            }
            let tx = self.db.transaction()?;
            delete_file(&tx, id)?;
            settle(&tx, None)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Indexes `files` in order until the deadline, recording where it stopped.
    fn index_files(
        &mut self,
        files: Vec<(Agent, PathBuf)>,
        deadline: Option<Instant>,
        report: &mut Refresh,
    ) -> Result<()> {
        for (agent, path) in files {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                report.stale = true;
                report.continuation = Some(path.to_string_lossy().into_owned());
                break;
            }
            let result = self.index_file(agent, &path, deadline)?;
            report.files_changed += usize::from(result.changed);
            report.records += result.records;
            if result.coverage == FileCoverage::DeferredTail {
                report.deferred_tails += 1;
            }
            if result.coverage == FileCoverage::BudgetExhausted {
                report.stale = true;
                report.continuation = Some(path.to_string_lossy().into_owned());
                break;
            }
        }
        Ok(())
    }
}

/// Whether more than half of the indexed records were written since the index
/// was last compacted. Rule: docs/formats.md `index_compaction`.
pub fn compaction_due(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT written*2>(SELECT coalesce(sum(lines_indexed),0) FROM files) FROM index_identity",
        [],
        |r| r.get(0),
    )?)
}
