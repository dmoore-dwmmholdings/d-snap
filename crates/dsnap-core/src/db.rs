//! SQLite index: projects, versions, entries, blobs, settings. Owner: Chain D (DSNA-7).
//!
//! Every write that creates a version runs in one transaction. Sets a busy timeout so the CLI
//! and the app can share the file.
#![allow(unused_variables)] // stub signatures; remove when implemented

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{
    BlobHash, BlobInfo, ChangeCounts, Entry, GlobalSettings, Project, ProjectId, ProjectSettings,
    RelPath, Version, VersionId, VersionKind,
};

/// Database handle. Thread-safe; serialises access to one connection.
#[derive(Debug)]
pub struct Db {
    #[allow(dead_code)] // used once Chain D implements the db
    pub(crate) conn: Mutex<Connection>,
}

/// Input for [`Db::insert_version`].
#[derive(Debug, Clone)]
pub struct NewVersion {
    /// Owning project.
    pub project_id: ProjectId,
    /// Label.
    pub label: String,
    /// Creation time, ms since the Unix epoch.
    pub created_at_ms: i64,
    /// How it was created.
    pub kind: VersionKind,
    /// Some files kept changing during capture.
    pub unstable: bool,
    /// Counts versus the previous version.
    pub counts: ChangeCounts,
    /// Full file list, sorted by path.
    pub entries: Vec<Entry>,
    /// Blobs written for this version (inserted if not already recorded).
    pub new_blobs: Vec<BlobInfo>,
}

impl Db {
    /// Open or create the database at `path` and apply migrations.
    pub fn open(path: &Path) -> Result<Self> {
        todo!("DSNA-7")
    }

    /// Open a private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        todo!("DSNA-7")
    }

    /// Insert a project and return its id.
    pub fn insert_project(
        &self,
        name: &str,
        root: &Path,
        settings: &ProjectSettings,
    ) -> Result<ProjectId> {
        todo!("DSNA-7")
    }

    /// Load one project (`missing` is always `false`; the caller checks the folder).
    pub fn get_project(&self, id: ProjectId) -> Result<Project> {
        todo!("DSNA-7")
    }

    /// All projects ordered by name.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        todo!("DSNA-7")
    }

    /// Change a project's display name.
    pub fn set_project_name(&self, id: ProjectId, name: &str) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Change a project's root folder.
    pub fn set_project_root(&self, id: ProjectId, root: &Path) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Replace a project's settings.
    pub fn set_project_settings(&self, id: ProjectId, settings: &ProjectSettings) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Delete a project with all its versions and entries (blobs are left for pruning).
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Insert a version, its entries and new blob rows in one transaction.
    pub fn insert_version(&self, v: &NewVersion) -> Result<Version> {
        todo!("DSNA-7")
    }

    /// Load one version.
    pub fn get_version(&self, id: VersionId) -> Result<Version> {
        todo!("DSNA-7")
    }

    /// A project's versions, newest first.
    pub fn list_versions(&self, project: ProjectId) -> Result<Vec<Version>> {
        todo!("DSNA-7")
    }

    /// A project's newest version.
    pub fn latest_version(&self, project: ProjectId) -> Result<Option<Version>> {
        todo!("DSNA-7")
    }

    /// The version created just before `id` in the same project.
    pub fn previous_version(&self, id: VersionId) -> Result<Option<Version>> {
        todo!("DSNA-7")
    }

    /// A version's entries, sorted by path.
    pub fn entries(&self, version: VersionId) -> Result<Vec<Entry>> {
        todo!("DSNA-7")
    }

    /// One entry of a version.
    pub fn entry(&self, version: VersionId, path: &RelPath) -> Result<Option<Entry>> {
        todo!("DSNA-7")
    }

    /// Change a version's label.
    pub fn set_version_label(&self, id: VersionId, label: &str) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Pin or unpin a version.
    pub fn set_version_pinned(&self, id: VersionId, pinned: bool) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Delete a version and its entries.
    pub fn delete_version(&self, id: VersionId) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Record a blob (no-op if present).
    pub fn insert_blob(&self, blob: &BlobInfo) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Blobs no entry references.
    pub fn unreferenced_blobs(&self) -> Result<Vec<BlobInfo>> {
        todo!("DSNA-7")
    }

    /// Remove a blob row.
    pub fn delete_blob(&self, hash: &BlobHash) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Stored global settings (defaults if never set).
    pub fn global_settings(&self) -> Result<GlobalSettings> {
        todo!("DSNA-7")
    }

    /// Replace global settings.
    pub fn set_global_settings(&self, settings: &GlobalSettings) -> Result<()> {
        todo!("DSNA-7")
    }

    /// Value that changes whenever any connection commits a write. Poll it to detect writes
    /// from another process (e.g. the CLI).
    pub fn change_token(&self) -> Result<i64> {
        todo!("DSNA-7")
    }
}

impl Dsnap {
    /// Token that changes when the database changes (from this or another process).
    pub fn db_change_token(&self) -> Result<i64> {
        todo!("DSNA-7")
    }
}
