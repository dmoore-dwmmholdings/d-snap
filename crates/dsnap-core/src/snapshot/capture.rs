//! The capture engine shared by [`Dsnap::snapshot`] and the working-tree status
//! ([`Dsnap::status`], [`Dsnap::working_entries`]).
//!
//! 1. Walk the project with one [`IgnoreRules`] and the global size cap.
//! 2. Fast path: a file whose kind, size and mtime match its entry in the latest version
//!    reuses that entry's hash without being opened.
//! 3. Every other file is hashed (and, in [`Mode::Store`], stored) in parallel on the rayon
//!    pool, which is bounded by the CPU count.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::ignore_rules::IgnoreRules;
use crate::store::{Store, hash_file};
use crate::types::{
    BlobHash, BlobInfo, CancelToken, Entry, EntryKind, Progress, ProgressEvent, Project, ProjectId,
    RelPath, SkipReason, SkippedFile, Stage, Version,
};
use crate::walk::{WalkOptions, walk};

/// A hash progress event is sent after this many files.
const HASH_PROGRESS_EVERY: u64 = 32;

/// Whether hashed files are also written to the blob store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Hash and store (snapshot).
    Store,
    /// Hash only; nothing is written (status).
    #[allow(dead_code)] // used by status (DSNA-51)
    HashOnly,
}

/// Progress and cancellation for one capture.
#[derive(Debug, Clone, Default)]
pub(crate) struct Hooks {
    pub(crate) progress: Option<Progress>,
    pub(crate) cancel: Option<CancelToken>,
}

impl Hooks {
    pub(crate) fn check_cancel(&self) -> Result<()> {
        match &self.cancel {
            Some(c) => c.check(),
            None => Ok(()),
        }
    }

    fn report(&self, stage: Stage, done: u64, total: Option<u64>, path: Option<&RelPath>) {
        if let Some(p) = &self.progress {
            p.report(&ProgressEvent {
                stage,
                done,
                total,
                path: path.cloned(),
            });
        }
    }
}

/// Result of [`Dsnap::capture`].
#[derive(Debug)]
pub(crate) struct Capture {
    /// The project folder.
    #[allow(dead_code)] // used by status (DSNA-51)
    pub(crate) root: PathBuf,
    /// The rules the walk used (reuse them to decide other paths in the same operation).
    #[allow(dead_code)] // used by status (DSNA-51)
    pub(crate) rules: IgnoreRules,
    /// The project's newest version when the capture started.
    pub(crate) latest: Option<Version>,
    /// That version's entries, sorted by path (empty without a version).
    pub(crate) latest_entries: Vec<Entry>,
    /// The folder now, sorted by path. Files have `blob: Some` (see [`Dsnap::capture`]).
    pub(crate) entries: Vec<Entry>,
    /// Paths left out, sorted by path.
    pub(crate) skipped: Vec<SkippedFile>,
    /// Files that kept changing while they were captured.
    pub(crate) unstable_paths: Vec<RelPath>,
    /// Blobs written to the store ([`Mode::Store`] only), one per hash.
    pub(crate) new_blobs: Vec<BlobInfo>,
}

/// Outcome of hashing one file.
enum Hashed {
    /// Captured content. `size` is the number of bytes hashed.
    Captured {
        hash: BlobHash,
        size: u64,
        mtime_ns: i64,
        stored: Option<BlobInfo>,
    },
    /// The file disappeared after the walk: it is deleted.
    Gone,
    /// The file could not be read.
    Skip(SkipReason),
}

impl Dsnap {
    /// Load a project and check that its folder exists ([`Error::ProjectMissing`] if not).
    pub(crate) fn load_project(&self, id: ProjectId) -> Result<Project> {
        let project = self.db.get_project(id)?;
        match fs::metadata(&project.root) {
            Ok(m) if m.is_dir() => Ok(project),
            Ok(_) => Err(Error::ProjectMissing { root: project.root }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(Error::ProjectMissing { root: project.root })
            }
            Err(e) => Err(Error::io(&project.root, e)),
        }
    }

    /// Walk `project` and hash every file, reusing the latest version's hash for files whose
    /// size and mtime match it.
    pub(crate) fn capture(&self, project: &Project, mode: Mode, hooks: &Hooks) -> Result<Capture> {
        let root = project.root.clone();
        let rules = IgnoreRules::new(&root, &project.settings)?;
        let size_cap = self.db.global_settings()?.size_cap_bytes;

        // Read the version and its entries as a pair; `latest_entry_index` would query the
        // latest version a second time and could see a newer one.
        let latest = self.db.latest_version(project.id)?;
        let latest_entries = match &latest {
            Some(v) => self.db.entries(v.id)?,
            None => Vec::new(),
        };

        let walked = walk(
            &root,
            &rules,
            &WalkOptions {
                size_cap_bytes: size_cap,
                progress: hooks.progress.clone(),
                cancel: hooks.cancel.clone(),
            },
        )?;
        hooks.check_cancel()?;

        let index: HashMap<&RelPath, &Entry> =
            latest_entries.iter().map(|e| (&e.path, e)).collect();
        let mut entries = walked.entries;
        let mut skipped = walked.skipped;
        let mut to_hash = Vec::new();
        for (i, e) in entries.iter_mut().enumerate() {
            if e.kind != EntryKind::File {
                continue;
            }
            match index.get(&e.path) {
                Some(old) if unchanged_stat(old, e) => e.blob = old.blob,
                _ => to_hash.push(i),
            }
        }

        let total = to_hash.len() as u64;
        let done = AtomicU64::new(0);
        let store = (mode == Mode::Store).then_some(&self.store);
        let results: Vec<Result<Hashed>> = to_hash
            .par_iter()
            .map(|&i| {
                hooks.check_cancel()?;
                let e = &entries[i];
                let out = hash_one(store, &e.path.to_path(&root), e);
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % HASH_PROGRESS_EVERY == 0 {
                    hooks.report(Stage::Hash, n, Some(total), Some(&e.path));
                }
                out
            })
            .collect();
        hooks.check_cancel()?;
        if total > 0 {
            hooks.report(Stage::Hash, total, Some(total), None);
        }

        let mut new_blobs: HashMap<BlobHash, BlobInfo> = HashMap::new();
        let mut drop_idx = Vec::new();
        for (&i, res) in to_hash.iter().zip(results) {
            let e = &mut entries[i];
            match res? {
                Hashed::Captured {
                    hash,
                    size,
                    mtime_ns,
                    stored,
                } => {
                    e.blob = Some(hash);
                    e.size = size;
                    e.mtime_ns = mtime_ns;
                    if let Some(info) = stored {
                        new_blobs.insert(info.hash, info);
                    }
                }
                Hashed::Gone => drop_idx.push(i),
                Hashed::Skip(reason) => {
                    skipped.push(SkippedFile {
                        path: e.path.to_string(),
                        reason,
                    });
                    drop_idx.push(i);
                }
            }
        }
        remove_indexes(&mut entries, &drop_idx);
        skipped.sort_by(|a, b| a.path.cmp(&b.path));

        let mut new_blobs: Vec<BlobInfo> = new_blobs.into_values().collect();
        new_blobs.sort_by_key(|b| b.hash);
        Ok(Capture {
            root,
            rules,
            latest,
            latest_entries,
            entries,
            skipped,
            unstable_paths: Vec::new(),
            new_blobs,
        })
    }
}

/// Same kind, size and mtime as the stored entry, which has a hash: the content is assumed
/// unchanged and the file is not read.
fn unchanged_stat(old: &Entry, now: &Entry) -> bool {
    old.kind == EntryKind::File
        && old.blob.is_some()
        && old.size == now.size
        && old.mtime_ns == now.mtime_ns
}

/// Hash (and with a store, store) one file.
///
/// A failure to read the source file becomes [`Hashed::Gone`] (not found) or
/// [`Hashed::Skip`]; any other error (writing the store) is returned and aborts the capture.
fn hash_one(store: Option<&Store>, abs: &Path, walked: &Entry) -> Result<Hashed> {
    let res = match store {
        Some(s) => s
            .put_file(abs)
            .map(|info| (info.hash, info.size, Some(info))),
        None => hash_file(abs).map(|(hash, size)| (hash, size, None)),
    };
    match res {
        Ok((hash, size, stored)) => Ok(Hashed::Captured {
            hash,
            size,
            mtime_ns: walked.mtime_ns,
            stored,
        }),
        Err(e) => source_failure(e, abs),
    }
}

/// Map an error from reading `abs` to an outcome; errors about other paths are returned.
fn source_failure(e: Error, abs: &Path) -> Result<Hashed> {
    match e {
        Error::Io {
            ref path,
            ref source,
        } if path == abs => {
            if source.kind() == io::ErrorKind::NotFound {
                Ok(Hashed::Gone)
            } else {
                Ok(Hashed::Skip(SkipReason::Unreadable {
                    msg: source.to_string(),
                }))
            }
        }
        other => Err(other),
    }
}

/// Remove the items at `idx` (any order, no duplicates).
fn remove_indexes<T>(v: &mut Vec<T>, idx: &[usize]) {
    if idx.is_empty() {
        return;
    }
    let mut drop = vec![false; v.len()];
    for &i in idx {
        if let Some(d) = drop.get_mut(i) {
            *d = true;
        }
    }
    let mut i = 0;
    v.retain(|_| {
        let keep = !drop.get(i).copied().unwrap_or(false);
        i += 1;
        keep
    });
}
