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
//! 1. **Only [`Db::prune_unreferenced`] and [`Db::sweep_orphans`] delete blob files**, each
//!    inside one `BEGIN IMMEDIATE` transaction that also reads which blobs are referenced:
//!    prune selects blob rows with `refs = 0`, deletes each store file ([`Store::delete`])
//!    and its row; sweep reads every hash with `refs > 0` and calls [`Store::sweep`] with
//!    that set. No other code calls `Store::delete` or `Store::sweep`, and a referenced set
//!    read in an earlier transaction must never be used to delete.
//! 2. **[`Db::insert_version`] verifies before committing.** Inside its own `BEGIN IMMEDIATE`
//!    transaction, for every distinct blob in the new entries whose row is missing or has
//!    `refs = 0`, it checks [`Store::contains`]. If one is missing it rolls back and returns
//!    [`crate::Error::BlobMissing`]. Blobs with `refs > 0` cannot be pruned while the lock is
//!    held, so they need no check.
//! 3. **The snapshot retries.** On `BlobMissing`, the caller (snapshot, DSNA-14) re-stores
//!    the affected files with `Store::put_file` and calls `insert_version` once more. If the
//!    file's hash changed in the meantime, it redoes the capture for that path.
//!
//! A prune that crashes after deleting files but before committing leaves rows for missing
//! files. Those rows are unreferenced, so step 2 catches any reuse and the next prune removes
//! them (`Store::delete` of a missing file returns `Ok(0)`). Keep prune batches small (see
//! `limit`) so the write lock is not held for longer than the busy timeout.
//!
//! **Reference counts (schema v3, DSNA-101).** `blobs.refs` counts the versions in which a
//! blob starts to be used (see the `refs` module); `refs > 0` holds exactly when a committed
//! entry references the blob, which is what the old `EXISTS` over the `entries_blob` index
//! answered. `insert_version`, `delete_version`, `delete_versions`, `delete_unpinned_beyond`
//! and `delete_project` update the counts in the same transaction as the rows. Any new code
//! that deletes versions must go through `delete_version_row` (or subtract
//! `refs::run_starts` for a whole project). Triggers refuse an entry whose blob has no row,
//! any change to an entry, and deleting a blob row with `refs > 0`.
//!
//! Every connection runs with `synchronous=FULL`, so a commit that removes references
//! (`delete_version`, `delete_project`) is on disk before a later prune deletes the files;
//! with `NORMAL` a power loss could bring the version back without its blobs.
mod codec;
mod refs;
mod schema;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Longest one write transaction of a project removal should take (DSNA-111); it stays
/// well under the 5 s busy timeout other processes wait for.
pub const REMOVE_BATCH_BUDGET: Duration = Duration::from_millis(250);

/// Pause between project-removal batches, so a writer waiting in SQLite's busy handler
/// (which polls at most every 100 ms) gets the lock before the next batch takes it.
const REMOVE_BATCH_PAUSE: Duration = Duration::from_millis(100);

/// Blobs deleted per [`Db::prune_unreferenced`] call when pruning everything in batches.
///
/// Small enough that one batch (one store file delete per blob) finishes well inside the
/// 5 s busy timeout another process waits for the write lock.
pub const PRUNE_BATCH: usize = 256;

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
    /// Uses WAL, `foreign_keys=ON`, `synchronous=FULL` and a busy timeout so the CLI and the
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
    ///
    /// `COMMIT` itself can fail with `SQLITE_BUSY` in WAL mode (seen on Windows while another
    /// connection checkpoints), and that path does not wait on the busy handler. The
    /// transaction stays open after such a failure, so the commit is retried within the busy
    /// timeout and rolled back only if it still fails (DSNA-112). `BEGIN IMMEDIATE` can fail
    /// the same way without waiting (seen on windows-latest CI), so it is retried too.
    fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        let start = std::time::Instant::now();
        let mut tx = loop {
            match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
                Ok(tx) => break tx,
                Err(e) if schema::is_busy(&e) && start.elapsed() < schema::BUSY_TIMEOUT => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(e.into()),
            }
        };
        let out = f(&tx)?; // an error drops `tx`, which rolls back
        tx.set_drop_behavior(rusqlite::DropBehavior::Ignore);
        commit_with_retry(&tx)?;
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
                &format!("SELECT {PROJECT_COLS} FROM projects WHERE id = ?1 AND removing = 0"),
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
        projects_of(&conn, false)
    }

    /// [`Db::insert_project`], first running `check` on all existing projects inside the
    /// same `BEGIN IMMEDIATE` transaction, so no other process can add a conflicting project
    /// between the check and the insert. `check`'s error aborts the insert.
    pub fn insert_project_checked(
        &self,
        name: &str,
        root: &Path,
        settings: &ProjectSettings,
        check: &dyn Fn(&[Project]) -> Result<()>,
    ) -> Result<ProjectId> {
        let root_s = root_to_sql(root)?;
        let json = settings_to_sql(settings)?;
        self.write(|tx| {
            // Projects being removed still own their root folder.
            check(&projects_of(tx, true)?)?;
            tx.execute(
                "INSERT INTO projects (name, root_path, settings_json, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![name, root_s, json, now_ms()],
            )
            .map_err(|e| unique_root(e, root))?;
            Ok(ProjectId(tx.last_insert_rowid()))
        })
    }

    /// [`Db::set_project_root`], first running `check` on all projects inside the same
    /// `BEGIN IMMEDIATE` transaction (see [`Db::insert_project_checked`]).
    pub fn set_project_root_checked(
        &self,
        id: ProjectId,
        root: &Path,
        check: &dyn Fn(&[Project]) -> Result<()>,
    ) -> Result<()> {
        let root_s = root_to_sql(root)?;
        self.write(|tx| {
            // Projects being removed still own their root folder.
            check(&projects_of(tx, true)?)?;
            let n = tx
                .execute(
                    "UPDATE projects SET root_path = ?2 WHERE id = ?1 AND removing = 0",
                    params![id.0, root_s],
                )
                .map_err(|e| unique_root(e, root))?;
            found(n, || not_found_project(id))
        })
    }

    /// Change a project's display name.
    pub fn set_project_name(&self, id: ProjectId, name: &str) -> Result<()> {
        self.write(|tx| {
            let n = tx.execute(
                "UPDATE projects SET name = ?2 WHERE id = ?1 AND removing = 0",
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
                    "UPDATE projects SET root_path = ?2 WHERE id = ?1 AND removing = 0",
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
                "UPDATE projects SET settings_json = ?2 WHERE id = ?1 AND removing = 0",
                params![id.0, json],
            )?;
            found(n, || not_found_project(id))
        })
    }

    /// Delete a project with all its versions and entries (blobs are left for pruning).
    ///
    /// Runs in short transactions so other processes are not locked out for long
    /// (DSNA-111): the first flags the project `removing`, which hides it from
    /// [`Db::get_project`] and [`Db::list_projects`] and refuses new versions; then its
    /// versions are deleted newest first, each batch holding the write lock for about
    /// [`REMOVE_BATCH_BUDGET`]; the last drops the project row. If the process dies part way,
    /// [`Db::resume_removals`] finishes the job.
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        self.delete_project_timed(id, REMOVE_BATCH_BUDGET)
            .map(|_| ())
    }

    /// [`Db::delete_project`] with an explicit batch budget; returns how long each write
    /// transaction took (tests and timing).
    pub(crate) fn delete_project_timed(
        &self,
        id: ProjectId,
        budget: Duration,
    ) -> Result<Vec<Duration>> {
        let mut times = Vec::new();
        let t = Instant::now();
        self.write(|tx| {
            let n = tx.execute("UPDATE projects SET removing = 1 WHERE id = ?1", [id.0])?;
            found(n, || not_found_project(id))
        })?;
        times.push(t.elapsed());
        self.finish_removal(id, budget, &mut times)?;
        Ok(times)
    }

    /// Finish removing projects that a crash left flagged `removing` (see
    /// [`Db::delete_project`]).
    pub fn resume_removals(&self) -> Result<()> {
        let ids = {
            let conn = self.conn();
            let mut stmt = conn.prepare_cached("SELECT id FROM projects WHERE removing = 1")?;
            stmt.query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in ids {
            self.finish_removal(ProjectId(id), REMOVE_BATCH_BUDGET, &mut Vec::new())?;
        }
        Ok(())
    }

    /// Delete a flagged project's versions in batches, then its row.
    fn finish_removal(
        &self,
        id: ProjectId,
        budget: Duration,
        times: &mut Vec<Duration>,
    ) -> Result<()> {
        loop {
            let t = Instant::now();
            let more = self.write(|tx| {
                loop {
                    let newest: Option<i64> = tx
                        .query_row(
                            "SELECT id FROM versions WHERE project_id = ?1
                             ORDER BY id DESC LIMIT 1",
                            [id.0],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let Some(newest) = newest else {
                        return Ok(false);
                    };
                    // Newest first: the deleted version has no successor, so only its own
                    // reference counts change.
                    delete_version_row(tx, VersionId(newest))?;
                    if t.elapsed() >= budget {
                        return Ok(true);
                    }
                }
            })?;
            times.push(t.elapsed());
            if !more {
                break;
            }
            std::thread::sleep(REMOVE_BATCH_PAUSE);
        }
        let t = Instant::now();
        self.write(|tx| {
            // No versions are left; subtracting their run starts keeps this exact even if
            // one slipped in before the flag was set.
            let mut gone = refs::run_starts(tx, Some(id))?;
            gone.values_mut().for_each(|n| *n = -*n);
            refs::apply(tx, &gone)?;
            tx.execute("DELETE FROM projects WHERE id = ?1", [id.0])?;
            Ok(())
        })?;
        times.push(t.elapsed());
        Ok(())
    }

    /// Insert a version, its entries and new blob rows in one `BEGIN IMMEDIATE` transaction.
    ///
    /// Before committing, checks that `store` holds every blob the new entries reference that
    /// no existing entry references; otherwise rolls back with [`crate::Error::BlobMissing`]
    /// (see the module docs). A blob that is in the store but has no row and is not in
    /// `new_blobs` gets a row (size from the entry, stored size from the store).
    pub fn insert_version(&self, v: &NewVersion, store: &Store) -> Result<Version> {
        self.insert_version_with(v, store)
    }

    /// [`Db::insert_version`] against any [`BlobFiles`] (tests use a fake store).
    pub(crate) fn insert_version_with(
        &self,
        v: &NewVersion,
        files: &dyn BlobFiles,
    ) -> Result<Version> {
        self.write(|tx| insert_version_tx(tx, v, files))
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
        self.write(|tx| match delete_version_row(tx, id)? {
            Some(_) => Ok(()),
            None => Err(not_found_version(id)),
        })
    }

    /// Delete several versions and recompute the stored counts of every surviving version
    /// whose predecessor changed, in one `BEGIN IMMEDIATE` transaction.
    ///
    /// `recount(old, new)` gets the entries of the new predecessor (empty if none) and of
    /// the survivor. Ids that do not exist are skipped. Returns the number of versions
    /// deleted. Blobs are left for [`Db::prune_unreferenced`].
    pub fn delete_versions(&self, ids: &[VersionId], recount: &Recount<'_>) -> Result<u32> {
        self.write(|tx| delete_versions_tx(tx, ids, recount))
    }

    /// Retention step: delete up to `limit` of a project's oldest unpinned versions beyond
    /// the newest `keep` unpinned ones, recomputing successor counts like
    /// [`Db::delete_versions`]. Versions in `protect` are treated like pinned ones: never
    /// deleted and not counted toward `keep`. The candidates are chosen inside the same
    /// `BEGIN IMMEDIATE` transaction, so a version pinned or added by another process
    /// meanwhile is honored. Returns the number deleted; call again until it returns 0.
    pub fn delete_unpinned_beyond(
        &self,
        project: ProjectId,
        keep: u32,
        protect: &[VersionId],
        limit: usize,
        recount: &Recount<'_>,
    ) -> Result<u32> {
        let keep = usize::try_from(keep).unwrap_or(usize::MAX);
        self.write(|tx| {
            let ids = {
                let mut stmt = tx.prepare_cached(
                    "SELECT id FROM versions WHERE project_id = ?1 AND pinned = 0
                     ORDER BY id DESC",
                )?;
                stmt.query_map([project.0], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            // Newest first: skip protected ones and the newest `keep`; delete oldest first.
            let mut ids: Vec<VersionId> = ids
                .into_iter()
                .map(VersionId)
                .filter(|id| !protect.contains(id))
                .skip(keep)
                .collect();
            ids.reverse();
            ids.truncate(limit);
            delete_versions_tx(tx, &ids, recount)
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
             WHERE refs = 0 ORDER BY hash"
        ))?;
        let rows = stmt
            .query_map([], blob_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(blob_from_row).collect()
    }

    /// Delete up to `limit` unreferenced blobs (store file and row) in one `BEGIN IMMEDIATE`
    /// transaction. This is the only code path that deletes blob files (see the module docs).
    /// Returns `versions_deleted: 0` plus the blobs and bytes freed.
    ///
    /// Call it repeatedly with [`PRUNE_BATCH`] until `blobs_pruned` is 0 to prune everything.
    ///
    /// A blob whose store delete fails keeps its row, and its failure count moves it behind
    /// every other candidate, so a file that cannot be deleted never blocks later batches.
    /// The batch still commits the blobs it did delete. If nothing in the batch could be
    /// deleted, the first delete error is returned (after committing the failure counts).
    pub fn prune_unreferenced(&self, store: &Store, limit: usize) -> Result<RetentionReport> {
        self.prune_unreferenced_with(store, limit)
    }

    /// [`Db::prune_unreferenced`] against any [`BlobFiles`] (tests use a fake store).
    pub(crate) fn prune_unreferenced_with(
        &self,
        files: &dyn BlobFiles,
        limit: usize,
    ) -> Result<RetentionReport> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let (report, first_err) = self.write(|tx| {
            let hashes = {
                let mut stmt = tx.prepare_cached(
                    "SELECT hash FROM blobs WHERE refs = 0
                     ORDER BY prune_failures, hash LIMIT ?1",
                )?;
                let rows = stmt
                    .query_map([limit], |r| r.get::<_, Vec<u8>>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.into_iter()
                    .map(codec::hash_from_sql)
                    .collect::<Result<Vec<_>>>()?
            };
            let mut report = RetentionReport::default();
            let mut first_err = None;
            let mut delete_row = tx.prepare_cached("DELETE FROM blobs WHERE hash = ?1")?;
            let mut count_failure = tx.prepare_cached(
                "UPDATE blobs SET prune_failures = prune_failures + 1 WHERE hash = ?1",
            )?;
            for hash in &hashes {
                match files.delete(hash) {
                    Ok(freed) => {
                        delete_row.execute([hash.0.as_slice()])?;
                        report.bytes_freed = report.bytes_freed.saturating_add(freed);
                        report.blobs_pruned = report.blobs_pruned.saturating_add(1);
                    }
                    Err(e) => {
                        count_failure.execute([hash.0.as_slice()])?;
                        first_err.get_or_insert(e);
                    }
                }
            }
            Ok((report, first_err))
        })?;
        match first_err {
            Some(e) if report.blobs_pruned == 0 => Err(e),
            _ => Ok(report),
        }
    }

    /// Delete every blob file no entry references, including orphans with no `blobs` row
    /// (written by a snapshot that never committed), plus stale temp files, and drop the
    /// `blobs` rows of unreferenced blobs. One `BEGIN IMMEDIATE` transaction; the referenced
    /// set is read inside it (DSNA-80 protocol, DSNA-88).
    ///
    /// Walks the whole store, so keep it off the hot path. A snapshot that stored a blob but
    /// has not committed yet is safe: the blob is new to the database, so its
    /// [`Db::insert_version`] re-checks it and returns [`Error::BlobMissing`].
    ///
    /// Reports blob files deleted and bytes freed (blobs and temp files); `versions_deleted`
    /// is 0. Files that could not be deleted are skipped and counted in `blobs_failed`; a
    /// later sweep retries them.
    pub fn sweep_orphans(&self, store: &Store) -> Result<RetentionReport> {
        self.sweep_orphans_with(store)
    }

    /// [`Db::sweep_orphans`] against any [`BlobSweep`] (tests use a fake store).
    pub(crate) fn sweep_orphans_with(&self, files: &dyn BlobSweep) -> Result<RetentionReport> {
        self.write(|tx| {
            let referenced = {
                let mut stmt = tx.prepare_cached("SELECT hash FROM blobs WHERE refs > 0")?;
                let rows = stmt
                    .query_map([], |r| r.get::<_, Vec<u8>>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.into_iter()
                    .map(codec::hash_from_sql)
                    .collect::<Result<HashSet<_>>>()?
            };
            let swept = files.sweep(&referenced)?;
            tx.execute("DELETE FROM blobs WHERE refs = 0", [])?;
            Ok(RetentionReport {
                versions_deleted: 0,
                blobs_pruned: u32::try_from(swept.deleted).unwrap_or(u32::MAX),
                bytes_freed: swept.bytes_freed,
                blobs_failed: u32::try_from(swept.failed).unwrap_or(u32::MAX),
            })
        })
    }

    /// Whether any entry references `hash`.
    pub fn blob_referenced(&self, hash: &BlobHash) -> Result<bool> {
        let conn = self.conn();
        is_referenced(&conn, hash)
    }

    /// Every blob hash that at least one entry references, sorted.
    pub fn referenced_hashes(&self) -> Result<Vec<BlobHash>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare_cached("SELECT hash FROM blobs WHERE refs > 0 ORDER BY hash")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(codec::hash_from_sql).collect()
    }

    /// Every entry that references `hash`, with its project and version, newest version
    /// first (blob repair, DSNA-99).
    ///
    /// Scans the whole `entries` table when the blob is referenced (there is no index on
    /// `blob_hash` since schema v3), so keep it off hot paths.
    pub fn entries_using_blob(
        &self,
        hash: &BlobHash,
    ) -> Result<Vec<(ProjectId, VersionId, Entry)>> {
        let conn = self.conn();
        if !is_referenced(&conn, hash)? {
            return Ok(Vec::new());
        }
        let cols = ENTRY_COLS
            .split(", ")
            .map(|c| format!("e.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT v.project_id, v.id, {cols} FROM entries e
             JOIN versions v ON v.id = e.version_id
             WHERE e.blob_hash = ?1 ORDER BY v.id DESC, e.path"
        ))?;
        let rows = stmt
            .query_map([hash.0.as_slice()], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    EntryRow::read_at(r, 2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(p, v, e)| Ok((ProjectId(p), VersionId(v), e.into_entry()?)))
            .collect()
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
    ///
    /// Combines `PRAGMA data_version` (bumped by commits from other connections) with this
    /// connection's total row-change count (bumped by its own writes). Both only grow, so any
    /// commit changes the token. Costs one pragma read; fine to poll every 500 ms. Only
    /// equality between two tokens from the same `Db` is meaningful.
    pub fn change_token(&self) -> Result<i64> {
        let conn = self.conn();
        let data_version: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        let own = i64::try_from(conn.total_changes()).unwrap_or(i64::MAX);
        Ok(data_version.wrapping_shl(32).wrapping_add(own))
    }
}

impl Dsnap {
    /// Token that changes when the database changes (from this or another process).
    pub fn db_change_token(&self) -> Result<i64> {
        self.db.change_token()
    }
}

/// Blob file access used by the lifetime protocol. [`Store`] is the real implementation.
pub(crate) trait BlobFiles {
    /// Compressed size of the blob file, or `None` if it is not stored (same rule as
    /// [`Store::contains`]: an unreadable or 0-length object counts as missing).
    fn present_size(&self, hash: &BlobHash) -> Option<u64>;
    /// Delete the blob file; bytes freed (0 if absent).
    fn delete(&self, hash: &BlobHash) -> Result<u64>;
}

impl BlobFiles for Store {
    fn present_size(&self, hash: &BlobHash) -> Option<u64> {
        self.stored_size(hash).ok().flatten()
    }

    fn delete(&self, hash: &BlobHash) -> Result<u64> {
        Store::delete(self, hash)
    }
}

/// What a store sweep removed.
pub(crate) struct Swept {
    /// Blob files deleted.
    pub(crate) deleted: u64,
    /// Bytes freed on disk (blobs and temp files).
    pub(crate) bytes_freed: u64,
    /// Unreferenced blob files that could not be deleted.
    pub(crate) failed: u64,
}

/// Whole-store sweep used by [`Db::sweep_orphans_with`]: delete every blob file whose hash
/// is not in `referenced` (and stale temp files). [`Store::sweep`] is the real implementation.
pub(crate) trait BlobSweep {
    /// Delete unreferenced blob files.
    fn sweep(&self, referenced: &HashSet<BlobHash>) -> Result<Swept>;
}

impl BlobSweep for Store {
    fn sweep(&self, referenced: &HashSet<BlobHash>) -> Result<Swept> {
        let r = Store::sweep(self, referenced)?;
        Ok(Swept {
            deleted: r.deleted,
            bytes_freed: r.bytes_freed,
            failed: r.failed,
        })
    }
}

/// Protocol step 2 (module docs): every blob the new entries use that no committed entry
/// references (row missing or `refs = 0`) must still exist in the store. Runs under the
/// write lock, after `new_blobs` rows are recorded and before the entries are inserted, so a
/// concurrent prune either finished (and the check sees the file gone) or cannot start until
/// this transaction ends. A present blob without a row gets one, because the entry triggers
/// refuse a reference to an unrecorded blob.
///
/// Blobs in `prev` (used by the project's newest committed version) are referenced, so they
/// are skipped without a lookup. Returns the distinct blobs of the new entries.
fn check_blobs_present(
    tx: &Transaction<'_>,
    v: &NewVersion,
    prev: &HashSet<BlobHash>,
    files: &dyn BlobFiles,
) -> Result<HashSet<BlobHash>> {
    let mut seen = HashSet::new();
    let mut refs_of = tx.prepare_cached("SELECT refs FROM blobs WHERE hash = ?1")?;
    for e in &v.entries {
        let Some(hash) = e.blob else { continue };
        if !seen.insert(hash) || prev.contains(&hash) {
            continue;
        }
        let refs: Option<i64> = refs_of
            .query_row([hash.0.as_slice()], |r| r.get(0))
            .optional()?;
        if refs.is_some_and(|n| n > 0) {
            continue;
        }
        let Some(stored_size) = files.present_size(&hash) else {
            return Err(Error::BlobMissing(hash));
        };
        if refs.is_none() {
            insert_blob_tx(
                tx,
                &BlobInfo {
                    hash,
                    size: e.size,
                    stored_size,
                },
            )?;
        }
    }
    Ok(seen)
}

fn is_referenced(conn: &Connection, hash: &BlobHash) -> Result<bool> {
    let mut stmt =
        conn.prepare_cached("SELECT EXISTS (SELECT 1 FROM blobs WHERE hash = ?1 AND refs > 0)")?;
    Ok(stmt.query_row([hash.0.as_slice()], |r| r.get(0))?)
}

fn insert_version_tx(
    tx: &Transaction<'_>,
    v: &NewVersion,
    files: &dyn BlobFiles,
) -> Result<Version> {
    let project_exists = tx
        .query_row(
            "SELECT 1 FROM projects WHERE id = ?1 AND removing = 0",
            [v.project_id.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !project_exists {
        return Err(not_found_project(v.project_id));
    }
    for b in &v.new_blobs {
        insert_blob_tx(tx, b)?;
    }
    let prev = refs::blob_set(tx, refs::predecessor(tx, v.project_id, None)?)?;
    let used = check_blobs_present(tx, v, &prev, files)?;
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
    refs::apply(tx, &refs::on_insert(&prev, &used))?;
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

/// Recomputes a version's stored counts from its predecessor's entries and its own.
pub type Recount<'a> = dyn Fn(&[Entry], &[Entry]) -> ChangeCounts + 'a;

fn delete_versions_tx(
    tx: &Transaction<'_>,
    ids: &[VersionId],
    recount: &Recount<'_>,
) -> Result<u32> {
    let mut deleted: Vec<(ProjectId, VersionId)> = Vec::new();
    for &id in ids {
        if let Some(project) = delete_version_row(tx, id)? {
            deleted.push((project, id));
        }
    }
    let mut successors = HashSet::new();
    for &(project, id) in &deleted {
        let next: Option<i64> = tx
            .query_row(
                "SELECT id FROM versions WHERE project_id = ?1 AND id > ?2 ORDER BY id LIMIT 1",
                params![project.0, id.0],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(next) = next {
            successors.insert((project, VersionId(next)));
        }
    }
    for (project, id) in successors {
        let prev: Option<i64> = tx
            .query_row(
                "SELECT id FROM versions WHERE project_id = ?1 AND id < ?2
                 ORDER BY id DESC LIMIT 1",
                params![project.0, id.0],
                |r| r.get(0),
            )
            .optional()?;
        let old = match prev {
            Some(p) => entries_of(tx, VersionId(p))?,
            None => Vec::new(),
        };
        let c = recount(&old, &entries_of(tx, id)?);
        tx.execute(
            "UPDATE versions SET added = ?2, modified = ?3, deleted = ?4 WHERE id = ?1",
            params![id.0, c.added, c.modified, c.deleted],
        )?;
    }
    Ok(u32::try_from(deleted.len()).unwrap_or(u32::MAX))
}

/// Delete one version (its entries cascade) and update the blob reference counts. Returns
/// its project, or `None` if it does not exist. The only code that deletes a version row.
fn delete_version_row(tx: &Transaction<'_>, id: VersionId) -> Result<Option<ProjectId>> {
    let Some(v) = version_by_id(tx, id)? else {
        return Ok(None);
    };
    let deltas = refs::on_delete(tx, v.project_id, id)?;
    tx.execute("DELETE FROM versions WHERE id = ?1", [id.0])?;
    refs::apply(tx, &deltas)?;
    Ok(Some(v.project_id))
}

/// `COMMIT` an open transaction, retrying while SQLite reports the database busy or locked,
/// up to the busy timeout. If it still fails, roll back and return the error.
fn commit_with_retry(conn: &Connection) -> Result<()> {
    let start = Instant::now();
    let mut pause = Duration::from_millis(2);
    loop {
        match conn.execute_batch("COMMIT") {
            Ok(()) => return Ok(()),
            Err(e) if is_busy(&e) && start.elapsed() < schema::BUSY_TIMEOUT => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e.into());
            }
        }
    }
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn projects_of(conn: &Connection, include_removing: bool) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {PROJECT_COLS} FROM projects WHERE removing = 0 OR ?1
         ORDER BY name COLLATE NOCASE, name, id"
    ))?;
    let rows = stmt
        .query_map([include_removing], ProjectRow::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(ProjectRow::into_project).collect()
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
    let primary_key = matches!(
        &e,
        rusqlite::Error::SqliteFailure(f, _)
            if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
    );
    if primary_key {
        Error::InvalidInput(format!("duplicate entry path {path}"))
    } else {
        e.into()
    }
}
