//! SQLite index: projects, versions, entries, blobs, settings. Owner: Chain D (DSNA-7).
//!
//! Every write that creates a version runs in one transaction. Sets a busy timeout so the CLI
//! and the app can share the file.
//!
//! # Blob lifetime protocol (DSNA-80, Rule 1)
//!
//! The CLI and the app are separate processes sharing one store. A snapshot may deduplicate
//! against a blob (`Store::put` is a no-op when the file exists) that retention is about to
//! prune. To make that safe, the SQLite write lock is the single lock for blob lifetime:
//!
//! 1. **Only [`Db::prune_unreferenced`] deletes blob files**, and it does so inside one
//!    `BEGIN IMMEDIATE` transaction: select unreferenced blobs, delete each store file
//!    ([`Store::delete`]), delete their rows, commit. No other code calls `Store::delete`.
//! 2. **[`Db::insert_version`] verifies before committing.** Inside its own `BEGIN IMMEDIATE`
//!    transaction, for every distinct blob in the new entries that no existing entry row
//!    references, it checks [`Store::contains`]. If one is missing it rolls back and returns
//!    [`crate::Error::BlobMissing`]. Blobs that an existing row references cannot be pruned
//!    while the lock is held, so they need no check.
//! 3. **The snapshot retries.** On `BlobMissing`, the caller (snapshot, DSNA-14) re-stores
//!    the affected files with `Store::put_file` and calls `insert_version` once more. If the
//!    file's hash changed in the meantime, it redoes the capture for that path.
//!
//! A prune that crashes after deleting files but before committing leaves rows for missing
//! files. Those rows are unreferenced, so step 2 catches any reuse and the next prune removes
//! them (`Store::delete` of a missing file returns `Ok(0)`). Keep prune batches small (see
//! `limit`) so the write lock is not held for longer than the busy timeout.
#![allow(unused_variables)] // DSNA-82 / DSNA-32 stubs below; removed when implemented

mod codec;
mod schema;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::store::Store;
use crate::types::{
    BlobHash, BlobInfo, ChangeCounts, Entry, GlobalSettings, Project, ProjectId, ProjectSettings,
    RelPath, RetentionReport, Version, VersionId, VersionKind,
};

use codec::{
    BLOB_COLS, ENTRY_COLS, EntryRow, PROJECT_COLS, ProjectRow, VERSION_COLS, VersionRow,
    blob_from_row, blob_row, entry_kind_to_sql, root_to_sql, settings_from_sql, settings_to_sql,
    u64_to_sql, version_kind_to_sql,
};

/// Key of the global settings row in the `settings` table.
const GLOBAL_SETTINGS_KEY: &str = "global";

/// Database handle. Thread-safe; serialises access to one connection.
#[derive(Debug)]
pub struct Db {
    pub(crate) conn: Mutex<Connection>,
}

/// Input for [`Db::insert_version`].
#[derive(Debug, Clone)]
pub struct NewVersion {
    /// Owning project.
    pub project_id: ProjectId,
    /// Label.
    pub label: String,
    /// Creation time, ms since the Unix epoch.
    pub created_at_ms: i64,
    /// How it was created.
    pub kind: VersionKind,
    /// Some files kept changing during capture.
    pub unstable: bool,
    /// Counts versus the previous version.
    pub counts: ChangeCounts,
    /// Full file list, sorted by path.
    pub entries: Vec<Entry>,
    /// Blobs written for this version (inserted if not already recorded).
    pub new_blobs: Vec<BlobInfo>,
}

impl Db {
    /// Open or create the database at `path` and apply migrations.
    ///
    /// Uses WAL, `foreign_keys=ON`, `synchronous=NORMAL` and a busy timeout so the CLI and the
    /// app can use the same file at once.
    pub fn open(path: &Path) -> Result<Self> {
        Self::setup(Connection::open(path)?, true)
    }

    /// Open a private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::setup(Connection::open_in_memory()?, false)
    }

    fn setup(mut conn: Connection, file: bool) -> Result<Self> {
        schema::prepare(&mut conn, file)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Lock the connection. A panic while holding the lock drops any open transaction (which
    /// rolls it back), so a poisoned lock still guards a consistent connection.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Run `f` in a `BEGIN IMMEDIATE` transaction and commit if it returns `Ok`.
    ///
    /// Every write takes the write lock up front: in WAL mode a read transaction that later
    /// tries to write fails with `SQLITE_BUSY` without waiting on the busy timeout.
    fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Insert a project and return its id.
    ///
    /// Fails with [`Error::InvalidInput`] if `root` is already tracked or is not valid UTF-8.
    pub fn insert_project(
        &self,
        name: &str,
        root: &Path,
        settings: &ProjectSettings,
    ) -> Result<ProjectId> {
        let root_s = root_to_sql(root)?;
        let json = settings_to_sql(settings)?;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO projects (name, root_path, settings_json, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![name, root_s, json, now_ms()],
            )
            .map_err(|e| unique_root(e, root))?;
            Ok(ProjectId(tx.last_insert_rowid()))
        })
    }

    /// Load one project (`missing` is always `false`; the caller checks the folder).
    pub fn get_project(&self, id: ProjectId) -> Result<Project> {
        self.conn()
            .query_row(
                &format!("SELECT {PROJECT_COLS} FROM projects WHERE id = ?1"),
                [id.0],
                ProjectRow::read,
            )
            .optional()?
            .ok_or_else(|| not_found_project(id))?
            .into_project()
    }

    /// All projects ordered by name (case-insensitive, then exact, then id).
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {PROJECT_COLS} FROM projects ORDER BY name COLLATE NOCASE, name, id"
        ))?;
        let rows = stmt
            .query_map([], ProjectRow::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(ProjectRow::into_project).collect()
    }

    /// Change a project's display name.
    pub fn set_project_name(&self, id: ProjectId, name: &str) -> Result<()> {
        self.write(|tx| {
            let n = tx.execute(
                "UPDATE projects SET name = ?2 WHERE id = ?1",
                params![id.0, name],
            )?;
            found(n, || not_found_project(id))
        })
    }

    /// Change a project's root folder.
    pub fn set_project_root(&self, id: ProjectId, root: &Path) -> Result<()> {
        let root_s = root_to_sql(root)?;
        self.write(|tx| {
            let n = tx
                .execute(
                    "UPDATE projects SET root_path = ?2 WHERE id = ?1",
                    params![id.0, root_s],
                )
                .map_err(|e| unique_root(e, root))?;
            found(n, || not_found_project(id))
        })
    }

    /// Replace a project's settings.
    pub fn set_project_settings(&self, id: ProjectId, settings: &ProjectSettings) -> Result<()> {
        let json = settings_to_sql(settings)?;
        self.write(|tx| {
            let n = tx.execute(
                "UPDATE projects SET settings_json = ?2 WHERE id = ?1",
                params![id.0, json],
            )?;
            found(n, || not_found_project(id))
        })
    }

    /// Delete a project with all its versions and entries (blobs are left for pruning).
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        self.write(|tx| {
            let n = tx.execute("DELETE FROM projects WHERE id = ?1", [id.0])?;
            found(n, || not_found_project(id))
        })
    }

    /// Insert a version, its entries and new blob rows in one `BEGIN IMMEDIATE` transaction.
    ///
    /// Before committing, checks that `store` holds every blob the new entries reference that
    /// no existing entry references; otherwise rolls back with [`crate::Error::BlobMissing`]
    /// (see the module docs).
    pub fn insert_version(&self, v: &NewVersion, store: &Store) -> Result<Version> {
        self.write(|tx| insert_version_tx(tx, v))
    }

    /// Load one version.
    pub fn get_version(&self, id: VersionId) -> Result<Version> {
        let conn = self.conn();
        version_by_id(&conn, id)?.ok_or_else(|| not_found_version(id))
    }

    /// A project's versions, newest first.
    pub fn list_versions(&self, project: ProjectId) -> Result<Vec<Version>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {VERSION_COLS} FROM versions WHERE project_id = ?1 ORDER BY id DESC"
        ))?;
        let rows = stmt
            .query_map([project.0], VersionRow::read)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(VersionRow::into_version).collect()
    }

    /// A project's newest version.
    pub fn latest_version(&self, project: ProjectId) -> Result<Option<Version>> {
        let conn = self.conn();
        latest_version(&conn, project)
    }

    /// The version created just before `id` in the same project.
    pub fn previous_version(&self, id: VersionId) -> Result<Option<Version>> {
        let conn = self.conn();
        let this = version_by_id(&conn, id)?.ok_or_else(|| not_found_version(id))?;
        conn.query_row(
            &format!(
                "SELECT {VERSION_COLS} FROM versions
                 WHERE project_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT 1"
            ),
            params![this.project_id.0, id.0],
            VersionRow::read,
        )
        .optional()?
        .map(VersionRow::into_version)
        .transpose()
    }

    /// A version's entries, sorted by path.
    pub fn entries(&self, version: VersionId) -> Result<Vec<Entry>> {
        let conn = self.conn();
        if version_by_id(&conn, version)?.is_none() {
            return Err(not_found_version(version));
        }
        entries_of(&conn, version)
    }

    /// One entry of a version.
    pub fn entry(&self, version: VersionId, path: &RelPath) -> Result<Option<Entry>> {
        self.conn()
            .query_row(
                &format!("SELECT {ENTRY_COLS} FROM entries WHERE version_id = ?1 AND path = ?2"),
                params![version.0, path.as_str()],
                EntryRow::read,
            )
            .optional()?
            .map(EntryRow::into_entry)
            .transpose()
    }

    /// Entries of a project's newest version keyed by path (empty if it has no versions).
    ///
    /// The snapshot fast path uses it to reuse a file's hash when size and mtime match.
    pub fn latest_entry_index(&self, project: ProjectId) -> Result<HashMap<RelPath, Entry>> {
        let conn = self.conn();
        let Some(latest) = latest_version(&conn, project)? else {
            return Ok(HashMap::new());
        };
        Ok(entries_of(&conn, latest.id)?
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect())
    }

    /// Change a version's label.
    pub fn set_version_label(&self, id: VersionId, label: &str) -> Result<()> {
        self.write(|tx| {
            let n = tx.execute(
                "UPDATE versions SET label = ?2 WHERE id = ?1",
                params![id.0, label],
            )?;
            found(n, || not_found_version(id))
        })
    }

    /// Pin or unpin a version.
    pub fn set_version_pinned(&self, id: VersionId, pinned: bool) -> Result<()> {
        self.write(|tx| {
            let n = tx.execute(
                "UPDATE versions SET pinned = ?2 WHERE id = ?1",
                params![id.0, pinned],
            )?;
            found(n, || not_found_version(id))
        })
    }

    /// Delete a version and its entries.
    pub fn delete_version(&self, id: VersionId) -> Result<()> {
        self.write(|tx| {
            let n = tx.execute("DELETE FROM versions WHERE id = ?1", [id.0])?;
            found(n, || not_found_version(id))
        })
    }

    /// Record a blob (no-op if present).
    pub fn insert_blob(&self, blob: &BlobInfo) -> Result<()> {
        self.write(|tx| insert_blob_tx(tx, blob))
    }

    /// Blobs no entry references, for reporting only. Never delete based on this list; the
    /// answer can change as soon as it returns. Use [`Db::prune_unreferenced`].
    pub fn unreferenced_blobs(&self) -> Result<Vec<BlobInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {BLOB_COLS} FROM blobs b
             WHERE NOT EXISTS (SELECT 1 FROM entries e WHERE e.blob_hash = b.hash)
             ORDER BY hash"
        ))?;
        let rows = stmt
            .query_map([], blob_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(blob_from_row).collect()
    }

    /// Delete up to `limit` unreferenced blobs (store file and row) in one `BEGIN IMMEDIATE`
    /// transaction. This is the only code path that deletes blob files (see the module docs).
    /// Returns `versions_deleted: 0` plus the blobs and bytes freed.
    pub fn prune_unreferenced(&self, store: &Store, limit: usize) -> Result<RetentionReport> {
        todo!("DSNA-82")
    }

    /// Whether any entry references `hash`.
    pub fn blob_referenced(&self, hash: &BlobHash) -> Result<bool> {
        todo!("DSNA-82")
    }

    /// Stored global settings (defaults if never set; missing fields take their defaults).
    pub fn global_settings(&self) -> Result<GlobalSettings> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                [GLOBAL_SETTINGS_KEY],
                |r| r.get(0),
            )
            .optional()?;
        json.map_or_else(|| Ok(GlobalSettings::default()), |j| settings_from_sql(&j))
    }

    /// Replace global settings.
    pub fn set_global_settings(&self, settings: &GlobalSettings) -> Result<()> {
        let json = settings_to_sql(settings)?;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO settings (key, value_json) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                params![GLOBAL_SETTINGS_KEY, json],
            )?;
            Ok(())
        })
    }

    /// Value that changes whenever any connection commits a write. Poll it to detect writes
    /// from another process (e.g. the CLI).
    pub fn change_token(&self) -> Result<i64> {
        todo!("DSNA-32")
    }
}

impl Dsnap {
    /// Token that changes when the database changes (from this or another process).
    pub fn db_change_token(&self) -> Result<i64> {
        todo!("DSNA-32")
    }
}

fn insert_version_tx(tx: &Transaction<'_>, v: &NewVersion) -> Result<Version> {
    let project_exists = tx
        .query_row(
            "SELECT 1 FROM projects WHERE id = ?1",
            [v.project_id.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !project_exists {
        return Err(not_found_project(v.project_id));
    }
    tx.execute(
        "INSERT INTO versions
             (project_id, label, created_at_ms, kind, pinned, unstable, added, modified, deleted)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8)",
        params![
            v.project_id.0,
            v.label,
            v.created_at_ms,
            version_kind_to_sql(v.kind),
            v.unstable,
            v.counts.added,
            v.counts.modified,
            v.counts.deleted,
        ],
    )?;
    let id = VersionId(tx.last_insert_rowid());
    {
        let mut stmt = tx.prepare_cached(&format!(
            "INSERT INTO entries (version_id, {ENTRY_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ))?;
        for e in &v.entries {
            let (kind, target) = entry_kind_to_sql(&e.kind);
            stmt.execute(params![
                id.0,
                e.path.as_str(),
                kind,
                e.blob.as_ref().map(|h| h.0.as_slice()),
                u64_to_sql(e.size, "entry size")?,
                e.mtime_ns,
                e.readonly,
                target,
            ])
            .map_err(|err| duplicate_path(err, &e.path))?;
        }
    }
    for b in &v.new_blobs {
        insert_blob_tx(tx, b)?;
    }
    Ok(Version {
        id,
        project_id: v.project_id,
        label: v.label.clone(),
        created_at_ms: v.created_at_ms,
        kind: v.kind,
        pinned: false,
        unstable: v.unstable,
        counts: v.counts,
    })
}

fn insert_blob_tx(tx: &Transaction<'_>, b: &BlobInfo) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO blobs (hash, size, stored_size) VALUES (?1, ?2, ?3)
         ON CONFLICT(hash) DO NOTHING",
    )?;
    stmt.execute(params![
        b.hash.0.as_slice(),
        u64_to_sql(b.size, "blob size")?,
        u64_to_sql(b.stored_size, "blob stored size")?,
    ])?;
    Ok(())
}

fn version_by_id(conn: &Connection, id: VersionId) -> Result<Option<Version>> {
    conn.query_row(
        &format!("SELECT {VERSION_COLS} FROM versions WHERE id = ?1"),
        [id.0],
        VersionRow::read,
    )
    .optional()?
    .map(VersionRow::into_version)
    .transpose()
}

fn latest_version(conn: &Connection, project: ProjectId) -> Result<Option<Version>> {
    conn.query_row(
        &format!(
            "SELECT {VERSION_COLS} FROM versions WHERE project_id = ?1 ORDER BY id DESC LIMIT 1"
        ),
        [project.0],
        VersionRow::read,
    )
    .optional()?
    .map(VersionRow::into_version)
    .transpose()
}

fn entries_of(conn: &Connection, version: VersionId) -> Result<Vec<Entry>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {ENTRY_COLS} FROM entries WHERE version_id = ?1 ORDER BY path"
    ))?;
    let rows = stmt
        .query_map([version.0], EntryRow::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(EntryRow::into_entry).collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `Ok` if a statement changed at least one row, else the error from `missing`.
fn found(changed: usize, missing: impl FnOnce() -> Error) -> Result<()> {
    if changed == 0 { Err(missing()) } else { Ok(()) }
}

fn not_found_project(id: ProjectId) -> Error {
    Error::NotFound(format!("project {id}"))
}

fn not_found_version(id: VersionId) -> Error {
    Error::NotFound(format!("version {id}"))
}

fn is_constraint(e: &rusqlite::Error) -> bool {
    e.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation)
}

/// Map a `projects.root_path` UNIQUE violation to [`Error::InvalidInput`].
fn unique_root(e: rusqlite::Error, root: &Path) -> Error {
    if is_constraint(&e) {
        Error::InvalidInput(format!("folder is already tracked: {}", root.display()))
    } else {
        e.into()
    }
}

/// Map an `entries` primary-key violation to [`Error::InvalidInput`].
fn duplicate_path(e: rusqlite::Error, path: &RelPath) -> Error {
    if is_constraint(&e) {
        Error::InvalidInput(format!("duplicate entry path {path}"))
    } else {
        e.into()
    }
}
