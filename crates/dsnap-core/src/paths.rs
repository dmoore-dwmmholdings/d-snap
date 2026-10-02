//! Location of D-Snap's data directory (database and object store).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, IoResultExt, Result};

/// Environment variable that overrides the data directory.
pub const HOME_ENV: &str = "DSNAP_HOME";
/// Database file name inside the home directory.
pub const DB_FILE: &str = "dsnap.db";
/// Object store directory name inside the home directory.
pub const OBJECTS_DIR: &str = "objects";

/// Resolved D-Snap data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// Use `root` as the home directory as-is.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve the home: `explicit` if given, else `DSNAP_HOME`, else the platform default
    /// (`%LOCALAPPDATA%\D-Snap\data` on Windows: the per-user installer puts the program in
    /// `%LOCALAPPDATA%\D-Snap`, and data must not share a folder with program files).
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        Self::resolve_with(explicit, |k| std::env::var_os(k))
    }

    /// [`Home::resolve`] with an injectable environment lookup, for tests.
    pub fn resolve_with(
        explicit: Option<PathBuf>,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        if let Some(p) = explicit {
            return Ok(Self::at(p));
        }
        let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if let Some(p) = var(HOME_ENV) {
            return Ok(Self::at(p));
        }
        platform_default(&var).map(Self::at).ok_or_else(|| {
            Error::InvalidInput(format!("cannot find a data directory; set {HOME_ENV}"))
        })
    }

    /// Home directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of the SQLite database.
    pub fn db_path(&self) -> PathBuf {
        self.root.join(DB_FILE)
    }

    /// Directory holding blobs.
    pub fn objects_dir(&self) -> PathBuf {
        self.root.join(OBJECTS_DIR)
    }

    /// Create the home and objects directories if missing.
    pub fn ensure(&self) -> Result<()> {
        let objects = self.objects_dir();
        std::fs::create_dir_all(&objects).at(objects)
    }
}

#[cfg(windows)]
fn platform_default(var: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    var("LOCALAPPDATA").map(|p| p.join("D-Snap").join("data"))
}

#[cfg(target_os = "macos")]
fn platform_default(var: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    var("HOME").map(|p| p.join("Library/Application Support/D-Snap"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_default(var: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    var("XDG_DATA_HOME")
        .map(|p| p.join("d-snap"))
        .or_else(|| var("HOME").map(|p| p.join(".local/share/d-snap")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn explicit_wins_over_env() {
        let h = Home::resolve_with(Some("x".into()), env_of(&[(HOME_ENV, "y")])).unwrap();
        assert_eq!(h.root(), Path::new("x"));
    }

    #[test]
    fn env_var_overrides_default() {
        let h = Home::resolve_with(
            None,
            env_of(&[(HOME_ENV, "envhome"), ("LOCALAPPDATA", "la"), ("HOME", "h")]),
        )
        .unwrap();
        assert_eq!(h.root(), Path::new("envhome"));
        assert_eq!(h.db_path(), Path::new("envhome").join("dsnap.db"));
        assert_eq!(h.objects_dir(), Path::new("envhome").join("objects"));
    }

    #[test]
    fn empty_env_var_is_ignored() {
        let h = Home::resolve_with(
            None,
            env_of(&[(HOME_ENV, ""), ("LOCALAPPDATA", "la"), ("HOME", "h")]),
        )
        .unwrap();
        assert_ne!(h.root(), Path::new(""));
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_is_localappdata() {
        let h = Home::resolve_with(
            None,
            env_of(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local")]),
        )
        .unwrap();
        assert_eq!(h.root(), Path::new(r"C:\Users\u\AppData\Local\D-Snap\data"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_default_uses_xdg_then_home() {
        let h = Home::resolve_with(None, env_of(&[("XDG_DATA_HOME", "/x")])).unwrap();
        assert_eq!(h.root(), Path::new("/x/d-snap"));
        let h = Home::resolve_with(None, env_of(&[("HOME", "/h")])).unwrap();
        assert_eq!(h.root(), Path::new("/h/.local/share/d-snap"));
    }

    #[test]
    fn no_env_at_all_is_an_error() {
        assert!(Home::resolve_with(None, |_| None).is_err());
    }

    #[test]
    fn ensure_creates_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Home::at(tmp.path().join("home"));
        h.ensure().unwrap();
        assert!(h.objects_dir().is_dir());
        h.ensure().unwrap();
    }
}
