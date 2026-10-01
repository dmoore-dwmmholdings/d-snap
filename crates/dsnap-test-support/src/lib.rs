//! Shared test helpers for D-Snap crates. Use only as a dev-dependency.
//!
//! - [`TestHome`]: a throwaway data directory passed to `Dsnap::open(Some(..))`. Never touches
//!   the real `DSNAP_HOME` or process environment, so tests can run in parallel.
//! - [`FixtureProject`]: builder for a project folder on disk.
//! - [`tree_bytes`], [`tree_dirs`], [`generate_tree`], [`touch_n`]: comparison and perf helpers.
//!
//! From `dsnap-core`, use this crate in integration tests (`crates/dsnap-core/tests/`), not in
//! `#[cfg(test)]` unit tests: in unit tests the core types this crate returns are a separate
//! copy of the crate and do not unify with `crate::` types.

// Test-only code: failing loudly is the right behavior here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dsnap_core::{Dsnap, RelPath};
use tempfile::TempDir;

/// Temporary D-Snap data directory, removed on drop.
#[derive(Debug)]
pub struct TestHome {
    dir: TempDir,
}

impl TestHome {
    /// Create an empty temporary home.
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("dsnap-home-")
            .tempdir()
            .expect("create temp DSNAP_HOME");
        Self { dir }
    }

    /// Path of the home directory.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Open a [`Dsnap`] on this home.
    pub fn open(&self) -> Dsnap {
        Dsnap::open(Some(self.path().to_path_buf())).expect("open Dsnap on test home")
    }
}

impl Default for TestHome {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
enum Op {
    File(RelPath, Vec<u8>),
    Dir(RelPath),
    Symlink(RelPath, String),
    Readonly(RelPath),
    Mtime(RelPath, SystemTime),
}

/// Builder for a project folder. Paths are `/`-separated and project-relative.
///
/// [`FixtureProject::build`] applies files and dirs first (in call order), then symlinks, then
/// mtimes, then read-only flags, so the order of builder calls does not matter across those.
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct FixtureProject {
    ops: Vec<Op>,
}

fn rel(path: &str) -> RelPath {
    RelPath::new(path).unwrap_or_else(|e| panic!("bad fixture path {path:?}: {e}"))
}

impl FixtureProject {
    /// Empty project.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a file (parent directories are created).
    pub fn file(mut self, path: &str, bytes: impl AsRef<[u8]>) -> Self {
        self.ops.push(Op::File(rel(path), bytes.as_ref().to_vec()));
        self
    }

    /// Create a directory (may stay empty).
    pub fn dir(mut self, path: &str) -> Self {
        self.ops.push(Op::Dir(rel(path)));
        self
    }

    /// Create a symlink at `path` pointing to `target` (stored verbatim).
    ///
    /// If the OS refuses (Windows without symlink privilege) the link is skipped with a
    /// message on stderr and recorded in [`Fixture::skipped_symlinks`]. Use
    /// [`symlinks_supported`] to skip a whole test instead.
    pub fn symlink(mut self, path: &str, target: &str) -> Self {
        self.ops.push(Op::Symlink(rel(path), target.to_owned()));
        self
    }

    /// Mark a file or directory read-only.
    pub fn readonly(mut self, path: &str) -> Self {
        self.ops.push(Op::Readonly(rel(path)));
        self
    }

    /// Set a file's modification time.
    pub fn mtime(mut self, path: &str, t: SystemTime) -> Self {
        self.ops.push(Op::Mtime(rel(path), t));
        self
    }

    /// Write a root `.gitignore` with one pattern per line.
    pub fn gitignore(self, lines: &[&str]) -> Self {
        let mut text = lines.join("\n");
        text.push('\n');
        self.file(".gitignore", text)
    }

    /// Create the project in a new temporary directory.
    pub fn build(self) -> Fixture {
        let dir = tempfile::Builder::new()
            .prefix("dsnap-proj-")
            .tempdir()
            .expect("create fixture dir");
        let root = dir.path().to_path_buf();
        let skipped = self.apply(&root);
        Fixture {
            root,
            dir: Some(dir),
            skipped_symlinks: skipped,
        }
    }

    /// Create the project inside an existing directory (not removed on drop).
    pub fn build_at(self, root: &Path) -> Fixture {
        fs::create_dir_all(root).expect("create fixture root");
        let skipped = self.apply(root);
        Fixture {
            root: root.to_path_buf(),
            dir: None,
            skipped_symlinks: skipped,
        }
    }

    fn apply(mut self, root: &Path) -> Vec<RelPath> {
        // Phases: files/dirs, then symlinks (so a Windows link sees whether its target is a
        // dir), then mtimes, then read-only (setting times on a read-only file fails on
        // Windows). The sort is stable, so call order holds within a phase.
        self.ops.sort_by_key(|op| match op {
            Op::File(..) | Op::Dir(_) => 0,
            Op::Symlink(..) => 1,
            Op::Mtime(..) => 2,
            Op::Readonly(_) => 3,
        });
        let mut skipped = Vec::new();
        for op in self.ops {
            match op {
                Op::File(p, bytes) => {
                    let abs = p.to_path(root);
                    mkdir_parent(&abs);
                    fs::write(&abs, bytes).unwrap_or_else(|e| panic!("write {p}: {e}"));
                }
                Op::Dir(p) => {
                    fs::create_dir_all(p.to_path(root)).unwrap_or_else(|e| panic!("mkdir {p}: {e}"))
                }
                Op::Symlink(p, target) => {
                    let abs = p.to_path(root);
                    mkdir_parent(&abs);
                    if let Err(e) = make_symlink(&abs, &target) {
                        eprintln!("dsnap-test-support: skipping symlink {p} -> {target}: {e}");
                        skipped.push(p);
                    }
                }
                Op::Mtime(p, t) => filetime::set_file_mtime(
                    p.to_path(root),
                    filetime::FileTime::from_system_time(t),
                )
                .unwrap_or_else(|e| panic!("set mtime {p}: {e}")),
                Op::Readonly(p) => set_readonly(&p.to_path(root), true),
            }
        }
        skipped
    }
}

fn mkdir_parent(abs: &Path) {
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir {}: {e}", parent.display()));
    }
}

fn set_readonly(abs: &Path, readonly: bool) {
    let mut perms = fs::symlink_metadata(abs)
        .unwrap_or_else(|e| panic!("stat {}: {e}", abs.display()))
        .permissions();
    #[allow(clippy::permissions_set_readonly_false)] // only used to undo our own readonly()
    perms.set_readonly(readonly);
    fs::set_permissions(abs, perms).unwrap_or_else(|e| panic!("chmod {}: {e}", abs.display()));
}

#[cfg(unix)]
fn make_symlink(link: &Path, target: &str) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_symlink(link: &Path, target: &str) -> std::io::Result<()> {
    let resolved = link.parent().map(|p| p.join(target));
    if resolved.is_some_and(|p| p.is_dir()) {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Whether this process can create symlinks (false on Windows without the privilege).
pub fn symlinks_supported() -> bool {
    let Ok(dir) = tempfile::tempdir() else {
        return false;
    };
    make_symlink(&dir.path().join("probe"), "target").is_ok()
}

/// A built project folder. Removes its temporary directory on drop (clearing read-only flags
/// first so Windows can delete it).
#[derive(Debug)]
pub struct Fixture {
    root: PathBuf,
    dir: Option<TempDir>,
    skipped_symlinks: Vec<RelPath>,
}

impl Fixture {
    /// Project root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path of a project-relative path.
    pub fn path(&self, rel_path: &str) -> PathBuf {
        rel(rel_path).to_path(&self.root)
    }

    /// Symlinks that could not be created.
    pub fn skipped_symlinks(&self) -> &[RelPath] {
        &self.skipped_symlinks
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.dir.is_some() {
            clear_readonly(&self.root);
        }
    }
}

fn clear_readonly(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let p = entry.path();
        if ft.is_dir() {
            clear_readonly(&p);
        }
        let readonly =
            !ft.is_symlink() && fs::metadata(&p).is_ok_and(|md| md.permissions().readonly());
        if readonly {
            set_readonly(&p, false);
        }
    }
}

fn walk_tree(dir: &Path, f: &mut dyn FnMut(&Path, fs::FileType)) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let ft = entry.file_type().expect("file type");
        let p = entry.path();
        f(&p, ft);
        if ft.is_dir() {
            walk_tree(&p, f);
        }
    }
}

fn rel_of(root: &Path, abs: &Path) -> RelPath {
    RelPath::from_abs(root, abs).unwrap_or_else(|e| panic!("{e}"))
}

/// Every file under `root` with its bytes. Symlinks (never followed) map to `b"symlink:"`
/// followed by their target. Directories are not included; see [`tree_dirs`].
pub fn tree_bytes(root: &Path) -> BTreeMap<RelPath, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk_tree(root, &mut |p, ft| {
        if ft.is_symlink() {
            let target = fs::read_link(p).expect("read_link");
            let mut v = b"symlink:".to_vec();
            v.extend_from_slice(target.to_string_lossy().as_bytes());
            out.insert(rel_of(root, p), v);
        } else if ft.is_file() {
            out.insert(rel_of(root, p), fs::read(p).expect("read file"));
        }
    });
    out
}

/// Every directory under `root` (not following symlinks).
pub fn tree_dirs(root: &Path) -> BTreeSet<RelPath> {
    let mut out = BTreeSet::new();
    walk_tree(root, &mut |p, ft| {
        if ft.is_dir() {
            out.insert(rel_of(root, p));
        }
    });
    out
}

/// Small deterministic PRNG (xorshift64*), so generated trees are identical across runs.
struct Rng(u64);

impl Rng {
    /// Seed via splitmix64, a bijection, so distinct seeds give distinct states.
    fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // xorshift needs a non-zero state; exactly one seed maps to 0.
        Self(if z == 0 { 0x9E37_79B9_7F4A_7C15 } else { z })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Write `n_files` text files under `root` in nested directories, with content derived from
/// `seed`. Same `(n_files, seed)` gives byte-identical trees. Returns the paths, sorted.
pub fn generate_tree(root: &Path, n_files: usize, seed: u64) -> Vec<RelPath> {
    let mut rng = Rng::new(seed);
    let mut paths = Vec::with_capacity(n_files);
    for i in 0..n_files {
        let p = rel(&format!("d{:02}/s{:02}/f{i:06}.txt", i % 17, (i / 17) % 13));
        let lines = 1 + (rng.next() % 40) as usize;
        let mut text = String::new();
        for l in 0..lines {
            text.push_str(&format!("line {l} {:016x}\n", rng.next()));
        }
        let abs = p.to_path(root);
        mkdir_parent(&abs);
        fs::write(&abs, text).unwrap_or_else(|e| panic!("write {p}: {e}"));
        paths.push(p);
    }
    paths.sort();
    paths
}

/// Modify the first `n` regular files under `root` (sorted by path): append a line and move
/// the mtime forward by 2 s so size+mtime change detection always sees it. Returns the paths.
pub fn touch_n(root: &Path, n: usize) -> Vec<RelPath> {
    let mut files = Vec::new();
    walk_tree(root, &mut |p, ft| {
        if ft.is_file() {
            files.push(p.to_path_buf());
        }
    });
    files.sort_by_key(|p| rel_of(root, p));
    files.truncate(n);
    let mut out = Vec::with_capacity(files.len());
    for abs in files {
        let old_mtime = fs::metadata(&abs)
            .and_then(|m| m.modified())
            .expect("mtime");
        let mut bytes = fs::read(&abs).expect("read");
        bytes.extend_from_slice(b"touched\n");
        fs::write(&abs, bytes).expect("write");
        let new_mtime = SystemTime::now().max(old_mtime) + Duration::from_secs(2);
        filetime::set_file_mtime(&abs, filetime::FileTime::from_system_time(new_mtime))
            .expect("set mtime");
        out.push(rel_of(root, &abs));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    #[test]
    fn test_home_is_temporary_and_removed() {
        let home = TestHome::new();
        let path = home.path().to_path_buf();
        assert!(path.is_dir());
        assert!(fs::read_dir(&path).unwrap().next().is_none());
        drop(home);
        assert!(!path.exists());
    }

    #[test]
    fn builder_writes_files_dirs_and_gitignore() {
        let fx = FixtureProject::new()
            .file("a.txt", "hello")
            .file("src/deep/b.rs", [0u8, 159, 146, 150])
            .dir("empty/inner")
            .gitignore(&["*.log", "build/"])
            .build();
        let bytes = tree_bytes(fx.root());
        assert_eq!(bytes[&rp("a.txt")], b"hello");
        assert_eq!(bytes[&rp("src/deep/b.rs")], [0u8, 159, 146, 150]);
        assert_eq!(bytes[&rp(".gitignore")], b"*.log\nbuild/\n");
        let dirs = tree_dirs(fx.root());
        assert!(dirs.contains(&rp("empty/inner")));
        assert!(dirs.contains(&rp("src/deep")));
        assert_eq!(
            fx.path("src/deep/b.rs"),
            fx.root().join("src").join("deep").join("b.rs")
        );
    }

    #[test]
    fn mtime_and_readonly_apply_regardless_of_call_order() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let fx = FixtureProject::new()
            .readonly("ro.txt")
            .mtime("ro.txt", t)
            .file("ro.txt", "x")
            .build();
        let md = fs::metadata(fx.path("ro.txt")).unwrap();
        assert!(md.permissions().readonly());
        assert_eq!(md.modified().unwrap(), t);
    }

    /// A read-only directory with content is what blocks deletion on unix; without
    /// `clear_readonly` in `Drop` this test fails there. (On Windows `remove_dir_all` ignores
    /// the read-only attribute, so it passes either way.)
    #[test]
    fn readonly_fixture_is_cleaned_up_on_drop() {
        let fx = FixtureProject::new()
            .file("d/ro.txt", "x")
            .file("d/sub/f", "y")
            .readonly("d/ro.txt")
            .readonly("d/sub")
            .readonly("d")
            .build();
        assert!(fs::metadata(fx.path("d")).unwrap().permissions().readonly());
        let root = fx.root().to_path_buf();
        drop(fx);
        assert!(!root.exists(), "fixture dir left behind");
    }

    #[test]
    fn symlink_to_dir_declared_before_the_dir() {
        if !symlinks_supported() {
            eprintln!("symlinks unsupported here; skipping");
            return;
        }
        let fx = FixtureProject::new()
            .symlink("link", "dir")
            .file("dir/a", "1")
            .build();
        let ft = fs::symlink_metadata(fx.path("link")).unwrap().file_type();
        assert!(ft.is_symlink());
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTypeExt;
            assert!(ft.is_symlink_dir(), "created as a file symlink");
        }
        assert!(fx.path("link").join("a").is_file());
    }

    #[test]
    fn build_at_keeps_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        drop(FixtureProject::new().file("a", "1").build_at(&root));
        assert!(root.join("a").is_file());
    }

    #[test]
    fn symlinks_are_created_or_reported() {
        let fx = FixtureProject::new()
            .file("target.txt", "t")
            .symlink("link", "target.txt")
            .build();
        if symlinks_supported() {
            assert!(fx.skipped_symlinks().is_empty());
            let md = fs::symlink_metadata(fx.path("link")).unwrap();
            assert!(md.file_type().is_symlink());
            assert_eq!(tree_bytes(fx.root())[&rp("link")], b"symlink:target.txt");
        } else {
            assert_eq!(fx.skipped_symlinks(), [rp("link")]);
            assert!(!fx.path("link").exists());
        }
    }

    #[test]
    fn generate_tree_is_deterministic() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let c = tempfile::tempdir().unwrap();
        let paths = generate_tree(a.path(), 250, 7);
        generate_tree(b.path(), 250, 7);
        generate_tree(c.path(), 250, 8);
        assert_eq!(paths.len(), 250);
        let ta = tree_bytes(a.path());
        assert_eq!(ta.len(), 250);
        assert_eq!(ta.keys().cloned().collect::<Vec<_>>(), paths);
        assert_eq!(ta, tree_bytes(b.path()));
        assert_ne!(ta, tree_bytes(c.path()));
    }

    #[test]
    fn adjacent_seeds_give_different_trees() {
        let mut seen = std::collections::HashSet::new();
        for seed in [0u64, 1, 2, 3, 6, 7, u64::MAX - 1, u64::MAX] {
            let tmp = tempfile::tempdir().unwrap();
            generate_tree(tmp.path(), 20, seed);
            assert!(seen.insert(tree_bytes(tmp.path())), "seed {seed} collides");
        }
    }

    #[test]
    fn touch_n_changes_exactly_n_files() {
        let tmp = tempfile::tempdir().unwrap();
        generate_tree(tmp.path(), 20, 1);
        let before = tree_bytes(tmp.path());
        let mtime_of = |p: &RelPath| {
            fs::metadata(p.to_path(tmp.path()))
                .unwrap()
                .modified()
                .unwrap()
        };
        let old_times: BTreeMap<_, _> = before.keys().map(|p| (p.clone(), mtime_of(p))).collect();

        let touched = touch_n(tmp.path(), 5);
        assert_eq!(touched.len(), 5);
        let after = tree_bytes(tmp.path());
        let changed: Vec<_> = after
            .iter()
            .filter(|(p, v)| before[*p] != **v)
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(changed, touched);
        for p in &touched {
            assert!(mtime_of(p) > old_times[p]);
        }
        assert_eq!(touch_n(tmp.path(), 100).len(), 20);
    }
}
