//! Restoring files and projects. Owner: Chain M (DSNA-16).
//!
//! Rule 1: every restore first takes a safety snapshot; if that fails, nothing is written
//! ([`crate::Error::SafetySnapshotFailed`]).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{Hunk, ProjectId, RelPath, RestorePlan, RestoreReport, VersionId};

impl Dsnap {
    /// What restoring the whole project to `target` would write, delete and create.
    pub fn restore_plan(&self, project: ProjectId, target: VersionId) -> Result<RestorePlan> {
        todo!("DSNA-16")
    }

    /// Restore the whole project to `target` (never touching ignored paths).
    pub fn restore_project(&self, project: ProjectId, target: VersionId) -> Result<RestoreReport> {
        todo!("DSNA-16")
    }

    /// Restore one file (including a deleted one) to its content in `version`.
    pub fn restore_file(
        &self,
        project: ProjectId,
        version: VersionId,
        path: &RelPath,
    ) -> Result<RestoreReport> {
        todo!("DSNA-16")
    }

    /// Undo one hunk of the diff from `base` to the working file at `path`.
    pub fn revert_hunk(
        &self,
        project: ProjectId,
        base: VersionId,
        path: &RelPath,
        hunk: &Hunk,
    ) -> Result<RestoreReport> {
        todo!("DSNA-16")
    }
}
