//! Retention and blob pruning. Owner: Chain L (DSNA-15).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{ProjectId, RetentionReport};

impl Dsnap {
    /// Keep pinned versions plus the newest `retention_keep`; delete the rest and prune blobs.
    pub fn apply_retention(&self, project: ProjectId) -> Result<RetentionReport> {
        todo!("DSNA-15")
    }

    /// Delete blobs no version references.
    pub fn prune_blobs(&self) -> Result<RetentionReport> {
        todo!("DSNA-15")
    }
}
