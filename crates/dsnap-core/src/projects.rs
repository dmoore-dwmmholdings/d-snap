//! Project management. Owner: Chain L (DSNA-15).
#![allow(unused_variables)] // stub signatures; remove when implemented

use std::path::Path;

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{Project, ProjectId};

impl Dsnap {
    /// Track the folder at `root`; `name` defaults to the folder name.
    pub fn add_project(&self, root: &Path, name: Option<&str>) -> Result<Project> {
        todo!("DSNA-15")
    }

    /// All projects, with `missing` set for folders that no longer exist.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        todo!("DSNA-15")
    }

    /// One project.
    pub fn project(&self, id: ProjectId) -> Result<Project> {
        todo!("DSNA-15")
    }

    /// Rename a project.
    pub fn rename_project(&self, id: ProjectId, name: &str) -> Result<()> {
        todo!("DSNA-15")
    }

    /// Stop tracking a project; with `delete_snapshots`, also delete its versions and prune blobs.
    pub fn remove_project(&self, id: ProjectId, delete_snapshots: bool) -> Result<()> {
        todo!("DSNA-15")
    }

    /// Point a project at a moved folder.
    pub fn relocate_project(&self, id: ProjectId, new_root: &Path) -> Result<Project> {
        todo!("DSNA-15")
    }
}
