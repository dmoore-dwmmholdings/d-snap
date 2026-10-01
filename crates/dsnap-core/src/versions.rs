//! Listing and editing versions. Owner: Chain L (DSNA-15).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{ProjectId, Version, VersionId};

impl Dsnap {
    /// A project's versions, newest first.
    pub fn list_versions(&self, project: ProjectId) -> Result<Vec<Version>> {
        todo!("DSNA-15")
    }

    /// One version.
    pub fn version(&self, id: VersionId) -> Result<Version> {
        todo!("DSNA-15")
    }

    /// Change a version's label.
    pub fn set_label(&self, id: VersionId, label: &str) -> Result<()> {
        todo!("DSNA-15")
    }

    /// Pin (never pruned) or unpin a version.
    pub fn set_pinned(&self, id: VersionId, pinned: bool) -> Result<()> {
        todo!("DSNA-15")
    }

    /// Delete a version and prune blobs it alone used.
    pub fn delete_version(&self, id: VersionId) -> Result<()> {
        todo!("DSNA-15")
    }
}
