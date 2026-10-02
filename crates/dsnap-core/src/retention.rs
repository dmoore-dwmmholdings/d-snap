//! Retention and blob pruning. Owner: Chain L (DSNA-15).
//!
//! Blob files are deleted only through `Db::prune_unreferenced` (and `Db::sweep_orphans`),
//! under the DSNA-80 blob lifetime protocol in the [`crate::db`] module docs. That protocol
//! replaces the "only sweep objects older than 10 minutes" guard first planned in DSNA-55: a
//! snapshot that deduplicated against a blob being pruned gets `Error::BlobMissing` from
//! `Db::insert_version` and stores it again, so no age guard is needed.
//!
//! Also blob repair ([`Dsnap::repair_blobs`]), the other store maintenance step.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::db::PRUNE_BATCH;
use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::store::RepairOutcome;
use crate::types::{
    BlobHash, EntryKind, ProjectId, RetentionReport, Version, VersionId, VersionKind,
};
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
        self.apply_retention_protecting(project, &[])
    }

    /// [`Dsnap::apply_retention`], also keeping the versions in `protect`: they are treated
    /// like pinned versions (never deleted, not counted toward `retention_keep`), checked
    /// inside each delete transaction.
    ///
    /// Restore calls this once it has finished, protecting its target and its safety
    /// version, so the version the user just restored to is not pruned by that run.
    pub fn apply_retention_protecting(
        &self,
        project: ProjectId,
        protect: &[VersionId],
    ) -> Result<RetentionReport> {
        self.db.get_project(project)?;
        let keep = self.db.global_settings()?.retention_keep.max(1);
        let mut versions_deleted = 0u32;
        loop {
            let n = self.db.delete_unpinned_beyond(
                project,
                keep,
                protect,
                RETENTION_BATCH,
                &version_counts,
            )?;
            if n == 0 {
                break;
            }
            versions_deleted = versions_deleted.saturating_add(n);
        }
        let mut report = self.prune_unreferenced_all()?;
        report.versions_deleted = versions_deleted;
        Ok(report)
    }

    /// Hook for snapshot (Chain K): call after `version` has committed. Runs
    /// [`Dsnap::apply_retention`] for its project, except for a
    /// [`VersionKind::Safety`] version, where it does nothing: a restore takes that snapshot
    /// before writing, and retention then could delete the very version being restored
    /// (Rule 1). Restore runs [`Dsnap::apply_retention_protecting`] itself once it is done.
    ///
    /// The version is already committed when this runs, so callers must treat an error as a
    /// warning, never as a failed snapshot.
    pub fn after_snapshot(&self, version: &Version) -> Result<RetentionReport> {
        if version.kind == VersionKind::Safety {
            return Ok(RetentionReport::default());
        }
        self.apply_retention(version.project_id)
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

/// What [`Dsnap::repair_blobs`] did for one damaged blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobRepair {
    /// The damaged blob.
    pub hash: BlobHash,
    /// Outcome.
    pub outcome: BlobRepairOutcome,
}

/// Outcome of repairing one blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BlobRepairOutcome {
    /// Rewritten from a working-tree file whose content still has this hash.
    Repaired {
        /// File the content came from.
        source: PathBuf,
    },
    /// Intact by the time it was repaired (e.g. another process repaired it).
    Healthy,
    /// No working-tree file still holds this content; these versions cannot restore it.
    NoIntactSource {
        /// Versions with an entry using the blob, newest first.
        versions: Vec<VersionId>,
        /// The last I/O error met while trying sources or reading the object, if any.
        error: Option<String>,
    },
    /// No entry references the blob, so it is left for pruning and not repaired.
    Unreferenced,
}

impl Dsnap {
    /// Find damaged blobs ([`crate::store::Store::verify_all`]) and rewrite each from a
    /// working-tree file that still holds its content (DSNA-99).
    ///
    /// Objects `verify_all` could not read (`Error::Io`) are skipped: they may be fine. Only
    /// blobs that an entry references are repaired; sources are tried newest version first,
    /// each working file once, and are stored only if they hash to the blob (`repair_file`
    /// checks). Repair renames over the object and never deletes, so it needs no DB lock.
    /// Returns one record per damaged blob, sorted by hash.
    pub fn repair_blobs(&self) -> Result<Vec<BlobRepair>> {
        let mut out = Vec::new();
        for (hash, err) in self.store.verify_all()? {
            if matches!(err, Error::Io { .. }) {
                continue;
            }
            let outcome = self.repair_one(&hash)?;
            out.push(BlobRepair { hash, outcome });
        }
        Ok(out)
    }

    fn repair_one(&self, hash: &BlobHash) -> Result<BlobRepairOutcome> {
        let uses = self.db.entries_using_blob(hash)?;
        if uses.is_empty() {
            return Ok(BlobRepairOutcome::Unreferenced);
        }
        let mut roots: HashMap<ProjectId, Option<PathBuf>> = HashMap::new();
        let mut tried = HashSet::new();
        let mut versions = Vec::new();
        let mut error = None;
        for (project, version, entry) in &uses {
            if versions.last() != Some(version) {
                versions.push(*version);
            }
            if entry.kind != EntryKind::File || !tried.insert((*project, entry.path.clone())) {
                continue;
            }
            let root = match roots.get(project) {
                Some(r) => r.clone(),
                None => {
                    let r = match self.db.get_project(*project) {
                        Ok(p) => Some(p.root),
                        Err(Error::NotFound(_)) => None,
                        Err(e) => return Err(e),
                    };
                    roots.insert(*project, r.clone());
                    r
                }
            };
            let Some(root) = root else { continue };
            let source = entry.path.to_path(&root);
            // Only a regular file (not a link to one) is a source for a file entry.
            match std::fs::symlink_metadata(&source) {
                Ok(m) if m.is_file() => {}
                _ => continue,
            }
            match self.store.repair_file(hash, &source) {
                Ok(RepairOutcome::Repaired { .. }) => {
                    return Ok(BlobRepairOutcome::Repaired { source });
                }
                Ok(RepairOutcome::Healthy) => return Ok(BlobRepairOutcome::Healthy),
                Ok(RepairOutcome::SourceMismatch { .. }) => {}
                Err(e @ Error::Io { .. }) => error = Some(e.to_string()),
                Err(e) => return Err(e),
            }
        }
        Ok(BlobRepairOutcome::NoIntactSource { versions, error })
    }
}
