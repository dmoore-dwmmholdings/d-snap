//! Project management. Owner: Chain L (DSNA-15).
//!
//! Project roots are stored normalized (see [`normalize_root`]) so one folder cannot be
//! tracked twice under different spellings (DSNA-97). Overlap checks also compare roots
//! case-insensitively on Windows, which covers rows stored before normalization.

use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, IoResultExt, Result};
use crate::facade::Dsnap;
use crate::types::{Project, ProjectId, ProjectSettings};

/// Longest accepted project or version name, in characters.
pub const MAX_NAME_CHARS: usize = 200;

impl Dsnap {
    /// Track the folder at `root`; `name` defaults to the folder name.
    ///
    /// `root` must be an existing directory. It is stored normalized ([`normalize_root`]).
    /// Fails with [`Error::InvalidInput`] if the folder is already tracked, lies inside or
    /// around a tracked folder, or overlaps the D-Snap data directory.
    pub fn add_project(&self, root: &Path, name: Option<&str>) -> Result<Project> {
        let root = normalize_root(root)?;
        let name = match name {
            Some(n) => valid_name(n, "project name")?,
            None => default_name(&root),
        };
        self.check_root_free(&root, None)?;
        let id = self
            .db
            .insert_project(&name, &root, &ProjectSettings::default())?;
        self.project(id)
    }

    /// All projects, with `missing` set for folders that no longer exist.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut projects = self.db.list_projects()?;
        for p in &mut projects {
            p.missing = is_missing(&p.root);
        }
        Ok(projects)
    }

    /// One project.
    pub fn project(&self, id: ProjectId) -> Result<Project> {
        let mut p = self.db.get_project(id)?;
        p.missing = is_missing(&p.root);
        Ok(p)
    }

    /// Rename a project (trimmed, non-empty, at most [`MAX_NAME_CHARS`] characters).
    pub fn rename_project(&self, id: ProjectId, name: &str) -> Result<()> {
        let name = valid_name(name, "project name")?;
        self.db.set_project_name(id, &name)
    }

    /// Stop tracking a project.
    ///
    /// Always deletes the project and all its versions from the index: versions without a
    /// project cannot be reached. `delete_snapshots` controls the blob cleanup: `true` prunes
    /// the blobs no other version uses now; `false` leaves them for the next retention run or
    /// [`Dsnap::prune_blobs`]. Blob files that cannot be deleted are left for the next prune
    /// and are not an error here.
    pub fn remove_project(&self, id: ProjectId, delete_snapshots: bool) -> Result<()> {
        self.db.delete_project(id)?;
        if delete_snapshots {
            self.prune_unreferenced_all()?;
        }
        Ok(())
    }

    /// Point a project at a moved folder. `new_root` is validated like [`Dsnap::add_project`].
    pub fn relocate_project(&self, id: ProjectId, new_root: &Path) -> Result<Project> {
        // NotFound for an unknown id comes before any path error.
        self.db.get_project(id)?;
        let root = normalize_root(new_root)?;
        self.check_root_free(&root, Some(id))?;
        self.db.set_project_root(id, &root)?;
        self.project(id)
    }

    /// Fail unless `root` (normalized) is disjoint from the D-Snap home and from every
    /// tracked root other than `except`'s.
    fn check_root_free(&self, root: &Path, except: Option<ProjectId>) -> Result<()> {
        let key = root_key(root);
        let home = self.home.root();
        let home_key = root_key(&std::fs::canonicalize(home).unwrap_or_else(|_| home.into()));
        if key.starts_with(&home_key) || home_key.starts_with(&key) {
            return Err(Error::InvalidInput(format!(
                "{} overlaps the D-Snap data folder {}",
                root.display(),
                home.display()
            )));
        }
        for p in self.db.list_projects()? {
            if Some(p.id) == except {
                continue;
            }
            let other = root_key(&p.root);
            let why = if key == other {
                "is already tracked by"
            } else if key.starts_with(&other) {
                "is inside the folder of"
            } else if other.starts_with(&key) {
                "contains the folder of"
            } else {
                continue;
            };
            return Err(Error::InvalidInput(format!(
                "{} {why} project {:?} ({})",
                root.display(),
                p.name,
                p.root.display()
            )));
        }
        Ok(())
    }
}

/// Trim `name` and check it is non-empty and at most [`MAX_NAME_CHARS`] characters.
pub(crate) fn valid_name(name: &str, what: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::InvalidInput(format!("{what} is empty")));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(Error::InvalidInput(format!(
            "{what} is longer than {MAX_NAME_CHARS} characters"
        )));
    }
    Ok(name.to_owned())
}

fn default_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

/// The folder at `root` no longer exists or is no longer a folder. A permission error means
/// it exists, so it does not count as missing.
fn is_missing(root: &Path) -> bool {
    match std::fs::metadata(root) {
        Ok(m) => !m.is_dir(),
        Err(e) => e.kind() != io::ErrorKind::PermissionDenied,
    }
}

/// Canonical form of a project root: absolute, symlinks and `..` resolved, on-disk case, no
/// trailing separator. On Windows the `\\?\` prefix is removed when the plain path means the
/// same thing, so the path stays displayable.
///
/// Fails with [`Error::InvalidInput`] if the path is empty, does not exist, is not a
/// directory or is not valid UTF-8.
pub fn normalize_root(root: &Path) -> Result<PathBuf> {
    if root.as_os_str().is_empty() {
        return Err(Error::InvalidInput("project folder path is empty".into()));
    }
    let canon = match std::fs::canonicalize(root) {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(Error::InvalidInput(format!(
                "folder does not exist: {}",
                root.display()
            )));
        }
        Err(e) => return Err(e).at(root),
    };
    if !std::fs::metadata(&canon).at(&canon)?.is_dir() {
        return Err(Error::InvalidInput(format!(
            "not a folder: {}",
            root.display()
        )));
    }
    let canon = strip_verbatim(canon);
    if canon.to_str().is_none() {
        return Err(Error::InvalidInput(format!(
            "project folder path is not valid UTF-8: {}",
            canon.display()
        )));
    }
    Ok(canon)
}

/// Comparison key for a root: its components with any verbatim prefix and trailing
/// separator dropped, case-folded on Windows.
fn root_key(root: &Path) -> PathBuf {
    let s = root.to_string_lossy();
    let plain = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        s.into_owned()
    };
    let plain = if cfg!(windows) {
        plain.to_lowercase()
    } else {
        plain
    };
    Path::new(&plain).components().collect()
}

/// Remove the verbatim prefix when the plain path is equivalent (Windows only).
#[cfg(windows)]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    let plain = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
            return p;
        }
        rest.to_owned()
    } else {
        return p;
    };
    // MAX_PATH: a longer plain path does not work everywhere.
    if plain.len() >= 260 || !plain_components_safe(&plain) {
        return p;
    }
    PathBuf::from(plain)
}

#[cfg(not(windows))]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    p
}

/// Every name in `plain` means the same with and without the verbatim prefix: no trailing
/// dot or space, no device names, no `.`/`..`.
#[cfg(windows)]
fn plain_components_safe(plain: &str) -> bool {
    use std::path::Component;
    const DEVICES: &[&str] = &["con", "prn", "aux", "nul", "conin$", "conout$"];
    Path::new(plain).components().all(|c| match c {
        Component::Normal(n) => {
            let Some(n) = n.to_str() else { return false };
            if n.ends_with('.') || n.ends_with(' ') {
                return false;
            }
            let stem = n
                .split('.')
                .next()
                .unwrap_or(n)
                .trim_end()
                .to_ascii_lowercase();
            let numbered = |prefix: &str| {
                stem.strip_prefix(prefix).is_some_and(|d| {
                    (d.len() == 1 && d.as_bytes()[0].is_ascii_digit())
                        || matches!(d, "¹" | "²" | "³")
                })
            };
            !(DEVICES.contains(&stem.as_str()) || numbered("com") || numbered("lpt"))
        }
        Component::Prefix(_) | Component::RootDir => true,
        Component::CurDir | Component::ParentDir => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(valid_name("  a  ", "n").unwrap(), "a");
        assert!(matches!(
            valid_name(" \t ", "n"),
            Err(Error::InvalidInput(_))
        ));
        assert!(valid_name(&"é".repeat(MAX_NAME_CHARS), "n").is_ok());
        assert!(valid_name(&"é".repeat(MAX_NAME_CHARS + 1), "n").is_err());
    }

    #[test]
    fn keys_compare_whole_components() {
        let k = |s: &str| root_key(Path::new(s));
        assert!(k("/a/b/c").starts_with(k("/a/b")));
        assert_eq!(k("/a/b/"), k("/a/b"));
        assert!(!k("/a/bc").starts_with(k("/a/b")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_keys_fold_case_and_verbatim() {
        let k = |s: &str| root_key(Path::new(s));
        assert_eq!(k(r"C:\Proj"), k(r"c:\proj\"));
        assert_eq!(k(r"C:\Proj"), k(r"\\?\C:\Proj"));
        assert_eq!(k(r"\\srv\share\x"), k(r"\\?\UNC\srv\share\x"));
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_prefix_kept_when_plain_form_differs() {
        let s = |p: &str| strip_verbatim(PathBuf::from(p));
        assert_eq!(s(r"\\?\C:\a\b"), PathBuf::from(r"C:\a\b"));
        assert_eq!(s(r"\\?\UNC\srv\share\a"), PathBuf::from(r"\\srv\share\a"));
        assert_eq!(s(r"\\?\C:\a\con"), PathBuf::from(r"\\?\C:\a\con"));
        assert_eq!(s(r"\\?\C:\a\COM1.txt"), PathBuf::from(r"\\?\C:\a\COM1.txt"));
        assert_eq!(s(r"\\?\C:\a\b."), PathBuf::from(r"\\?\C:\a\b."));
        assert_eq!(s(r"\\?\Volume{x}\a"), PathBuf::from(r"\\?\Volume{x}\a"));
        let long = format!(r"\\?\C:\{}", "x".repeat(300));
        assert_eq!(s(&long), PathBuf::from(&long));
    }
}
