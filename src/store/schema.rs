use super::{SCHEMA_VERSION, Store, WriterLock, derived::create_work_tables};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

impl Store {
    pub fn require_schema(db: &Connection) -> Result<()> {
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            version == SCHEMA_VERSION,
            "database schema changed; run sessidx index --full"
        );
        anyhow::ensure!(
            definitions(db, "view")? == definitions(&expected_schema()?, "view")?,
            "database views changed; run sessidx index"
        );
        Ok(())
    }

    /// Brings views and indexes to the definitions in `schema.sql`, which
    /// change without a schema version. Views are recreated at once, because
    /// the query that follows reads them; indexes only when `build_indexes`,
    /// because building one reads a whole table and cannot stop at a deadline.
    /// Returns whether indexes remain to be built. Rule: docs/formats.md
    /// `schema_definitions`.
    pub(super) fn reconcile_definitions(&mut self, build_indexes: bool) -> Result<bool> {
        let expected = expected_schema()?;
        let tx = self.db.transaction()?;
        if definitions(&tx, "view")? != definitions(&expected, "view")? {
            for name in definitions(&tx, "view")?.keys() {
                tx.execute_batch(&format!("DROP VIEW \"{name}\";"))?;
            }
            for sql in creation_order(&expected, "view")? {
                tx.execute_batch(&sql)?;
            }
        }
        let (have, want) = (definitions(&tx, "index")?, definitions(&expected, "index")?);
        if build_indexes {
            for (name, sql) in &have {
                if want.get(name) != Some(sql) {
                    tx.execute_batch(&format!("DROP INDEX \"{name}\";"))?;
                }
            }
            for (name, sql) in &want {
                if have.get(name) != Some(sql) {
                    tx.execute_batch(sql)?;
                }
            }
        }
        tx.commit()?;
        Ok(!build_indexes && have != want)
    }

    pub(super) fn initialize(&mut self, _lock: &WriterLock, full: bool) -> Result<()> {
        // Checkpointing every 10,000 pages instead of SQLite's default 1,000
        // built the maintainer's logs 7-13% faster, with the WAL peaking near
        // 100 MiB instead of 80 MiB.
        self.db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-8192; PRAGMA wal_autocheckpoint=10000;",
        )?;
        let version = schema_version(&self.db, full)?;
        if version == 0 {
            self.db.execute_batch("PRAGMA journal_mode=WAL;")?;
            self.db.execute_batch(include_str!("../schema.sql"))?;
        }
        if full && version != 0 {
            // With foreign keys on, each DROP TABLE first deletes its rows,
            // firing triggers on tables that may already be gone.
            self.db.execute_batch("PRAGMA foreign_keys=OFF;")?;
            let tx = self.db.transaction()?;
            // The schema recreates the index identity at random, so that
            // every earlier cursor is refused instead of resolving to new rows
            // whose file IDs restart.
            let objects: Vec<(String, String)> = tx
                .prepare("SELECT type,name FROM pragma_table_list WHERE schema='main' AND type IN ('view','table','virtual') AND name NOT LIKE 'sqlite_%' ORDER BY type='view' DESC")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (kind, name) in objects {
                let kind = if kind == "view" { "VIEW" } else { "TABLE" };
                tx.execute_batch(&format!("DROP {kind} IF EXISTS \"{name}\";"))?;
            }
            tx.execute_batch(include_str!("../schema.sql"))?;
            tx.commit()?;
            self.db.execute_batch("PRAGMA foreign_keys=ON;")?;
            self.db.execute_batch("VACUUM;")?;
        }
        create_work_tables(&self.db)
    }
}

/// The database's schema version: 0 for a new one, this binary's, or when
/// `rebuilding`, an older one that a full rebuild replaces.
pub(super) fn schema_version(db: &Connection, rebuilding: bool) -> Result<i64> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    anyhow::ensure!(
        version == 0
            || version == SCHEMA_VERSION
            || rebuilding && (1..SCHEMA_VERSION).contains(&version),
        "database schema changed; run sessidx index --full"
    );
    Ok(version)
}

/// The views and indexes this binary's `schema.sql` creates.
fn expected_schema() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(include_str!("../schema.sql"))?;
    Ok(db)
}

/// The definition of each view or index, as SQLite stored its statement.
fn definitions(db: &Connection, kind: &str) -> Result<BTreeMap<String, String>> {
    Ok(db
        .prepare("SELECT name,sql FROM sqlite_master WHERE type=? AND sql IS NOT NULL")?
        .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The statements that create each view or index, in the order `schema.sql`
/// creates them, since a view can read another.
fn creation_order(db: &Connection, kind: &str) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT sql FROM sqlite_master WHERE type=? AND sql IS NOT NULL ORDER BY rowid")?
        .query_map([kind], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
