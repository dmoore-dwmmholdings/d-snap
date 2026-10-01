//! Retention and blob pruning. Owner: Chain L (DSNA-15).
//!
//! Blob files are deleted only through `Db::prune_unreferenced` (and `Db::sweep_orphans`),
//! under the DSNA-80 blob lifetime protocol in the [`crate::db`] module docs. That protocol
//! replaces the "only sweep objects older than 10 minutes" guard first planned in DSNA-55: a
//! snapshot that deduplicated against a blob being pruned gets `Error::BlobMissing` from
//! `Db::insert_version` and stores it again, so no age guard is needed.

use crate::db::PRUNE_BATCH;
use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::types::{ProjectId, RetentionReport};
use crate::versions::version_counts;

/// Versions deleted per write transaction during retention, so the write lock is released
/// between batches (a concurrent `dsnap snap` waits at most the 5 s busy timeout).
pub const RETENTION_BATCH: usize = 32;

impl Dsnap {
    /// Keep every pinned version plus the newest `retention_keep` unpinned ones (global
    /// setting, default 200); delete the rest, oldest first, recomputing the stored counts
    /// of the versions that follow them; then prune unreferenced blobs.
    ///
    /// The versions to delete are chosen inside each delete transaction, so a version pinned
    /// by another process meanwhile is kept. Blob files that cannot be deleted end the prune
    /// as a warning (`blobs_failed`), not an error.
    pub fn apply_retention(&self, project: ProjectId) -> Result<RetentionReport> {
        self.db.get_project(project)?;
        let keep = self.db.global_settings()?.retention_keep.max(1);
        let mut versions_deleted = 0u32;
        loop {
            let n =
                self.db
                    .delete_unpinned_beyond(project, keep, RETENTION_BATCH, &version_counts)?;
            if n == 0 {
                break;
            }
            versions_deleted = versions_deleted.saturating_add(n);
        }
        let mut report = self.prune_unreferenced_all()?;
        report.versions_deleted = versions_deleted;
        Ok(report)
    }

    /// Hook for snapshot (Chain K): call after a new version is committed. Runs
    /// [`Dsnap::apply_retention`] for `project`.
    pub fn after_snapshot(&self, project: ProjectId) -> Result<RetentionReport> {
        self.apply_retention(project)
    }

    /// Manual full prune: delete blobs no version references by calling
    /// `Db::prune_unreferenced` in batches, then run `Db::sweep_orphans` to remove blob files
    /// with no database row (left by snapshots that never committed) and stale temp files.
    ///
    /// The sweep walks the whole store while holding the database write lock, so a
    /// concurrent snapshot may wait or fail with a busy error; that is why only this manual
    /// action sweeps, never retention, version delete or project removal (DSNA-98). Blob
    /// files that cannot be deleted are reported in `blobs_failed`, not as an error. Never
    /// delete store files directly (see the `db` module docs, DSNA-80).
    pub fn prune_blobs(&self) -> Result<RetentionReport> {
        let mut total = self.prune_unreferenced_all()?;
        match self.db.sweep_orphans(&self.store) {
            Ok(r) => {
                total.blobs_pruned = total.blobs_pruned.saturating_add(r.blobs_pruned);
                total.bytes_freed = total.bytes_freed.saturating_add(r.bytes_freed);
            }
            // The sweep stops at the first file it cannot delete and rolls back; what it
            // deleted is gone either way and the next sweep retries the rest.
            Err(Error::Io { .. }) => total.blobs_failed = total.blobs_failed.max(1),
            Err(e) => return Err(e),
        }
        Ok(total)
    }

    /// Call [`crate::db::Db::prune_unreferenced`] with [`PRUNE_BATCH`] until it frees
    /// nothing (DSNA-80 protocol; the only way this crate deletes blob files).
    ///
    /// Once only undeletable blobs are left, `prune_unreferenced` returns the file delete
    /// error. That ends the loop as a warning: the bytes already freed are kept and
    /// `blobs_failed` counts the unreferenced blobs left behind. Database errors propagate.
    pub(crate) fn prune_unreferenced_all(&self) -> Result<RetentionReport> {
        let mut total = RetentionReport::default();
        loop {
            match self.db.prune_unreferenced(&self.store, PRUNE_BATCH) {
                Ok(r) if r.blobs_pruned == 0 => return Ok(total),
                Ok(r) => {
                    total.blobs_pruned = total.blobs_pruned.saturating_add(r.blobs_pruned);
                    total.bytes_freed = total.bytes_freed.saturating_add(r.bytes_freed);
                }
                Err(Error::Io { .. }) => {
                    // Reporting only: the count is not used to delete anything.
                    let left = self.db.unreferenced_blobs()?.len();
                    total.blobs_failed = u32::try_from(left).unwrap_or(u32::MAX).max(1);
                    return Ok(total);
                }
                Err(e) => return Err(e),
            }
        }
    }
}
