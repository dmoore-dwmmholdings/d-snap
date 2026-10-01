//! Retention and blob pruning. Owner: Chain L (DSNA-15).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::db::PRUNE_BATCH;
use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::types::{ProjectId, RetentionReport};

impl Dsnap {
    /// Keep pinned versions plus the newest `retention_keep`; delete the rest and prune blobs.
    pub fn apply_retention(&self, project: ProjectId) -> Result<RetentionReport> {
        todo!("DSNA-15")
    }

    /// Delete blobs no version references, by calling `Db::prune_unreferenced` in batches.
    /// Never delete store files directly (see the `db` module docs, DSNA-80).
    pub fn prune_blobs(&self) -> Result<RetentionReport> {
        todo!("DSNA-15")
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
