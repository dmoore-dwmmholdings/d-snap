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
#![allow(unused_variables)] // stub signatures; remove when implemented

mod schema;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

use crate::error::Result;
use crate::facade::Dsnap;
use crate::store::Store;
use crate::types::{
    BlobHash, BlobInfo, ChangeCounts, Entry, GlobalSettings, Project, ProjectId, ProjectSettings,
    RelPath, RetentionReport, Version, VersionId, VersionKind,
};

/// Database handle. Thread-safe; serialises access to one connection.
#[derive(Debug)]
pub struct Db {
    #[cfg_attr(not(test), allow(dead_code))] // read by the repository methods (DSNA-31)
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

    #[cfg_attr(not(test), allow(dead_code))] // used by the repository methods (DSNA-31)
    /// Lock the connection. A panic while holding the lock drops any open transaction (which
    /// rolls it back), so a poisoned lock still guards a consistent connection.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Insert a project and return its id.
    pub fn insert_project(
        &self,
        name: &str,
        root: &Path,
        settings: &ProjectSettings,
    ) -> Result<ProjectId> {
        todo!("DSNA-7")
    }

    /// Load one project (`missing` is always `false`; the caller checks the folder).
    pub fn get_project(&self, id: ProjectId) -> Result<Project> {
        todo!("DSNA-7")
    }

    /// All projects ordered by name.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        todo!("DSNA-7")
    }

    /// Change a project's display name.
    pub fn set_project_name(&self, id: ProjectId, name: &str) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Change a project's root folder.
    pub fn set_project_root(&self, id: ProjectId, root: &Path) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Replace a project's settings.
    pub fn set_project_settings(&self, id: ProjectId, settings: &ProjectSettings) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Delete a project with all its versions and entries (blobs are left for pruning).
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Insert a version, its entries and new blob rows in one `BEGIN IMMEDIATE` transaction.
    ///
    /// Before committing, checks that `store` holds every blob the new entries reference that
    /// no existing entry references; otherwise rolls back with [`crate::Error::BlobMissing`]
    /// (see the module docs).
    pub fn insert_version(&self, v: &NewVersion, store: &Store) -> Result<Version> {
        todo!("DSNA-7")
    }

    /// Load one version.
    pub fn get_version(&self, id: VersionId) -> Result<Version> {
        todo!("DSNA-7")
    }

    /// A project's versions, newest first.
    pub fn list_versions(&self, project: ProjectId) -> Result<Vec<Version>> {
        todo!("DSNA-7")
    }

    /// A project's newest version.
    pub fn latest_version(&self, project: ProjectId) -> Result<Option<Version>> {
        todo!("DSNA-7")
    }

    /// The version created just before `id` in the same project.
    pub fn previous_version(&self, id: VersionId) -> Result<Option<Version>> {
        todo!("DSNA-7")
    }

    /// A version's entries, sorted by path.
    pub fn entries(&self, version: VersionId) -> Result<Vec<Entry>> {
        todo!("DSNA-7")
    }

    /// One entry of a version.
    pub fn entry(&self, version: VersionId, path: &RelPath) -> Result<Option<Entry>> {
        todo!("DSNA-7")
    }

    /// Change a version's label.
    pub fn set_version_label(&self, id: VersionId, label: &str) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Pin or unpin a version.
    pub fn set_version_pinned(&self, id: VersionId, pinned: bool) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Delete a version and its entries.
    pub fn delete_version(&self, id: VersionId) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Record a blob (no-op if present).
    pub fn insert_blob(&self, blob: &BlobInfo) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Blobs no entry references, for reporting only. Never delete based on this list; the
    /// answer can change as soon as it returns. Use [`Db::prune_unreferenced`].
    pub fn unreferenced_blobs(&self) -> Result<Vec<BlobInfo>> {
        todo!("DSNA-7")
    }

    /// Delete up to `limit` unreferenced blobs (store file and row) in one `BEGIN IMMEDIATE`
    /// transaction. This is the only code path that deletes blob files (see the module docs).
    /// Returns `versions_deleted: 0` plus the blobs and bytes freed.
    pub fn prune_unreferenced(&self, store: &Store, limit: usize) -> Result<RetentionReport> {
        todo!("DSNA-7")
    }

    /// Whether any entry references `hash`.
    pub fn blob_referenced(&self, hash: &BlobHash) -> Result<bool> {
        todo!("DSNA-7")
    }

    /// Stored global settings (defaults if never set).
    pub fn global_settings(&self) -> Result<GlobalSettings> {
        todo!("DSNA-7")
    }

    /// Replace global settings.
    pub fn set_global_settings(&self, settings: &GlobalSettings) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Value that changes whenever any connection commits a write. Poll it to detect writes
    /// from another process (e.g. the CLI).
    pub fn change_token(&self) -> Result<i64> {
        todo!("DSNA-7")
    }
}

impl Dsnap {
    /// Token that changes when the database changes (from this or another process).
    pub fn db_change_token(&self) -> Result<i64> {
        todo!("DSNA-7")
    }
}
