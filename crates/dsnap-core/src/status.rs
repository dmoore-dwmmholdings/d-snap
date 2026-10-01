//! Working-tree status versus the latest version. Owner: Chain K (DSNA-14).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{Entry, FileChange, ProjectId};

impl Dsnap {
    /// Changes in the folder since the latest version (the "Unsaved changes" row and badge).
    pub fn status(&self, project: ProjectId) -> Result<Vec<FileChange>> {
        todo!("DSNA-14")
    }

    /// Current folder entries with hashes (reusing stored hashes when size and mtime match).
    /// Stores nothing.
    pub fn working_entries(&self, project: ProjectId) -> Result<Vec<Entry>> {
        todo!("DSNA-14")
    }
}
