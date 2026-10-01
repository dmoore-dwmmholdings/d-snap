//! Connection setup and forward-only schema migrations keyed on `PRAGMA user_version`.

use std::time::{Duration, Instant};

use rusqlite::{Connection, TransactionBehavior};

use crate::error::{Error, Result};

/// How long a connection waits for another connection's write lock before failing with
/// `SQLITE_BUSY`. Long enough for one prune batch or a large snapshot insert.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Migrations in order; migration `i` moves the schema from version `i` to `i + 1`.
/// Never edit a released migration; append a new one.
const MIGRATIONS: &[&str] = &[
    // v1 (DSNA-30)
    "
    CREATE TABLE projects (
        id            INTEGER PRIMARY KEY,
        name          TEXT    NOT NULL,
        root_path     TEXT    NOT NULL UNIQUE,
        settings_json TEXT    NOT NULL,
        created_at_ms INTEGER NOT NULL
    );

    CREATE TABLE versions (
        id            INTEGER PRIMARY KEY,
        project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        label         TEXT    NOT NULL,
        created_at_ms INTEGER NOT NULL,
        kind          TEXT    NOT NULL CHECK (kind IN ('manual', 'auto', 'cli', 'safety')),
        pinned        INTEGER NOT NULL DEFAULT 0,
        unstable      INTEGER NOT NULL DEFAULT 0,
        added         INTEGER NOT NULL DEFAULT 0,
        modified      INTEGER NOT NULL DEFAULT 0,
        deleted       INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX versions_project ON versions(project_id, id DESC);

    CREATE TABLE entries (
        version_id  INTEGER NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
        path        TEXT    NOT NULL,
        kind        TEXT    NOT NULL CHECK (kind IN ('file', 'symlink', 'dir')),
        blob_hash   BLOB,
        size        INTEGER NOT NULL,
        mtime_ns    INTEGER NOT NULL,
        readonly    INTEGER NOT NULL DEFAULT 0,
        link_target TEXT,
        PRIMARY KEY (version_id, path)
    ) WITHOUT ROWID;
    CREATE INDEX entries_blob ON entries(blob_hash) WHERE blob_hash IS NOT NULL;

    CREATE TABLE blobs (
        hash        BLOB    PRIMARY KEY,
        size        INTEGER NOT NULL,
        stored_size INTEGER NOT NULL
    );

    CREATE TABLE settings (
        key        TEXT PRIMARY KEY,
        value_json TEXT NOT NULL
    );
    ",
];

/// Newest schema version this build knows.
pub(crate) const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Configure `conn` and migrate it to [`SCHEMA_VERSION`].
///
/// Switching a fresh file to WAL takes a lock that SQLite does not retry through the busy
/// handler, so two processes opening a new database at once can see `SQLITE_BUSY` here. Those
/// errors are retried until [`BUSY_TIMEOUT`] runs out.
pub(crate) fn prepare(conn: &mut Connection, file: bool) -> Result<()> {
    let start = Instant::now();
    loop {
        match configure(conn, file).and_then(|()| migrate(conn)) {
            Err(Error::Db(e)) if is_busy(&e) && start.elapsed() < BUSY_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

/// Whether `e` is `SQLITE_BUSY` or `SQLITE_LOCKED`.
pub(crate) fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// Apply connection pragmas. `file` is false for in-memory databases (no WAL there).
fn configure(conn: &Connection, file: bool) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    if file {
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Corrupt(format!(
                "cannot enable WAL journal mode (got {mode})"
            )));
        }
    }
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// Read `PRAGMA user_version`.
pub(crate) fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Bring the schema up to [`SCHEMA_VERSION`]. Each step runs in its own `BEGIN IMMEDIATE`
/// transaction and re-reads the version under the lock, so two processes opening a fresh
/// file at once apply each migration exactly once.
fn migrate(conn: &mut Connection) -> Result<()> {
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = user_version(&tx)?;
        if current > SCHEMA_VERSION {
            return Err(Error::Corrupt(format!(
                "database schema v{current} is newer than this build (v{SCHEMA_VERSION})"
            )));
        }
        let Some(sql) = MIGRATIONS.get(current as usize) else {
            return Ok(()); // up to date; dropping `tx` ends the empty transaction
        };
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", current + 1)?;
        tx.commit()?;
    }
}
