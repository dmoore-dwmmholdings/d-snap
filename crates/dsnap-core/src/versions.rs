//! Listing and editing versions. Owner: Chain L (DSNA-15).

use crate::diff::entries::{EntryDiffOptions, count_changes, diff_entries};
use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::projects::valid_name;
use crate::types::{ChangeCounts, Entry, ProjectId, Version, VersionId};

/// Stored counts of a version whose predecessor has entries `old` (empty for the first
/// version) and which itself has entries `new`.
///
/// Snapshot (Chain K) must store counts computed by this function so that counts
/// recomputed after a delete agree with counts stored at insert.
pub fn version_counts(old: &[Entry], new: &[Entry]) -> ChangeCounts {
    count_changes(&diff_entries(old, new, EntryDiffOptions::default()))
}

impl Dsnap {
    /// A project's versions, newest first. [`Error::NotFound`] for an unknown project.
    pub fn list_versions(&self, project: ProjectId) -> Result<Vec<Version>> {
        self.db.get_project(project)?;
        self.db.list_versions(project)
    }

    /// One version.
    pub fn version(&self, id: VersionId) -> Result<Version> {
        self.db.get_version(id)
    }

    /// Change a version's label (trimmed, non-empty, at most
    /// [`crate::projects::MAX_NAME_CHARS`] characters).
    pub fn set_label(&self, id: VersionId, label: &str) -> Result<()> {
        let label = valid_name(label, "label")?;
        self.db.set_version_label(id, &label)
    }

    /// Pin (never pruned) or unpin a version.
    pub fn set_pinned(&self, id: VersionId, pinned: bool) -> Result<()> {
        self.db.set_version_pinned(id, pinned)
    }

    /// Delete a version (any kind, pinned or not), then prune unreferenced blobs in batches
    /// through `Db::prune_unreferenced`.
    ///
    /// The next version's stored counts are recomputed against its new predecessor in the
    /// same transaction. Blob files that cannot be deleted are left for the next prune and
    /// are not an error here.
    pub fn delete_version(&self, id: VersionId) -> Result<()> {
        if self.db.delete_versions(&[id], &version_counts)? == 0 {
            return Err(Error::NotFound(format!("version {id}")));
        }
        self.prune_unreferenced_all()?;
        Ok(())
    }
}
