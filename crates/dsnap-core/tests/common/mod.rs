//! Helpers shared by the Chain L integration tests. Versions are committed through a second
//! `Db`/`Store` handle on the same home, as the CLI would, until snapshot (Chain K) exists.
#![allow(clippy::unwrap_used, dead_code)]

use std::path::Path;

use dsnap_core::db::{Db, NewVersion};
use dsnap_core::store::Store;
use dsnap_core::versions::version_counts;
use dsnap_core::{BlobHash, Entry, EntryKind, ProjectId, RelPath, Version, VersionKind};

/// A second handle on a test home.
pub struct Cli {
    pub db: Db,
    pub store: Store,
}

impl Cli {
    pub fn open(home: &Path) -> Self {
        Self {
            db: Db::open(&home.join("dsnap.db")).unwrap(),
            store: Store::open(home.join("objects")).unwrap(),
        }
    }

    /// Commit a version holding exactly `files` (path, content), with counts against the
    /// project's latest version.
    pub fn commit(&self, project: ProjectId, files: &[(&str, &[u8])]) -> Version {
        let mut entries = Vec::new();
        let mut new_blobs = Vec::new();
        for (path, content) in files {
            let info = self.store.put(content).unwrap();
            new_blobs.push(info);
            entries.push(Entry {
                path: RelPath::new(*path).unwrap(),
                kind: EntryKind::File,
                blob: Some(info.hash),
                size: info.size,
                mtime_ns: 0,
                readonly: false,
            });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let prev = match self.db.latest_version(project).unwrap() {
            Some(v) => self.db.entries(v.id).unwrap(),
            None => Vec::new(),
        };
        self.db
            .insert_version(
                &NewVersion {
                    project_id: project,
                    label: "v".into(),
                    created_at_ms: 0,
                    kind: VersionKind::Cli,
                    unstable: false,
                    counts: version_counts(&prev, &entries),
                    entries,
                    new_blobs,
                },
                &self.store,
            )
            .unwrap()
    }

    /// Commit a version with one file `f` holding `content`; returns its blob hash.
    pub fn commit_one(&self, project: ProjectId, content: &[u8]) -> BlobHash {
        self.commit(project, &[("f", content)]);
        dsnap_core::store::hash_bytes(content)
    }
}
