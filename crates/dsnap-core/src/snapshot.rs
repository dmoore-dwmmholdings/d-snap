//! Taking snapshots. Owner: Chain K (DSNA-14).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{ProjectId, SnapshotOptions, SnapshotReport};

impl Dsnap {
    /// Capture the project folder. Returns `version: None` when nothing changed (F7).
    pub fn snapshot(&self, project: ProjectId, opts: SnapshotOptions) -> Result<SnapshotReport> {
        todo!("DSNA-14")
    }
}
