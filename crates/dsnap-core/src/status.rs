//! Working-tree status versus the latest version, and the read-side comparison facade behind
//! [`Dsnap::changes`] and [`Dsnap::file_diff`]. Owner: Chain K (DSNA-14).
//!
//! Nothing here writes to the database or the blob store. The working tree is captured with
//! the snapshot fast path: only files whose size or mtime differs from the latest version
//! are read and hashed.
//!
//! Working-tree comparisons leave out paths that are now ignored on both sides (DSNA-27): a
//! file captured earlier and ignored since does not show as deleted. Skipped paths (over the
//! size cap, locked, unreadable) keep their previous entry, so they do not show as deleted
//! either.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::diff::content::{ContentClass, DiffSide, build_file_diff, classify};
use crate::diff::entries::{EntryDiffOptions, diff_entries};
use crate::diff::lines::line_counts_bytes;
use crate::error::{Error, IoResultExt, Result};
use crate::facade::Dsnap;
use crate::ignore_rules::IgnoreRules;
use crate::snapshot::capture::{Hooks, Mode};
use crate::types::{
    ChangeStatus, DiffOptions, Entry, EntryKind, FileChange, FileDiff, ProjectId, RelPath,
    VersionId, VersionRef,
};

/// [`Dsnap::changes`] computes line counts only for text files up to this size (both sides).
pub const MAX_LINE_COUNT_BYTES: u64 = 1024 * 1024;

/// Two entry lists to compare, plus where to read the new side's files from.
struct Sides {
    old: Vec<Entry>,
    new: Vec<Entry>,
    /// Project root when the new side is the working tree (its bytes are read from disk).
    working_root: Option<PathBuf>,
}

impl Dsnap {
    /// Changes in the folder since the latest version (the "Unsaved changes" row and badge).
    ///
    /// Without a version, every path in the folder is added. Line counts are not filled; use
    /// [`Dsnap::changes`] with [`VersionRef::WorkingTree`] for those. Writes nothing.
    pub fn status(&self, project: ProjectId) -> Result<Vec<FileChange>> {
        let sides = self.sides(project, None, VersionRef::WorkingTree)?;
        Ok(diff_entries(
            &sides.old,
            &sides.new,
            EntryDiffOptions::default(),
        ))
    }

    /// Current folder entries with hashes (reusing stored hashes when size and mtime match).
    /// Stores nothing.
    ///
    /// A file that cannot be read keeps `blob: None` (it compares by size and mtime). Paths
    /// the walk skipped (over the size cap, unreadable) keep the latest version's entry.
    pub fn working_entries(&self, project: ProjectId) -> Result<Vec<Entry>> {
        let proj = self.load_project(project)?;
        Ok(self
            .capture(&proj, Mode::HashOnly, &Hooks::default())?
            .entries)
    }

    /// [`Dsnap::changes`]: see its docs.
    pub(crate) fn changes_impl(
        &self,
        project: ProjectId,
        from: Option<VersionId>,
        to: VersionRef,
    ) -> Result<Vec<FileChange>> {
        let sides = self.sides(project, from, to)?;
        let mut changes = diff_entries(&sides.old, &sides.new, EntryDiffOptions::default());
        let root = sides.working_root.as_deref();
        changes
            .par_iter_mut()
            .for_each(|c| self.fill_line_counts(c, root));
        Ok(changes)
    }

    /// [`Dsnap::file_diff`]: see its docs.
    pub(crate) fn file_diff_impl(
        &self,
        project: ProjectId,
        from: Option<VersionId>,
        to: VersionRef,
        path: &RelPath,
        opts: &DiffOptions,
    ) -> Result<FileDiff> {
        let sides = self.sides(project, from, to)?;
        let find = |list: &[Entry], p: &RelPath| list.iter().position(|e| e.path == *p);
        let new_i = find(&sides.new, path);
        let mut old_i = find(&sides.old, path);
        if new_i.is_some() && old_i.is_none() {
            // A path that is new on this side may be a rename: load the old side by the
            // rename's `from` (DSNA-89: a case-only rename can also change the content).
            let changes = diff_entries(&sides.old, &sides.new, EntryDiffOptions::default());
            let renamed_from = changes.iter().find_map(|c| match &c.status {
                ChangeStatus::Renamed { from } if c.path == *path => Some(from),
                _ => None,
            });
            if let Some(from) = renamed_from {
                old_i = find(&sides.old, from);
            }
        }
        let old = old_i.and_then(|i| sides.old.get(i));
        let new = new_i.and_then(|i| sides.new.get(i));
        if old.is_none() && new.is_none() {
            return Err(Error::NotFound(format!("{path} on either side")));
        }
        let root = sides.working_root.as_deref();
        let old_bytes = old.map(|e| self.side_bytes(e, None)).transpose()?;
        let new_bytes = new.map(|e| self.side_bytes(e, root)).transpose()?;
        let o: Option<DiffSide<'_>> = old.zip(old_bytes.as_deref());
        let n: Option<DiffSide<'_>> = new.zip(new_bytes.as_deref());
        Ok(build_file_diff(path, o, n, opts))
    }

    /// Resolve `from` and `to` into entry lists for `project`.
    ///
    /// `from = None` is the version before `to` (for [`VersionRef::WorkingTree`], the latest
    /// version), or nothing when there is none. Both versions must belong to `project`.
    fn sides(&self, project: ProjectId, from: Option<VersionId>, to: VersionRef) -> Result<Sides> {
        match to {
            VersionRef::Version(id) => {
                let new = self.project_entries(project, id)?;
                let from = match from {
                    Some(f) => Some(f),
                    None => self.db.previous_version(id)?.map(|v| v.id),
                };
                let old = match from {
                    Some(f) => self.project_entries(project, f)?,
                    None => Vec::new(),
                };
                Ok(Sides {
                    old,
                    new,
                    working_root: None,
                })
            }
            VersionRef::WorkingTree => {
                let proj = self.load_project(project)?;
                let cap = self.capture(&proj, Mode::HashOnly, &Hooks::default())?;
                let old = match from {
                    Some(id) => self.project_entries(project, id)?,
                    None => cap.latest_entries,
                };
                Ok(Sides {
                    old: in_scope(old, &cap.rules)?,
                    new: cap.entries,
                    working_root: Some(cap.root),
                })
            }
        }
    }

    /// Entries of version `id`, which must belong to `project`.
    fn project_entries(&self, project: ProjectId, id: VersionId) -> Result<Vec<Entry>> {
        let v = self.db.get_version(id)?;
        if v.project_id != project {
            return Err(Error::NotFound(format!(
                "version {id} in project {project}"
            )));
        }
        self.db.entries(id)
    }

    /// Bytes of one side of a file diff: the stored blob, or the file on disk for a
    /// working-tree entry (`root`). Links and directories have no bytes.
    fn side_bytes(&self, e: &Entry, root: Option<&Path>) -> Result<Vec<u8>> {
        if e.kind != EntryKind::File {
            return Ok(Vec::new());
        }
        if let Some(root) = root {
            let abs = e.path.to_path(root);
            return fs::read(&abs).at(abs);
        }
        match &e.blob {
            Some(h) => self.store.get(h),
            None => Err(Error::Corrupt(format!(
                "stored file {} has no hash",
                e.path
            ))),
        }
    }

    /// Fill a change's line counts when every present side is a small text file. Problems
    /// reading either side leave the counts `None`.
    fn fill_line_counts(&self, c: &mut FileChange, working_root: Option<&Path>) {
        let small_file = |e: &Option<Entry>| match e {
            None => true,
            Some(e) => e.kind == EntryKind::File && e.size <= MAX_LINE_COUNT_BYTES,
        };
        if !small_file(&c.old) || !small_file(&c.new) {
            return;
        }
        if let (Some(o), Some(n)) = (&c.old, &c.new) {
            if o.blob.is_some() && o.blob == n.blob {
                c.lines_added = Some(0);
                c.lines_removed = Some(0);
                return;
            }
        }
        let load = |e: &Option<Entry>, root: Option<&Path>| -> Option<Vec<u8>> {
            match e {
                None => Some(Vec::new()),
                Some(e) => {
                    let bytes = self.side_bytes(e, root).ok()?;
                    let text = classify(&e.path, &bytes) == ContentClass::Text;
                    let small = bytes.len() as u64 <= MAX_LINE_COUNT_BYTES;
                    (text && small).then_some(bytes)
                }
            }
        };
        let (Some(old), Some(new)) = (load(&c.old, None), load(&c.new, working_root)) else {
            return;
        };
        let (added, removed) = line_counts_bytes(&old, &new, &DiffOptions::default());
        c.lines_added = Some(added);
        c.lines_removed = Some(removed);
    }
}

/// Drop entries that `rules` now ignore (the old side of a working-tree comparison).
fn in_scope(entries: Vec<Entry>, rules: &IgnoreRules) -> Result<Vec<Entry>> {
    let mut out = Vec::with_capacity(entries.len());
    // Most entries share a parent: decide each directory once.
    let mut dir_ignored: HashMap<RelPath, bool> = HashMap::new();
    for e in entries {
        let parent_ignored = match e.path.parent() {
            None => false,
            Some(dir) => match dir_ignored.get(&dir) {
                Some(&v) => v,
                None => {
                    let v = rules.try_is_ignored(&dir, true)?;
                    dir_ignored.insert(dir, v);
                    v
                }
            },
        };
        if parent_ignored || rules.try_is_ignored(&e.path, e.kind == EntryKind::Dir)? {
            continue;
        }
        out.push(e);
    }
    Ok(out)
}
