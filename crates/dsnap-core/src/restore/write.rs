//! Writing one entry into the project folder (DSNA-56).
//!
//! A file is never written in place. Its blob is streamed to a temp file in the target's
//! own directory (same volume), fsynced, given the recorded mtime, and then moved over the
//! target with one atomic rename (`MoveFileExW(MOVEFILE_REPLACE_EXISTING)` on Windows). The
//! target therefore always holds either its old or its new content. A temp file is removed
//! on every error path.
//!
//! The workspace forbids `unsafe`, so `ReplaceFileW` (which keeps the replaced file's ACL)
//! is not used; a restored file gets the default security of its directory.

// Used by the restore engine (DSNA-57); until then only tests call it.
#![cfg_attr(not(test), allow(dead_code))]

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use filetime::FileTime;

use crate::error::{Error, IoResultExt, Result};
use crate::snapshot::capture::is_locked;
use crate::store::Store;
use crate::types::{Entry, EntryKind};

/// Prefix of restore temp files, so a stray one is recognisable.
pub(crate) const TEMP_PREFIX: &str = ".dsnap-restore-";

/// Write `entry` (relative to `root`) from `store`, replacing what is there.
///
/// - File: atomic replace (see the module docs), then mtime and read-only as recorded. An
///   existing read-only target is made writable first.
/// - Directory: created with its parents.
/// - Symlink: recreated (a directory link if the target is a directory; on Windows a
///   junction when symlinks need privileges and the target is an absolute directory).
///
/// The caller decides what may be replaced; this never removes a directory.
///
/// Errors: [`Error::Locked`] if another program holds the target open, [`Error::Io`] for
/// other filesystem errors, [`Error::Corrupt`] for a file entry without a blob, and store
/// errors if the blob cannot be read.
pub(crate) fn write_entry(root: &Path, entry: &Entry, store: &Store) -> Result<()> {
    let target = entry.path.to_path(root);
    match &entry.kind {
        EntryKind::Dir => match fs::symlink_metadata(&target) {
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(Error::InvalidInput(format!(
                "{} exists and is not a directory",
                target.display()
            ))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&target).at(&target)
            }
            Err(e) => Err(Error::io(&target, e)),
        },
        EntryKind::File => write_file(&target, entry, store),
        EntryKind::Symlink { target: link } => write_symlink(&target, link),
    }
}

/// Removes the temp file unless the write completed.
struct TempGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn temp_path(dir: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{TEMP_PREFIX}{}-{n}.tmp", std::process::id()))
}

fn write_file(target: &Path, entry: &Entry, store: &Store) -> Result<()> {
    let hash = entry
        .blob
        .ok_or_else(|| Error::Corrupt(format!("file entry {} has no blob", entry.path)))?;
    let dir = target
        .parent()
        .ok_or_else(|| Error::InvalidInput(format!("{} has no parent", target.display())))?;
    let existing = match fs::symlink_metadata(target) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(Error::io(target, e)),
    };
    if existing.as_ref().is_some_and(fs::Metadata::is_dir) {
        return Err(Error::InvalidInput(format!(
            "{} is a directory; refusing to replace it with a file",
            target.display()
        )));
    }
    fs::create_dir_all(dir).at(dir)?;

    let temp = temp_path(dir);
    let mut guard = TempGuard {
        path: temp.clone(),
        armed: true,
    };
    {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&temp)
            .at(&temp)?;
        store.copy_to(&hash, &mut file)?;
        file.sync_all().at(&temp)?;
    }
    filetime::set_file_mtime(&temp, mtime(entry.mtime_ns)).at(&temp)?;

    let was_readonly = existing
        .as_ref()
        .is_some_and(|m| m.is_file() && m.permissions().readonly());
    if was_readonly {
        set_readonly(target, false)?;
    }
    if let Err(e) = fs::rename(&temp, target) {
        if was_readonly {
            // Leave the untouched target as it was.
            let _ = set_readonly(target, true);
        }
        return Err(locked_or_io(target, e));
    }
    guard.armed = false;
    sync_dir(dir);
    if entry.readonly {
        set_readonly(target, true)?;
    }
    Ok(())
}

/// Make the rename durable. Best effort: the content itself is already fsynced.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir; // NTFS journals the rename; directories cannot be opened for fsync here.
}

/// Map a failed replace of `target` to [`Error::Locked`] when another program holds it.
///
/// Replacing a file that is open without delete sharing fails with "access denied" on
/// Windows rather than a sharing violation, so that case is confirmed by opening the file.
fn locked_or_io(target: &Path, e: io::Error) -> Error {
    if is_locked(&e) {
        return Error::Locked {
            path: target.to_path_buf(),
        };
    }
    if e.kind() == io::ErrorKind::PermissionDenied {
        if let Err(open) = File::open(target) {
            if is_locked(&open) {
                return Error::Locked {
                    path: target.to_path_buf(),
                };
            }
        }
    }
    Error::io(target, e)
}

fn set_readonly(path: &Path, readonly: bool) -> Result<()> {
    let mut perms = fs::metadata(path).at(path)?.permissions();
    if perms.readonly() != readonly {
        #[allow(clippy::permissions_set_readonly_false)] // intended: clear before replacing
        perms.set_readonly(readonly);
        fs::set_permissions(path, perms).at(path)?;
    }
    Ok(())
}

/// Recorded nanoseconds since the epoch as a [`FileTime`] (negative before 1970).
fn mtime(ns: i64) -> FileTime {
    let secs = ns.div_euclid(1_000_000_000);
    let nanos = u32::try_from(ns.rem_euclid(1_000_000_000)).unwrap_or(0);
    FileTime::from_unix_time(secs, nanos)
}

fn write_symlink(path: &Path, link: &str) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::InvalidInput(format!("{} has no parent", path.display())))?;
    fs::create_dir_all(dir).at(dir)?;
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            if fs::read_link(path).ok().as_deref() == Some(Path::new(link)) {
                return Ok(());
            }
            remove_link(path)?;
        }
        Ok(m) if m.is_dir() => {
            return Err(Error::InvalidInput(format!(
                "{} is a directory; refusing to replace it with a link",
                path.display()
            )));
        }
        Ok(_) => fs::remove_file(path).map_err(|e| locked_or_io(path, e))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io(path, e)),
    }
    create_link(path, link)
}

/// Remove a symlink or junction without following it.
fn remove_link(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        // A directory symlink or junction on Windows is removed like a directory.
        Err(_) if cfg!(windows) => fs::remove_dir(path).at(path),
        Err(e) => Err(Error::io(path, e)),
    }
}

#[cfg(unix)]
fn create_link(path: &Path, link: &str) -> Result<()> {
    std::os::unix::fs::symlink(link, path).at(path)
}

#[cfg(windows)]
fn create_link(path: &Path, link: &str) -> Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

    let resolved = path
        .parent()
        .map_or_else(|| PathBuf::from(link), |p| p.join(link));
    let is_dir = fs::metadata(&resolved).is_ok_and(|m| m.is_dir());
    let made = if is_dir {
        symlink_dir(link, path)
    } else {
        symlink_file(link, path)
    };
    match made {
        Ok(()) => Ok(()),
        // Without Developer Mode or admin rights, an absolute directory target can still be
        // restored as a junction (the walker records junctions as links).
        Err(e)
            if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
                && is_dir
                && Path::new(link).is_absolute() =>
        {
            make_junction(path, link)
        }
        Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => Err(Error::io(
            path,
            io::Error::other(
                "creating a symbolic link needs Developer Mode or administrator rights",
            ),
        )),
        Err(e) => Err(Error::io(path, e)),
    }
}

#[cfg(windows)]
fn make_junction(path: &Path, target: &str) -> Result<()> {
    let target = target.strip_prefix(r"\\?\").unwrap_or(target);
    let out = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(path)
        .arg(target)
        .output()
        .at(path)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::io(
            path,
            io::Error::other(format!(
                "mklink /J failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BlobHash, RelPath};

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        store: Store,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir(&root).unwrap();
        let store = Store::open(tmp.path().join("objects")).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            store,
        }
    }

    fn file(f: &Fixture, path: &str, bytes: &[u8], mtime_ns: i64, readonly: bool) -> Entry {
        let info = f.store.put(bytes).unwrap();
        Entry {
            path: RelPath::new(path).unwrap(),
            kind: EntryKind::File,
            blob: Some(info.hash),
            size: info.size,
            mtime_ns,
            readonly,
        }
    }

    fn temps(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .count()
    }

    const T: i64 = 1_700_000_000_123_456_700;

    #[test]
    fn overwrites_an_existing_file() {
        let f = fixture();
        fs::write(f.root.join("a.txt"), "new local edit").unwrap();
        let e = file(&f, "a.txt", b"old content", T, false);
        write_entry(&f.root, &e, &f.store).unwrap();
        assert_eq!(fs::read(f.root.join("a.txt")).unwrap(), b"old content");
        assert_eq!(temps(&f.root), 0);
    }

    #[test]
    fn creates_a_file_in_new_directories() {
        let f = fixture();
        let e = file(&f, "x/y/z.txt", b"deep", T, false);
        write_entry(&f.root, &e, &f.store).unwrap();
        assert_eq!(fs::read(f.root.join("x/y/z.txt")).unwrap(), b"deep");
    }

    #[test]
    fn restores_mtime_and_readonly() {
        let f = fixture();
        let e = file(&f, "r.txt", b"ro", T, true);
        write_entry(&f.root, &e, &f.store).unwrap();
        let m = fs::metadata(f.root.join("r.txt")).unwrap();
        assert!(m.permissions().readonly());
        let got = FileTime::from_last_modification_time(&m);
        // NTFS keeps 100 ns ticks, so compare to the tick.
        let want = mtime(T);
        assert_eq!(got.unix_seconds(), want.unix_seconds());
        assert_eq!(got.nanoseconds() / 100, want.nanoseconds() / 100);
    }

    #[test]
    fn replaces_a_readonly_target() {
        let f = fixture();
        let p = f.root.join("ro.txt");
        fs::write(&p, "local").unwrap();
        set_readonly(&p, true).unwrap();
        let e = file(&f, "ro.txt", b"restored", T, false);
        write_entry(&f.root, &e, &f.store).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"restored");
        assert!(!fs::metadata(&p).unwrap().permissions().readonly());
    }

    #[test]
    fn a_missing_blob_leaves_target_and_no_temp() {
        let f = fixture();
        fs::write(f.root.join("a.txt"), "keep me").unwrap();
        let mut e = file(&f, "a.txt", b"whatever", T, false);
        e.blob = Some(BlobHash::of(b"never stored"));
        assert!(write_entry(&f.root, &e, &f.store).is_err());
        assert_eq!(fs::read(f.root.join("a.txt")).unwrap(), b"keep me");
        assert_eq!(temps(&f.root), 0);
    }

    #[test]
    fn refuses_to_replace_a_directory_with_a_file() {
        let f = fixture();
        fs::create_dir(f.root.join("d")).unwrap();
        fs::write(f.root.join("d/child"), "x").unwrap();
        let e = file(&f, "d", b"file", T, false);
        assert!(matches!(
            write_entry(&f.root, &e, &f.store),
            Err(Error::InvalidInput(_))
        ));
        assert!(f.root.join("d/child").exists());
        assert_eq!(temps(&f.root), 0);
    }

    #[test]
    fn creates_directories() {
        let f = fixture();
        let e = Entry {
            path: RelPath::new("empty/inner").unwrap(),
            kind: EntryKind::Dir,
            blob: None,
            size: 0,
            mtime_ns: 0,
            readonly: false,
        };
        write_entry(&f.root, &e, &f.store).unwrap();
        assert!(f.root.join("empty/inner").is_dir());
        write_entry(&f.root, &e, &f.store).unwrap(); // idempotent
    }

    #[test]
    fn recreates_a_symlink_when_supported() {
        let f = fixture();
        fs::write(f.root.join("target.txt"), "t").unwrap();
        let e = Entry {
            path: RelPath::new("link").unwrap(),
            kind: EntryKind::Symlink {
                target: "target.txt".into(),
            },
            blob: None,
            size: 10,
            mtime_ns: 0,
            readonly: false,
        };
        match write_entry(&f.root, &e, &f.store) {
            Ok(()) => {
                assert_eq!(
                    fs::read_link(f.root.join("link")).unwrap(),
                    Path::new("target.txt")
                );
                assert_eq!(fs::read(f.root.join("link")).unwrap(), b"t");
            }
            // Windows without Developer Mode: a clear error naming the path.
            Err(err) => assert!(cfg!(windows) && err.to_string().contains("link"), "{err}"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_target_is_reported_and_untouched() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = fixture();
        let p = f.root.join("locked.txt");
        fs::write(&p, "in use").unwrap();
        let _held = File::options().read(true).share_mode(0).open(&p).unwrap();
        let e = file(&f, "locked.txt", b"restored", T, false);
        let err = write_entry(&f.root, &e, &f.store).unwrap_err();
        assert!(matches!(err, Error::Locked { .. }), "{err:?}");
        drop(_held);
        assert_eq!(fs::read(&p).unwrap(), b"in use");
        assert_eq!(temps(&f.root), 0);
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_to_an_absolute_directory_is_restored() {
        let f = fixture();
        let target_dir = f.root.join("real");
        fs::create_dir(&target_dir).unwrap();
        fs::write(target_dir.join("x"), "x").unwrap();
        let link = target_dir.to_string_lossy().into_owned();
        // make_junction directly: it is the fallback path when symlinks need privileges.
        make_junction(&f.root.join("j"), &link).unwrap();
        assert_eq!(fs::read(f.root.join("j/x")).unwrap(), b"x");
    }
}
