//! The capture engine shared by [`Dsnap::snapshot`] and the working-tree status
//! ([`Dsnap::status`], [`Dsnap::working_entries`]).
//!
//! 1. Walk the project with one [`IgnoreRules`] and the global size cap.
//! 2. Fast path: a file whose kind, size and mtime match its entry in the latest version
//!    reuses that entry's hash without being opened.
//! 3. Every other file is hashed (and, in [`Mode::Store`], stored) in parallel on the rayon
//!    pool, which is bounded by the CPU count.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rayon::prelude::*;

use crate::diff::entries::EntryDiffOptions;
use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::ignore_rules::IgnoreRules;
use crate::store::hash_file;
use crate::types::{
    BlobHash, BlobInfo, CancelToken, Entry, EntryKind, Progress, ProgressEvent, Project, ProjectId,
    RelPath, SkipReason, SkippedFile, Stage, Version,
};
use crate::walk::{WalkOptions, mtime_ns, walk};

/// A hash progress event is sent after this many files.
const HASH_PROGRESS_EVERY: u64 = 32;

/// Whether hashed files are also written to the blob store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Hash and store (snapshot).
    Store,
    /// Hash only; nothing is written (status).
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

/// What one read of a file produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Read {
    /// Hash of the bytes read.
    pub(crate) hash: BlobHash,
    /// Number of bytes read.
    pub(crate) size: u64,
    /// The stored blob ([`Mode::Store`] only).
    pub(crate) stored: Option<BlobInfo>,
}

/// Outcome of capturing one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Hashed {
    /// Captured content.
    Captured {
        /// What was read.
        read: Read,
        /// Modification time from the stat taken before that read.
        mtime_ns: i64,
        /// The file kept changing while it was read.
        unstable: bool,
    },
    /// The file disappeared after the walk: it is deleted.
    Gone,
    /// The file could not be read.
    Skip(SkipReason),
}

/// Pauses between attempts to read a locked file: about 500 ms in total.
pub(crate) const LOCK_RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
    Duration::from_millis(150),
    Duration::from_millis(200),
];

/// How [`capture_file`] behaves.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CaptureRules<'a> {
    /// Stat again after reading and read once more if the file changed meanwhile.
    pub(crate) recheck: bool,
    /// Pauses between attempts when the file is locked; empty = no retry.
    pub(crate) lock_delays: &'a [Duration],
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
    ///
    /// In [`Mode::Store`] (snapshot) each file is re-checked after reading (see
    /// [`capture_file`]), locked files are retried, and a file that still cannot be read is
    /// skipped. In [`Mode::HashOnly`] (status) each file is read once and a file that cannot
    /// be read keeps its walk entry with `blob: None`, so it compares by size and mtime.
    ///
    /// Skipped paths are unknown, not deleted: the latest version's entries at or below a
    /// skipped path that the walk did not see are carried forward (see [`carry_forward`]).
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

        let store = &self.store;
        let read_store = |abs: &Path| {
            store.put_file(abs).map(|info| Read {
                hash: info.hash,
                size: info.size,
                stored: Some(info),
            })
        };
        let read_hash = |abs: &Path| {
            hash_file(abs).map(|(hash, size)| Read {
                hash,
                size,
                stored: None,
            })
        };
        let (read, rules_for): (&ReadFn<'_>, CaptureRules<'_>) = match mode {
            Mode::Store => (
                &read_store,
                CaptureRules {
                    recheck: true,
                    lock_delays: &LOCK_RETRY_DELAYS,
                },
            ),
            Mode::HashOnly => (
                &read_hash,
                CaptureRules {
                    recheck: false,
                    lock_delays: &[],
                },
            ),
        };

        let total = to_hash.len() as u64;
        let done = AtomicU64::new(0);
        let results: Vec<Result<Hashed>> = to_hash
            .par_iter()
            .map(|&i| {
                hooks.check_cancel()?;
                let e = &entries[i];
                let out = capture_file(read, &e.path.to_path(&root), e, rules_for);
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
        let mut unstable_paths = Vec::new();
        let mut drop_idx = Vec::new();
        for (&i, res) in to_hash.iter().zip(results) {
            let e = &mut entries[i];
            match res? {
                Hashed::Captured {
                    read,
                    mtime_ns,
                    unstable,
                } => {
                    e.blob = Some(read.hash);
                    e.size = read.size;
                    e.mtime_ns = mtime_ns;
                    if let Some(info) = read.stored {
                        new_blobs.insert(info.hash, info);
                    }
                    if unstable {
                        unstable_paths.push(e.path.clone());
                    }
                }
                Hashed::Gone => drop_idx.push(i),
                Hashed::Skip(_) if mode == Mode::HashOnly => {}
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
        unstable_paths.sort();
        carry_forward(
            &mut entries,
            &skipped,
            &latest_entries,
            EntryDiffOptions::default().case_insensitive,
        );

        let mut new_blobs: Vec<BlobInfo> = new_blobs.into_values().collect();
        new_blobs.sort_by_key(|b| b.hash);
        Ok(Capture {
            root,
            rules,
            latest,
            latest_entries,
            entries,
            skipped,
            unstable_paths,
            new_blobs,
        })
    }
}

/// Reads one file: stores it ([`Mode::Store`]) or only hashes it.
pub(crate) type ReadFn<'a> = dyn Fn(&Path) -> Result<Read> + Sync + 'a;

/// Same kind, size and mtime as the stored entry, which has a hash: the content is assumed
/// unchanged and the file is not read.
fn unchanged_stat(old: &Entry, now: &Entry) -> bool {
    old.kind == EntryKind::File
        && old.blob.is_some()
        && old.size == now.size
        && old.mtime_ns == now.mtime_ns
}

/// Capture one file whose walk entry is `walked`.
///
/// With `rules.recheck` (DSNA-1 "File changes while hashing"): after the read, stat the file
/// again. If its size or mtime differs from the stat before the read (the walk's), or the
/// bytes read differ from the stat size, read it once more. If it changed during that read
/// too, keep the second capture and mark it unstable. The recorded mtime is the one from the
/// stat before the kept read, so a later change is never hidden by the fast path. If the
/// second read fails, the first capture is kept and marked unstable.
///
/// A locked file is retried after each pause in `rules.lock_delays`, then skipped as
/// [`SkipReason::Locked`]. A file that is gone is [`Hashed::Gone`]; other read errors are
/// [`SkipReason::Unreadable`]. Errors not about `abs` (writing the store) are returned.
pub(crate) fn capture_file(
    read: &ReadFn<'_>,
    abs: &Path,
    walked: &Entry,
    rules: CaptureRules<'_>,
) -> Result<Hashed> {
    let captured = |read: Read, mtime_ns: i64, unstable: bool| Hashed::Captured {
        read,
        mtime_ns,
        unstable,
    };
    let first = match read_retrying(read, abs, rules.lock_delays)? {
        Ok(r) => r,
        Err(other) => return Ok(other),
    };
    if !rules.recheck {
        return Ok(captured(first, walked.mtime_ns, false));
    }
    let before = Some((walked.size, walked.mtime_ns));
    let after = stat(abs);
    if after == before && first.size == walked.size {
        return Ok(captured(first, walked.mtime_ns, false));
    }

    // Changed while it was read: read once more.
    let before = after;
    match read_retrying(read, abs, rules.lock_delays)? {
        Ok(second) => {
            let after = stat(abs);
            let stable = before.is_some()
                && after == before
                && before.map(|(size, _)| size) == Some(second.size);
            let mtime_ns = before.map_or(walked.mtime_ns, |(_, m)| m);
            Ok(captured(second, mtime_ns, !stable))
        }
        Err(Hashed::Gone) => Ok(Hashed::Gone),
        Err(_) => Ok(captured(first, walked.mtime_ns, true)),
    }
}

/// Read `abs`, retrying while it is locked. `Err` carries a non-capture outcome.
fn read_retrying(
    read: &ReadFn<'_>,
    abs: &Path,
    delays: &[Duration],
) -> Result<std::result::Result<Read, Hashed>> {
    let mut attempt = 0;
    loop {
        let e = match read(abs) {
            Ok(r) => return Ok(Ok(r)),
            Err(e) => e,
        };
        match e {
            Error::Io {
                ref path,
                ref source,
            } if path == abs => {
                if source.kind() == io::ErrorKind::NotFound {
                    return Ok(Err(Hashed::Gone));
                }
                if !is_locked(source) {
                    return Ok(Err(Hashed::Skip(SkipReason::Unreadable {
                        msg: source.to_string(),
                    })));
                }
                match delays.get(attempt) {
                    Some(d) => {
                        std::thread::sleep(*d);
                        attempt += 1;
                    }
                    None => return Ok(Err(Hashed::Skip(SkipReason::Locked))),
                }
            }
            other => return Err(other),
        }
    }
}

/// A sharing or lock violation: another process holds the file open.
#[cfg(windows)]
fn is_locked(e: &io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    matches!(
        e.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
    )
}

#[cfg(not(windows))]
fn is_locked(_e: &io::Error) -> bool {
    false
}

/// Size and mtime of a regular file (`None` if it is gone or no longer a regular file).
fn stat(abs: &Path) -> Option<(u64, i64)> {
    let m = fs::symlink_metadata(abs).ok()?;
    m.file_type().is_file().then(|| (m.len(), mtime_ns(&m)))
}

/// Keep the previous entries of skipped paths (DSNA-2: skipped paths are unknown, not deleted).
///
/// For every skipped path (a file, or a directory that could not be listed; `""` covers the
/// whole tree), each latest entry at or below it that `entries` does not hold is added back.
/// With `case_insensitive`, paths are compared case-folded, so a skipped `Foo.txt` keeps
/// `foo.txt`, and an entry whose folded path is present is not added twice.
///
/// A carried entry is not added when an ancestor of it is now a file or symlink, or (for a
/// directory entry) when other entries now lie beneath it. A recorded empty directory that is
/// an ancestor of a carried entry is dropped, because it is no longer empty.
pub(crate) fn carry_forward(
    entries: &mut Vec<Entry>,
    skipped: &[SkippedFile],
    latest: &[Entry],
    case_insensitive: bool,
) {
    if skipped.is_empty() || latest.is_empty() {
        return;
    }
    let key = |s: &str| {
        if case_insensitive {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let skipped_keys: HashSet<String> = skipped.iter().map(|s| key(&s.path)).collect();
    let present: HashMap<String, bool> = entries
        .iter()
        .map(|e| (key(e.path.as_str()), e.kind == EntryKind::Dir))
        .collect();
    let mut present_dirs: HashSet<String> = HashSet::new();
    for k in present.keys() {
        for a in ancestors(k) {
            if !present_dirs.insert(a.to_owned()) {
                break;
            }
        }
    }

    let mut carried = Vec::new();
    let mut drop_dirs: HashSet<String> = HashSet::new();
    for old in latest {
        let k = key(old.path.as_str());
        if present.contains_key(&k) {
            continue;
        }
        let under_skipped = skipped_keys.contains("")
            || skipped_keys.contains(&k)
            || ancestors(&k).any(|a| skipped_keys.contains(a));
        if !under_skipped {
            continue;
        }
        if present_dirs.contains(&k) {
            continue;
        }
        if ancestors(&k).any(|a| present.get(a) == Some(&false)) {
            continue;
        }
        drop_dirs.extend(
            ancestors(&k)
                .filter(|a| present.get(*a) == Some(&true))
                .map(str::to_owned),
        );
        carried.push(old.clone());
    }
    if carried.is_empty() {
        return;
    }
    entries.retain(|e| !(e.kind == EntryKind::Dir && drop_dirs.contains(&key(e.path.as_str()))));
    entries.extend(carried);
    entries.sort_by(|a, b| a.path.cmp(&b.path));
}

/// Proper ancestors of a `/`-separated path, nearest first.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').rev().map(move |(i, _)| &path[..i])
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::time::SystemTime;

    use super::*;

    fn rp(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    const RECHECK: CaptureRules<'static> = CaptureRules {
        recheck: true,
        lock_delays: &[],
    };

    /// A file plus its walk entry, as the walker would have produced it.
    fn walked(dir: &Path, name: &str, bytes: &[u8]) -> (PathBuf, Entry) {
        let abs = dir.join(name);
        fs::write(&abs, bytes).unwrap();
        let m = fs::symlink_metadata(&abs).unwrap();
        let e = Entry {
            path: rp(name),
            kind: EntryKind::File,
            blob: None,
            size: m.len(),
            mtime_ns: mtime_ns(&m),
            readonly: false,
        };
        (abs, e)
    }

    /// Rewrite `abs` with `bytes` and move its mtime forward by `secs`.
    fn rewrite(abs: &Path, bytes: &[u8], secs: u64) {
        fs::write(abs, bytes).unwrap();
        let t = SystemTime::now() + Duration::from_secs(secs);
        filetime::set_file_mtime(abs, filetime::FileTime::from_system_time(t)).unwrap();
    }

    fn hash_read(abs: &Path) -> Result<Read> {
        hash_file(abs).map(|(hash, size)| Read {
            hash,
            size,
            stored: None,
        })
    }

    fn captured(h: Hashed) -> (Read, i64, bool) {
        match h {
            Hashed::Captured {
                read,
                mtime_ns,
                unstable,
            } => (read, mtime_ns, unstable),
            other => panic!("expected a capture, got {other:?}"),
        }
    }

    #[test]
    fn unchanged_file_is_read_once() {
        let dir = tempfile::tempdir().unwrap();
        let (abs, e) = walked(dir.path(), "a", b"same");
        let calls = AtomicUsize::new(0);
        let read = |p: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            hash_read(p)
        };
        let (r, mtime, unstable) = captured(capture_file(&read, &abs, &e, RECHECK).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(r.hash, BlobHash::of(b"same"));
        assert_eq!(mtime, e.mtime_ns);
        assert!(!unstable);
    }

    #[test]
    fn change_during_first_read_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let (abs, e) = walked(dir.path(), "a", b"v1");
        let calls = AtomicUsize::new(0);
        let read = |p: &Path| {
            let r = hash_read(p);
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                rewrite(p, b"v2 longer", 3);
            }
            r
        };
        let (r, mtime, unstable) = captured(capture_file(&read, &abs, &e, RECHECK).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(r.hash, BlobHash::of(b"v2 longer"));
        assert_eq!(r.size, 9);
        assert_eq!(mtime, stat(&abs).unwrap().1);
        assert!(!unstable);
    }

    #[test]
    fn file_changing_during_both_reads_keeps_second_capture_and_is_unstable() {
        let dir = tempfile::tempdir().unwrap();
        let (abs, e) = walked(dir.path(), "a", b"v0");
        let calls = AtomicUsize::new(0);
        let read = |p: &Path| {
            let r = hash_read(p);
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let text = format!("v{n} {}", "x".repeat(n));
            rewrite(p, text.as_bytes(), 2 * n as u64);
            r
        };
        let (r, mtime, unstable) = captured(capture_file(&read, &abs, &e, RECHECK).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(r.hash, BlobHash::of(b"v1 x"), "second capture is kept");
        assert!(unstable);
        // The mtime recorded is the one seen before the second read, not the current one, so
        // the next snapshot re-hashes the file.
        assert_ne!(mtime, stat(&abs).unwrap().1);
    }

    #[test]
    fn same_size_and_mtime_but_short_read_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let (abs, e) = walked(dir.path(), "a", b"abc");
        let calls = AtomicUsize::new(0);
        let read = |p: &Path| {
            let mut r = hash_read(p)?;
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                r.size -= 1; // as if the file was truncated and restored mid-read
            }
            Ok(r)
        };
        let (r, _, unstable) = captured(capture_file(&read, &abs, &e, RECHECK).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(r.size, 3);
        assert!(!unstable);
    }

    #[test]
    fn file_deleted_during_read_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (abs, e) = walked(dir.path(), "a", b"bye");
        let read = |p: &Path| {
            let r = hash_read(p);
            let _ = fs::remove_file(p);
            r
        };
        assert_eq!(
            capture_file(&read, &abs, &e, RECHECK).unwrap(),
            Hashed::Gone
        );
        assert_eq!(
            capture_file(&read, &abs, &e, RECHECK).unwrap(),
            Hashed::Gone
        );
    }

    #[test]
    fn second_read_failure_keeps_first_capture_as_unstable() {
        let dir = tempfile::tempdir().unwrap();
        let (abs, e) = walked(dir.path(), "a", b"first");
        let calls = AtomicUsize::new(0);
        let read = |p: &Path| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let r = hash_read(p);
                rewrite(p, b"changed", 3);
                r
            } else {
                Err(Error::io(
                    p,
                    io::Error::from(io::ErrorKind::PermissionDenied),
                ))
            }
        };
        let (r, mtime, unstable) = captured(capture_file(&read, &abs, &e, RECHECK).unwrap());
        assert_eq!(r.hash, BlobHash::of(b"first"));
        assert_eq!(mtime, e.mtime_ns);
        assert!(unstable);
    }

    #[test]
    fn read_errors_map_to_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let (abs, e) = walked(dir.path(), "a", b"x");
        let denied = |p: &Path| -> Result<Read> {
            Err(Error::io(
                p,
                io::Error::from(io::ErrorKind::PermissionDenied),
            ))
        };
        assert!(matches!(
            capture_file(&denied, &abs, &e, RECHECK).unwrap(),
            Hashed::Skip(SkipReason::Unreadable { .. })
        ));
        // An error about another path (the store) aborts.
        let store_full = |_: &Path| -> Result<Read> {
            Err(Error::io("objects/.tmp-1", io::Error::other("disk full")))
        };
        assert!(matches!(
            capture_file(&store_full, &abs, &e, RECHECK),
            Err(Error::Io { .. })
        ));
        // No recheck: one read, walk mtime.
        let once = CaptureRules {
            recheck: false,
            lock_delays: &[],
        };
        let (_, mtime, unstable) = captured(capture_file(&hash_read, &abs, &e, once).unwrap());
        assert_eq!(mtime, e.mtime_ns);
        assert!(!unstable);
    }

    #[cfg(windows)]
    #[test]
    fn locked_file_is_retried_then_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (abs, e) = walked(dir.path(), "a", b"x");
        let delays = [Duration::from_millis(1); 3];
        let rules = CaptureRules {
            recheck: true,
            lock_delays: &delays,
        };
        let calls = AtomicUsize::new(0);
        let always_locked = |p: &Path| -> Result<Read> {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(Error::io(p, io::Error::from_raw_os_error(32)))
        };
        assert_eq!(
            capture_file(&always_locked, &abs, &e, rules).unwrap(),
            Hashed::Skip(SkipReason::Locked)
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "first try plus one per delay"
        );

        let calls = AtomicUsize::new(0);
        let locked_twice = |p: &Path| -> Result<Read> {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(Error::io(p, io::Error::from_raw_os_error(33)))
            } else {
                hash_read(p)
            }
        };
        let (r, _, unstable) = captured(capture_file(&locked_twice, &abs, &e, rules).unwrap());
        assert_eq!(r.hash, BlobHash::of(b"x"));
        assert!(!unstable);
    }

    #[test]
    fn lock_retry_budget_is_about_half_a_second() {
        let total: Duration = LOCK_RETRY_DELAYS.iter().sum();
        assert_eq!(total, Duration::from_millis(500));
    }

    fn file(path: &str, content: &str) -> Entry {
        Entry {
            path: rp(path),
            kind: EntryKind::File,
            blob: Some(BlobHash::of(content.as_bytes())),
            size: content.len() as u64,
            mtime_ns: 1,
            readonly: false,
        }
    }

    fn dir(path: &str) -> Entry {
        Entry {
            path: rp(path),
            kind: EntryKind::Dir,
            blob: None,
            size: 0,
            mtime_ns: 1,
            readonly: false,
        }
    }

    fn skip(path: &str) -> SkippedFile {
        SkippedFile {
            path: path.into(),
            reason: SkipReason::Locked,
        }
    }

    fn paths(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn skipped_file_keeps_its_previous_entry() {
        let latest = [file("a", "1"), file("b", "2"), file("gone", "3")];
        let mut now = vec![file("a", "1")];
        carry_forward(&mut now, &[skip("b")], &latest, false);
        assert_eq!(paths(&now), ["a", "b"]);
        assert_eq!(now[1], latest[1]);
    }

    #[test]
    fn unlisted_directory_keeps_everything_below_it() {
        let latest = [
            file("d/x", "1"),
            file("d/sub/y", "2"),
            file("dd/z", "3"),
            file("e", "4"),
        ];
        let mut now = vec![file("d/new", "5")];
        carry_forward(&mut now, &[skip("d")], &latest, false);
        assert_eq!(paths(&now), ["d/new", "d/sub/y", "d/x"]);

        let mut now = vec![];
        carry_forward(&mut now, &[skip("")], &latest, false);
        assert_eq!(
            now.len(),
            latest.len(),
            "a root listing error keeps everything"
        );
    }

    #[test]
    fn carry_forward_respects_case_folding() {
        let latest = [file("Foo.txt", "1")];
        let mut now = vec![];
        carry_forward(&mut now, &[skip("foo.txt")], &latest, true);
        assert_eq!(paths(&now), ["Foo.txt"]);

        let mut now = vec![];
        carry_forward(&mut now, &[skip("foo.txt")], &latest, false);
        assert!(now.is_empty());

        // Present under another case: not added twice.
        let mut now = vec![file("FOO.txt", "2")];
        carry_forward(&mut now, &[skip("Foo.txt")], &latest, true);
        assert_eq!(paths(&now), ["FOO.txt"]);
    }

    #[test]
    fn carried_entry_replaces_empty_dir_ancestor_and_yields_to_files() {
        // `d` now looks empty because its only file is locked.
        let latest = [file("d/locked", "1")];
        let mut now = vec![dir("d")];
        carry_forward(&mut now, &[skip("d/locked")], &latest, false);
        assert_eq!(paths(&now), ["d/locked"]);

        // `d` is now a file: the old `d/x` cannot come back.
        let latest = [file("d/x", "1")];
        let mut now = vec![file("d", "now a file")];
        carry_forward(&mut now, &[skip("d")], &latest, false);
        assert_eq!(paths(&now), ["d"]);

        // An old empty dir is not carried when entries now lie beneath it, nor an old file
        // where a directory now is.
        let latest = [dir("e"), file("f", "old")];
        let mut now = vec![file("e/new", "1"), file("f/inner", "2")];
        carry_forward(&mut now, &[skip("e"), skip("f")], &latest, false);
        assert_eq!(paths(&now), ["e/new", "f/inner"]);
    }
}
