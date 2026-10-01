//! Project and global settings. Owner: Chain L (DSNA-15).
#![allow(unused_variables)] // stub signatures; remove when implemented

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{GlobalSettings, ProjectId, ProjectSettings};

impl Dsnap {
    /// A project's settings.
    pub fn project_settings(&self, id: ProjectId) -> Result<ProjectSettings> {
        todo!("DSNA-15")
    }

    /// Replace a project's settings.
    pub fn set_project_settings(&self, id: ProjectId, settings: &ProjectSettings) -> Result<()> {
        todo!("DSNA-15")
    }

    /// App-wide settings.
    pub fn global_settings(&self) -> Result<GlobalSettings> {
        todo!("DSNA-15")
    }

    /// Replace app-wide settings.
    pub fn set_global_settings(&self, settings: &GlobalSettings) -> Result<()> {
        todo!("DSNA-15")
    }
}
