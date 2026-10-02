//! Per-project operation lock shared by every D-Snap process (DSNA-64, DSNA-68).
//!
//! The app, `dsnap snap`/`restore` and Claude Code hooks all write the same projects. A
//! snapshot or restore holds `<home>/locks/project-<id>.lock` (never a file in the project
//! folder) so a hook snapshot and a GUI restore never interleave. The lock is a file made
//! with `create_new` and removed on drop; one left by a crashed process is taken over once
//! it is older than [`STALE_AFTER`]. (`std::fs::File::lock` needs Rust 1.89, above the
//! workspace MSRV.)

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{Error, IoResultExt, Result};
use crate::facade::Dsnap;
use crate::types::{CancelToken, ProjectId};

/// A lock file older than this is left from a crashed process and is taken over.
pub const STALE_AFTER: Duration = Duration::from_secs(600);
/// Pause between attempts while waiting.
const POLL: Duration = Duration::from_millis(50);

/// A held project lock; released on drop.
#[derive(Debug)]
pub struct ProjectLock {
    path: PathBuf,
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl ProjectLock {
    /// The lock file of `project` under the data directory `home`.
    pub fn path_for(home: &Path, project: ProjectId) -> PathBuf {
        home.join("locks")
            .join(format!("project-{}.lock", project.0))
    }

    /// Take the lock now, or `None` if another operation holds it.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).at(dir)?;
        }
        loop {
            match File::options().write(true).create_new(true).open(path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(Some(Self {
                        path: path.to_path_buf(),
                    }));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if !is_stale(path) {
                        return Ok(None);
                    }
                    // Taken over: retry once it is gone (or another process wins).
                    let _ = fs::remove_file(path);
                }
                // Windows reports a lock file being deleted as access denied.
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
                Err(e) => return Err(Error::io(path, e)),
            }
        }
    }

    /// Take the lock, waiting up to `timeout`. `on_wait` runs once if the lock is busy at
    /// first (to tell the user). `None` after the timeout; [`Error::Cancelled`] if
    /// `cancel` fires while waiting.
    pub fn acquire(
        path: &Path,
        timeout: Duration,
        cancel: Option<&CancelToken>,
        on_wait: impl FnOnce(),
    ) -> Result<Option<Self>> {
        let deadline = Instant::now() + timeout;
        let mut on_wait = Some(on_wait);
        loop {
            if let Some(l) = Self::try_acquire(path)? {
                return Ok(Some(l));
            }
            if let Some(f) = on_wait.take() {
                f();
            }
            if let Some(c) = cancel {
                c.check()?;
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(POLL);
        }
    }
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > STALE_AFTER)
}

impl Dsnap {
    /// The lock file of `project` in this data directory ([`ProjectLock`]).
    pub fn project_lock_path(&self, project: ProjectId) -> PathBuf {
        ProjectLock::path_for(self.home.root(), project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_waits_until_release() {
        let tmp = tempfile::tempdir().unwrap();
        let path = ProjectLock::path_for(tmp.path(), ProjectId(7));
        let first = ProjectLock::try_acquire(&path).unwrap().unwrap();
        assert!(ProjectLock::try_acquire(&path).unwrap().is_none());

        let mut waited = false;
        let none = ProjectLock::acquire(&path, Duration::from_millis(120), None, || waited = true);
        assert!(none.unwrap().is_none());
        assert!(waited);

        let p2 = path.clone();
        let t = std::thread::spawn(move || {
            ProjectLock::acquire(&p2, Duration::from_secs(5), None, || {}).unwrap()
        });
        std::thread::sleep(Duration::from_millis(100));
        drop(first);
        assert!(t.join().unwrap().is_some());
        assert!(!path.exists(), "released on drop");
    }

    #[test]
    fn waiting_can_be_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let path = ProjectLock::path_for(tmp.path(), ProjectId(1));
        let _held = ProjectLock::try_acquire(&path).unwrap().unwrap();
        let c = CancelToken::new();
        c.cancel();
        let r = ProjectLock::acquire(&path, Duration::from_secs(5), Some(&c), || {});
        assert!(matches!(r, Err(Error::Cancelled)));
    }

    #[test]
    fn a_stale_lock_is_taken_over() {
        let tmp = tempfile::tempdir().unwrap();
        let path = ProjectLock::path_for(tmp.path(), ProjectId(2));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "crashed").unwrap();
        let old = SystemTime::now() - STALE_AFTER - Duration::from_secs(1);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(old)).unwrap();
        assert!(ProjectLock::try_acquire(&path).unwrap().is_some());
    }
}
