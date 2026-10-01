//! Entry-level comparison of two file lists. Owner: Chain F (DSNA-9).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{ChangeCounts, Entry, FileChange, ProjectId, VersionId, VersionRef};

/// Compare two path-sorted entry lists. With `detect_renames`, an add and a delete with the
/// same blob hash become one [`crate::ChangeStatus::Renamed`]. Line counts are left `None`.
pub fn diff_entries(old: &[Entry], new: &[Entry], detect_renames: bool) -> Vec<FileChange> {
    todo!("DSNA-9")
}

/// Count added/modified/deleted (a rename counts as modified).
pub fn count_changes(changes: &[FileChange]) -> ChangeCounts {
    todo!("DSNA-9")
}

impl Dsnap {
    /// Changes from `from` to `to`.
    ///
    /// `from = None` means the version before `to` (for [`VersionRef::WorkingTree`], the latest
    /// version); an empty list is used when there is none. Working-tree entries come from
    /// [`Dsnap::working_entries`].
    pub fn changes(
        &self,
        project: ProjectId,
        from: Option<VersionId>,
        to: VersionRef,
    ) -> Result<Vec<FileChange>> {
        todo!("DSNA-9")
    }
}
