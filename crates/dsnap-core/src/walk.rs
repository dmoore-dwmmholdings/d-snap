//! Folder walk producing unhashed entries. Owner: Chain E (DSNA-8).
//!
//! Never follows symlinks or junctions; records them as [`crate::EntryKind::Symlink`].
//!
//! The walk goes level by level (no recursion, so deep trees cannot overflow the stack) and
//! lists the directories of one level in parallel. Ignored directories are never entered.
//! Output is sorted by path, so it does not depend on thread scheduling.
//!
//! Windows specifics:
//! - NTFS junctions, like symlinks, are name-surrogate reparse points; both are recorded as
//!   links and never entered. Other reparse points (deduplicated or cloud-synced files and
//!   folders) are ordinary files and directories.
//! - Online-only cloud files (OneDrive placeholders) are skipped as unreadable without being
//!   opened, so a walk never downloads them.
//! - Paths longer than `MAX_PATH` work: std adds the `\\?\` prefix to long absolute paths
//!   itself, and stored [`RelPath`]s never carry it.
//! - `readonly` is the `FILE_ATTRIBUTE_READONLY` bit.

use std::collections::HashSet;
use std::fs::{self, DirEntry, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use rayon::prelude::*;

use crate::error::{Error, IoResultExt, Result};
use crate::ignore_rules::IgnoreRules;
use crate::types::{
    CancelToken, Entry, EntryKind, Progress, ProgressEvent, RelPath, SkipReason, SkippedFile, Stage,
};

/// A progress event is sent after this many entries.
pub const PROGRESS_EVERY: u64 = 250;

/// Options for [`walk`].
#[derive(Debug, Clone, Default)]
pub struct WalkOptions {
    /// Files above this size are skipped with [`crate::SkipReason::TooLarge`]; 0 = no cap.
    pub size_cap_bytes: u64,
    /// Optional progress callback ([`crate::Stage::Walk`]).
    pub progress: Option<Progress>,
    /// Optional cancellation token.
    pub cancel: Option<CancelToken>,
}

/// Result of [`walk`].
#[derive(Debug, Clone, Default)]
pub struct WalkOutput {
    /// Files, symlinks and directories, sorted by path, with `blob: None`.
    ///
    /// A [`EntryKind::Dir`] entry means the directory exists and no other entry lies beneath
    /// it. On disk it may still hold ignored, skipped or unlistable paths, so it is not
    /// necessarily empty; other directories are implied by their contents. Restore must
    /// remove a directory only with a non-recursive `remove_dir`, once it is really empty.
    pub entries: Vec<Entry>,
    /// Paths left out (too large, non-UTF-8 name, unreadable), sorted by path.
    pub skipped: Vec<SkippedFile>,
}

/// Walk `root`, applying `rules`.
///
/// `rules` should be built for the same `root`. Symlinks and junctions are recorded, never
/// followed. Entries have `blob: None`; nothing is hashed or opened here.
///
/// Errors: [`Error::Io`] if `root` is missing, not a directory or cannot be listed, or if a
/// `.gitignore` the walk needs exists but cannot be read (walking on would capture, and let a
/// restore delete, paths the user ignored); [`Error::Cancelled`] once `opts.cancel` is set.
/// Other problems below the root become [`WalkOutput::skipped`] entries instead.
pub fn walk(root: &Path, rules: &IgnoreRules, opts: &WalkOptions) -> Result<WalkOutput> {
    let walker = Walker {
        rules,
        opts,
        seen: AtomicU64::new(0),
    };
    walker.check_cancel()?;
    let meta = fs::metadata(root).at(root)?;
    if !meta.is_dir() {
        return Err(Error::io(
            root,
            io::Error::new(
                io::ErrorKind::NotADirectory,
                "project root is not a directory",
            ),
        ));
    }

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut unlisted = HashSet::new();
    let mut skipped = Vec::new();
    let mut level = vec![PendingDir {
        abs: root.to_path_buf(),
        rel: None,
    }];
    while !level.is_empty() {
        let listings: Vec<Result<Listing>> = level.par_iter().map(|d| walker.list(d)).collect();
        let mut next = Vec::new();
        for (dir, listing) in level.iter().zip(listings) {
            let listing = listing?;
            if let (true, Some(rel)) = (listing.unlisted, &dir.rel) {
                unlisted.insert(rel.clone());
            }
            files.extend(listing.files);
            skipped.extend(listing.skipped);
            for (dir, entry) in listing.subdirs {
                next.push(dir);
                dirs.push(entry);
            }
        }
        level = next;
    }
    walker.check_cancel()?;

    let mut entries = files;
    dirs.retain(|d| !unlisted.contains(&d.path));
    entries.extend(empty_dirs(&entries, dirs));
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    if let Some(p) = &opts.progress {
        p.report(&ProgressEvent {
            stage: Stage::Walk,
            done: walker.seen.load(Ordering::Relaxed),
            total: None,
            path: None,
        });
    }
    Ok(WalkOutput { entries, skipped })
}

/// A directory waiting to be listed.
struct PendingDir {
    abs: PathBuf,
    /// `None` for the project root.
    rel: Option<RelPath>,
}

/// What one directory contained.
#[derive(Default)]
struct Listing {
    files: Vec<Entry>,
    skipped: Vec<SkippedFile>,
    /// Subdirectories to list next, with their `Dir` entry (recorded only if empty).
    subdirs: Vec<(PendingDir, Entry)>,
    /// The directory itself could not be listed (it is in `skipped`, so never recorded).
    unlisted: bool,
}

struct Walker<'a> {
    rules: &'a IgnoreRules,
    opts: &'a WalkOptions,
    seen: AtomicU64,
}

impl Walker<'_> {
    fn check_cancel(&self) -> Result<()> {
        match &self.opts.cancel {
            Some(c) => c.check(),
            None => Ok(()),
        }
    }

    fn tick(&self, path: &RelPath) {
        let n = self.seen.fetch_add(1, Ordering::Relaxed) + 1;
        if n % PROGRESS_EVERY != 0 {
            return;
        }
        if let Some(p) = &self.opts.progress {
            p.report(&ProgressEvent {
                stage: Stage::Walk,
                done: n,
                total: None,
                path: Some(path.clone()),
            });
        }
    }

    /// List one directory. Errors for cancellation, an unreadable project root or an
    /// unreadable `.gitignore`.
    fn list(&self, dir: &PendingDir) -> Result<Listing> {
        self.check_cancel()?;
        let mut out = Listing::default();
        let read = match fs::read_dir(&dir.abs) {
            Ok(r) => r,
            Err(e) => match &dir.rel {
                None => return Err(Error::io(&dir.abs, e)),
                Some(rel) => {
                    out.skipped.push(unreadable(rel.as_str(), &e));
                    out.unlisted = true;
                    return Ok(out);
                }
            },
        };
        self.rules.load_dir(dir.rel.as_ref())?;
        for item in read {
            self.check_cancel()?;
            match item {
                Ok(item) => self.visit(dir, &item, &mut out)?,
                Err(e) => {
                    let at = dir.rel.as_ref().map_or("", RelPath::as_str);
                    out.skipped.push(unreadable(at, &e));
                }
            }
        }
        Ok(out)
    }

    /// Classify one directory item into `out`.
    fn visit(&self, dir: &PendingDir, item: &DirEntry, out: &mut Listing) -> Result<()> {
        let name = item.file_name();
        let Some(name) = name.to_str() else {
            out.skipped.push(SkippedFile {
                path: lossy_path(dir.rel.as_ref(), &item.path(), &dir.abs),
                reason: SkipReason::NonUtf8Name,
            });
            return Ok(());
        };
        let joined = match &dir.rel {
            None => RelPath::new(name),
            Some(parent) => parent.join(name),
        };
        let display = || match &dir.rel {
            None => name.to_owned(),
            Some(parent) => format!("{parent}/{name}"),
        };
        let rel = match joined {
            Ok(rel) => rel,
            Err(e) => {
                out.skipped.push(SkippedFile {
                    path: display(),
                    reason: SkipReason::Unreadable { msg: e.to_string() },
                });
                return Ok(());
            }
        };
        // `DirEntry::metadata` does not follow links (lstat; on Windows the directory
        // listing's own data).
        let meta = match item.metadata() {
            Ok(m) => m,
            Err(e) => {
                // Without metadata we cannot tell a dir from a file; an ignored path must
                // still not be reported, so check both ways.
                if !self.rules.is_ignored_entry(&rel, false)?
                    && !self.rules.is_ignored_entry(&rel, true)?
                {
                    out.skipped.push(unreadable(rel.as_str(), &e));
                }
                return Ok(());
            }
        };
        let ft = meta.file_type();
        let is_dir = ft.is_dir() && !ft.is_symlink();
        if self.rules.is_ignored_entry(&rel, is_dir)? {
            return Ok(());
        }
        self.tick(&rel);

        if ft.is_symlink() {
            match link_target(&item.path()) {
                Ok(target) => {
                    // The link's own size differs by OS (0 on Windows); use the target length.
                    let size = target.len() as u64;
                    let kind = EntryKind::Symlink { target };
                    out.files.push(entry(rel, kind, size, &meta));
                }
                Err(msg) => out.skipped.push(SkippedFile {
                    path: rel.into(),
                    reason: SkipReason::Unreadable { msg },
                }),
            }
        } else if is_dir {
            let e = entry(rel.clone(), EntryKind::Dir, 0, &meta);
            out.subdirs.push((
                PendingDir {
                    abs: item.path(),
                    rel: Some(rel),
                },
                e,
            ));
        } else {
            let size = meta.len();
            let cap = self.opts.size_cap_bytes;
            if let Some(msg) = offline_file(&meta) {
                out.skipped.push(SkippedFile {
                    path: rel.into(),
                    reason: SkipReason::Unreadable { msg },
                });
            } else if cap > 0 && size > cap {
                out.skipped.push(SkippedFile {
                    path: rel.into(),
                    reason: SkipReason::TooLarge { size },
                });
            } else {
                out.files.push(entry(rel, EntryKind::File, size, &meta));
            }
        }
        Ok(())
    }
}

/// Windows file attributes that mark a cloud placeholder whose data is not on disk.
#[cfg(windows)]
mod attrs {
    pub const OFFLINE: u32 = 0x0000_1000;
    pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
    pub const DATA_NOT_LOCAL: u32 = OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS;
}

/// Why a file must not be read, if it is an online-only cloud file (OneDrive and other cloud
/// sync providers). Reading it would download ("hydrate") it, so the walk skips it instead.
///
/// The attributes come from the directory listing, so checking them opens nothing.
#[cfg(windows)]
fn offline_file(meta: &Metadata) -> Option<String> {
    use std::os::windows::fs::MetadataExt;
    offline_reason(meta.file_attributes())
}

#[cfg(not(windows))]
fn offline_file(_meta: &Metadata) -> Option<String> {
    None
}

#[cfg(windows)]
fn offline_reason(attributes: u32) -> Option<String> {
    (attributes & attrs::DATA_NOT_LOCAL != 0).then(|| {
        "online-only cloud file (not downloaded); make it available offline to snapshot it"
            .to_owned()
    })
}

fn entry(path: RelPath, kind: EntryKind, size: u64, meta: &Metadata) -> Entry {
    Entry {
        path,
        kind,
        blob: None,
        size,
        mtime_ns: mtime_ns(meta),
        readonly: meta.permissions().readonly(),
    }
}

/// Modification time in nanoseconds since the Unix epoch (negative before it, 0 if unknown).
pub(crate) fn mtime_ns(meta: &Metadata) -> i64 {
    let Ok(t) = meta.modified() else {
        return 0;
    };
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    }
}

/// A link's target as UTF-8 text, exactly as stored in the link.
fn link_target(path: &Path) -> std::result::Result<String, String> {
    let target = fs::read_link(path).map_err(|e| format!("cannot read link: {e}"))?;
    target
        .into_os_string()
        .into_string()
        .map_err(|t| format!("link target is not UTF-8: {}", Path::new(&t).display()))
}

fn unreadable(path: &str, e: &io::Error) -> SkippedFile {
    SkippedFile {
        path: path.to_owned(),
        reason: SkipReason::Unreadable { msg: e.to_string() },
    }
}

/// Best-effort `/`-separated display path for a name that is not UTF-8.
fn lossy_path(parent: Option<&RelPath>, abs: &Path, dir_abs: &Path) -> String {
    let name = abs
        .strip_prefix(dir_abs)
        .unwrap_or(abs)
        .to_string_lossy()
        .into_owned();
    match parent {
        None => name,
        Some(p) => format!("{p}/{name}"),
    }
}

/// The directories with no entry beneath them, decided bottom-up.
fn empty_dirs(files: &[Entry], mut dirs: Vec<Entry>) -> Vec<Entry> {
    let mut non_empty: HashSet<String> = HashSet::new();
    fn mark_ancestors(set: &mut HashSet<String>, path: &str) {
        let mut p = path;
        while let Some((parent, _)) = p.rsplit_once('/') {
            if !set.insert(parent.to_owned()) {
                break;
            }
            p = parent;
        }
    }
    for f in files {
        mark_ancestors(&mut non_empty, f.path.as_str());
    }
    // Deepest first, so a recorded empty dir marks its parents as non-empty.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.path.as_str().matches('/').count()));
    let mut out = Vec::new();
    for d in dirs {
        if !non_empty.contains(d.path.as_str()) {
            mark_ancestors(&mut non_empty, d.path.as_str());
            out.push(d);
        }
    }
    out
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn cloud_placeholder_attributes_are_offline() {
        const NORMAL: u32 = 0x80;
        const REPARSE_POINT: u32 = 0x400;
        const PINNED: u32 = 0x0008_0000;
        assert!(offline_reason(NORMAL).is_none());
        // A hydrated cloud file is still a reparse point, but its data is local.
        assert!(offline_reason(REPARSE_POINT | PINNED).is_none());
        for a in [
            attrs::OFFLINE,
            attrs::RECALL_ON_OPEN,
            REPARSE_POINT | attrs::RECALL_ON_DATA_ACCESS,
        ] {
            let msg = offline_reason(a).unwrap();
            assert!(msg.contains("online-only"), "{msg}");
        }
    }
}
