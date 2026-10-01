//! Conversions between Rust types and SQL column values.

use std::path::{Path, PathBuf};

use rusqlite::Row;

use crate::error::{Error, Result};
use crate::types::{
    BlobHash, BlobInfo, ChangeCounts, Entry, EntryKind, Project, ProjectId, ProjectSettings,
    RelPath, Version, VersionId, VersionKind,
};

/// Column list matching [`VersionRow::read`].
pub(crate) const VERSION_COLS: &str =
    "id, project_id, label, created_at_ms, kind, pinned, unstable, added, modified, deleted";

/// Column list matching [`EntryRow::read`].
pub(crate) const ENTRY_COLS: &str = "path, kind, blob_hash, size, mtime_ns, readonly, link_target";

/// Column list matching [`ProjectRow::read`].
pub(crate) const PROJECT_COLS: &str = "id, name, root_path, settings_json";

/// Column list matching [`blob_from_row`].
pub(crate) const BLOB_COLS: &str = "hash, size, stored_size";

pub(crate) fn u64_to_sql(v: u64, what: &str) -> Result<i64> {
    i64::try_from(v).map_err(|_| Error::InvalidInput(format!("{what} {v} is too large")))
}

fn u64_from_sql(v: i64, what: &str) -> Result<u64> {
    u64::try_from(v).map_err(|_| Error::Corrupt(format!("negative {what} {v}")))
}

fn u32_from_sql(v: i64, what: &str) -> Result<u32> {
    u32::try_from(v).map_err(|_| Error::Corrupt(format!("{what} {v} out of range")))
}

pub(crate) fn hash_from_sql(bytes: Vec<u8>) -> Result<BlobHash> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|b: Vec<u8>| Error::Corrupt(format!("blob hash has {} bytes", b.len())))?;
    Ok(BlobHash(arr))
}

pub(crate) fn version_kind_to_sql(k: VersionKind) -> &'static str {
    match k {
        VersionKind::Manual => "manual",
        VersionKind::Auto => "auto",
        VersionKind::Cli => "cli",
        VersionKind::Safety => "safety",
    }
}

fn version_kind_from_sql(s: &str) -> Result<VersionKind> {
    Ok(match s {
        "manual" => VersionKind::Manual,
        "auto" => VersionKind::Auto,
        "cli" => VersionKind::Cli,
        "safety" => VersionKind::Safety,
        other => return Err(Error::Corrupt(format!("unknown version kind {other:?}"))),
    })
}

pub(crate) fn root_to_sql(root: &Path) -> Result<&str> {
    root.to_str().ok_or_else(|| {
        Error::InvalidInput(format!(
            "project folder path is not valid UTF-8: {}",
            root.display()
        ))
    })
}

pub(crate) fn settings_to_sql<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| Error::InvalidInput(format!("settings: {e}")))
}

pub(crate) fn settings_from_sql<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|e| Error::Corrupt(format!("settings JSON: {e}")))
}

/// Raw `versions` row; converted with [`VersionRow::into_version`].
pub(crate) struct VersionRow {
    id: i64,
    project_id: i64,
    label: String,
    created_at_ms: i64,
    kind: String,
    pinned: bool,
    unstable: bool,
    added: i64,
    modified: i64,
    deleted: i64,
}

impl VersionRow {
    pub(crate) fn read(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            project_id: r.get(1)?,
            label: r.get(2)?,
            created_at_ms: r.get(3)?,
            kind: r.get(4)?,
            pinned: r.get(5)?,
            unstable: r.get(6)?,
            added: r.get(7)?,
            modified: r.get(8)?,
            deleted: r.get(9)?,
        })
    }

    pub(crate) fn into_version(self) -> Result<Version> {
        Ok(Version {
            id: VersionId(self.id),
            project_id: ProjectId(self.project_id),
            label: self.label,
            created_at_ms: self.created_at_ms,
            kind: version_kind_from_sql(&self.kind)?,
            pinned: self.pinned,
            unstable: self.unstable,
            counts: ChangeCounts {
                added: u32_from_sql(self.added, "added count")?,
                modified: u32_from_sql(self.modified, "modified count")?,
                deleted: u32_from_sql(self.deleted, "deleted count")?,
            },
        })
    }
}

/// Raw `entries` row; converted with [`EntryRow::into_entry`].
pub(crate) struct EntryRow {
    path: String,
    kind: String,
    blob: Option<Vec<u8>>,
    size: i64,
    mtime_ns: i64,
    readonly: bool,
    link_target: Option<String>,
}

impl EntryRow {
    pub(crate) fn read(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            path: r.get(0)?,
            kind: r.get(1)?,
            blob: r.get(2)?,
            size: r.get(3)?,
            mtime_ns: r.get(4)?,
            readonly: r.get(5)?,
            link_target: r.get(6)?,
        })
    }

    pub(crate) fn into_entry(self) -> Result<Entry> {
        let kind = match (self.kind.as_str(), self.link_target) {
            ("file", _) => EntryKind::File,
            ("dir", _) => EntryKind::Dir,
            ("symlink", Some(target)) => EntryKind::Symlink { target },
            ("symlink", None) => {
                return Err(Error::Corrupt(format!(
                    "symlink entry {} has no target",
                    self.path
                )));
            }
            (other, _) => return Err(Error::Corrupt(format!("unknown entry kind {other:?}"))),
        };
        let path = RelPath::new(self.path)
            .map_err(|e| Error::Corrupt(format!("stored entry path: {e}")))?;
        Ok(Entry {
            path,
            kind,
            blob: self.blob.map(hash_from_sql).transpose()?,
            size: u64_from_sql(self.size, "entry size")?,
            mtime_ns: self.mtime_ns,
            readonly: self.readonly,
        })
    }
}

/// `(kind, link_target)` columns for an entry kind.
pub(crate) fn entry_kind_to_sql(k: &EntryKind) -> (&'static str, Option<&str>) {
    match k {
        EntryKind::File => ("file", None),
        EntryKind::Dir => ("dir", None),
        EntryKind::Symlink { target } => ("symlink", Some(target.as_str())),
    }
}

/// Raw `projects` row; converted with [`ProjectRow::into_project`].
pub(crate) struct ProjectRow {
    id: i64,
    name: String,
    root: String,
    settings_json: String,
}

impl ProjectRow {
    pub(crate) fn read(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            root: r.get(2)?,
            settings_json: r.get(3)?,
        })
    }

    pub(crate) fn into_project(self) -> Result<Project> {
        Ok(Project {
            id: ProjectId(self.id),
            name: self.name,
            root: PathBuf::from(self.root),
            settings: settings_from_sql::<ProjectSettings>(&self.settings_json)?,
            missing: false,
        })
    }
}

/// Raw `blobs` row.
pub(crate) type BlobRow = (Vec<u8>, i64, i64);

pub(crate) fn blob_row(r: &Row<'_>) -> rusqlite::Result<BlobRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
}

pub(crate) fn blob_from_row((hash, size, stored): BlobRow) -> Result<BlobInfo> {
    Ok(BlobInfo {
        hash: hash_from_sql(hash)?,
        size: u64_from_sql(size, "blob size")?,
        stored_size: u64_from_sql(stored, "blob stored size")?,
    })
}
