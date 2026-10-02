//! Deciding what a restore writes, deletes and must leave alone (DSNA-58, DSNA-87).
//!
//! The plan compares the target version with the folder as captured now. Paths are matched
//! by a key that folds case on a case-insensitive filesystem (Windows, DSNA-89), so a file
//! to write and a file to delete that name the same file on disk are one write, never a
//! delete.
//!
//! Rule 1: a path is *uncaptured* when changing it could replace or remove content that no
//! snapshot holds. Such a path is never written, created or deleted; it is reported in
//! `uncaptured`. See [`Guard::holds`].

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::error::Result;
use crate::ignore_rules::IgnoreRules;
use crate::types::{Entry, EntryKind, RelPath, SkippedFile};

/// Matching key for a path: lowercased when the filesystem ignores case.
fn key(path: &str, ci: bool) -> String {
    if ci {
        path.to_lowercase()
    } else {
        path.to_owned()
    }
}

/// Every proper ancestor of `path`, nearest first (`a/b/c` → `a/b`, `a`).
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').rev().map(move |(i, _)| &path[..i])
}

/// The folder as captured now.
#[derive(Debug)]
pub(crate) struct Current {
    /// Entries whose content a snapshot holds, by key.
    entries: HashMap<String, Entry>,
    /// Keys of paths in the folder whose content no snapshot holds.
    held: Vec<String>,
    /// Those paths as captured (a non-UTF-8 name has none).
    held_paths: Vec<RelPath>,
    ci: bool,
}

impl Current {
    /// Build from a capture: `entries` (with carried-forward entries for skipped paths),
    /// the paths it `skipped`, and the files it read as `unstable`.
    ///
    /// Held paths are the skipped and unstable ones plus files without a hash (a status
    /// capture keeps an unreadable file that way). Entries at or below a held path are not
    /// current content: snapshot carries the old entry forward for them (DSNA-50).
    pub(crate) fn new(
        entries: Vec<Entry>,
        skipped: &[SkippedFile],
        unstable: &[RelPath],
        ci: bool,
    ) -> Self {
        let mut held: Vec<String> = skipped
            .iter()
            .map(|s| key(&s.path, ci))
            .chain(unstable.iter().map(|p| key(p.as_str(), ci)))
            .chain(
                entries
                    .iter()
                    .filter(|e| e.kind == EntryKind::File && e.blob.is_none())
                    .map(|e| key(e.path.as_str(), ci)),
            )
            .collect();
        held.sort();
        held.dedup();
        let mut held_paths: Vec<RelPath> = skipped
            .iter()
            .filter_map(|s| RelPath::new(s.path.clone()).ok())
            .chain(unstable.iter().cloned())
            .chain(
                entries
                    .iter()
                    .filter(|e| e.kind == EntryKind::File && e.blob.is_none())
                    .map(|e| e.path.clone()),
            )
            .collect();
        held_paths.sort();
        held_paths.dedup();
        let held_set: HashSet<&str> = held.iter().map(String::as_str).collect();
        let entries = entries
            .into_iter()
            .map(|e| (key(e.path.as_str(), ci), e))
            .filter(|(k, _)| {
                !held_set.contains(k.as_str()) && !ancestors(k).any(|a| held_set.contains(a))
            })
            .collect();
        Self {
            entries,
            held,
            held_paths,
            ci,
        }
    }

    fn key(&self, p: &RelPath) -> String {
        key(p.as_str(), self.ci)
    }

    /// The captured entry at `p`, if its content is held.
    pub(crate) fn get(&self, p: &RelPath) -> Option<&Entry> {
        self.entries.get(&self.key(p))
    }
}

/// Decides whether a path is uncaptured (see the module docs).
struct Guard<'a> {
    root: &'a Path,
    current: &'a Current,
    rules: &'a IgnoreRules,
}

impl Guard<'_> {
    /// Whether writing (`as_file`: as a file or link) or creating `p` could replace or
    /// remove content no snapshot holds:
    ///
    /// - `p` is ignored by the current rules;
    /// - `p` is held (in the folder, but not capturable), or a held path is its ancestor,
    ///   or (as a file) its descendant;
    /// - an ancestor of `p` is on disk as a file or link that the capture does not hold;
    /// - (as a file) a directory at `p` holds anything the capture does not hold (an
    ///   ignored child, say), since replacing it would remove that.
    ///
    /// Errors if a `.gitignore` that applies cannot be read: the plan must not guess.
    fn holds(&self, p: &RelPath, as_file: bool) -> Result<bool> {
        if self.rules.try_is_ignored(p, !as_file)? {
            return Ok(true);
        }
        let k = self.current.key(p);
        let under = format!("{k}/");
        for q in &self.current.held {
            if *q == k || k.starts_with(&format!("{q}/")) || (as_file && q.starts_with(&under)) {
                return Ok(true);
            }
        }
        for a in ancestors(p.as_str()) {
            let abs = RelPath::new(a)?.to_path(self.root);
            match fs::symlink_metadata(&abs) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => {
                    if !self.current.entries.contains_key(&key(a, self.current.ci)) {
                        return Ok(true);
                    }
                }
                // Missing: nothing above it is in the way either.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                // Cannot tell what is there: leave it alone.
                Err(_) => return Ok(true),
            }
        }
        if as_file {
            let abs = p.to_path(self.root);
            if fs::symlink_metadata(&abs).is_ok_and(|m| m.is_dir()) && self.untracked(&abs, p) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether directory `abs` (project path `rel`) holds anything the capture does not.
    /// Unlistable counts as yes.
    fn untracked(&self, abs: &Path, rel: &RelPath) -> bool {
        let Ok(read) = fs::read_dir(abs) else {
            return true;
        };
        for item in read {
            let Ok(item) = item else { return true };
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                return true;
            };
            let Ok(child) = rel.join(&name) else {
                return true;
            };
            let Ok(ft) = item.file_type() else {
                return true;
            };
            if ft.is_dir() {
                if self.untracked(&item.path(), &child) {
                    return true;
                }
            } else if self.current.get(&child).is_none() {
                return true;
            }
        }
        false
    }
}

/// Whether writing `path` as a file would replace or remove content no snapshot holds
/// (see [`Guard::holds`]); also true when `path` itself is held.
pub(crate) fn uncaptured_file(
    root: &Path,
    current: &Current,
    rules: &IgnoreRules,
    path: &RelPath,
) -> Result<bool> {
    Guard {
        root,
        current,
        rules,
    }
    .holds(path, true)
}

/// What a restore will do.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    /// Target entries to write (files and links), by path.
    pub(crate) write: Vec<Entry>,
    /// Current entries (files and links) to remove, by path.
    pub(crate) delete: Vec<Entry>,
    /// Directories to create, by path.
    pub(crate) create_dirs: Vec<RelPath>,
    /// Directories to remove once empty (never recursively), deepest first. A failure is
    /// not an error: the directory still holds something.
    pub(crate) remove_dirs: Vec<RelPath>,
    /// Paths left alone (Rule 1), sorted.
    pub(crate) uncaptured: Vec<RelPath>,
}

/// Same content and attributes, so restoring changes nothing. A name that differs only in
/// case is rewritten so the version's casing comes back.
fn same(cur: &Entry, e: &Entry) -> bool {
    cur.path == e.path
        && cur.kind == e.kind
        && (e.kind == EntryKind::Dir || (cur.blob == e.blob && cur.readonly == e.readonly))
}

/// Plan restoring `target` (a version's entries) over `current`.
///
/// With `scope`, only that path is restored (restore one file): the target is that one
/// entry, and only current paths that collide with it are considered (the path itself, its
/// ancestors and its descendants).
pub(crate) fn plan(
    root: &Path,
    target: &[Entry],
    current: &Current,
    rules: &IgnoreRules,
    scope: Option<&RelPath>,
) -> Result<Plan> {
    let ci = current.ci;
    let guard = Guard {
        root,
        current,
        rules,
    };
    let target: HashMap<String, &Entry> = target
        .iter()
        .filter(|e| match scope {
            None => true,
            Some(s) => key(e.path.as_str(), ci) == key(s.as_str(), ci),
        })
        .map(|e| (key(e.path.as_str(), ci), e))
        .collect();
    // Descendants collide only with a file or link written at the scoped path.
    let scope_is_dir = target.values().all(|e| e.kind == EntryKind::Dir);
    let in_scope = |p: &str| match scope {
        None => true,
        Some(s) => {
            let (k, s) = (key(p, ci), key(s.as_str(), ci));
            k == s
                || s.starts_with(&format!("{k}/"))
                || (!scope_is_dir && k.starts_with(&format!("{s}/")))
        }
    };
    // Directories the target has, explicitly or as an ancestor of an entry.
    let mut target_dirs: HashSet<String> = HashSet::new();
    for (k, e) in &target {
        if e.kind == EntryKind::Dir {
            target_dirs.insert(k.clone());
        }
        target_dirs.extend(ancestors(k).map(str::to_owned));
    }

    let mut out = Plan::default();
    let mut uncaptured: HashSet<RelPath> = HashSet::new();
    for (k, e) in &target {
        let cur = current.entries.get(k);
        if cur.is_some_and(|c| same(c, e)) {
            continue;
        }
        let is_dir = e.kind == EntryKind::Dir;
        // A directory there already (as an entry, or implied by entries below it).
        let dir_exists = cur.is_some_and(|c| c.kind == EntryKind::Dir)
            || fs::symlink_metadata(e.path.to_path(root)).is_ok_and(|m| m.is_dir());
        if guard.holds(&e.path, !is_dir)? {
            // An existing dir needs nothing; anything else held there must stay.
            if !is_dir || !dir_exists {
                uncaptured.insert(e.path.clone());
            }
        } else if is_dir {
            if !dir_exists {
                out.create_dirs.push(e.path.clone());
            }
        } else {
            out.write.push((*e).clone());
        }
    }

    // Held paths the target does not have would be deleted if they were captured.
    for h in &current.held_paths {
        let k = key(h.as_str(), ci);
        if in_scope(&k) && !target.contains_key(&k) {
            uncaptured.insert(h.clone());
        }
    }

    let mut remove_dirs: HashSet<String> = HashSet::new();
    let mut dir_paths: HashMap<String, RelPath> = HashMap::new();
    for (k, cur) in &current.entries {
        if !in_scope(k) {
            continue;
        }
        if cur.kind == EntryKind::Dir {
            if !target_dirs.contains(k) {
                remove_dirs.insert(k.clone());
                dir_paths.insert(k.clone(), cur.path.clone());
            }
            continue;
        }
        // Kept when the target has a file or link here (the write replaces it).
        if target.get(k).is_some_and(|e| e.kind != EntryKind::Dir) {
            continue;
        }
        if guard.holds(&cur.path, true)? {
            uncaptured.insert(cur.path.clone());
            continue;
        }
        out.delete.push(cur.clone());
        let mut p = cur.path.parent();
        while let Some(a) = p {
            let ak = key(a.as_str(), ci);
            if target_dirs.contains(&ak) || !in_scope(&ak) {
                break;
            }
            remove_dirs.insert(ak.clone());
            p = a.parent();
            dir_paths.insert(ak, a);
        }
    }

    out.write.sort_by(|a, b| a.path.cmp(&b.path));
    out.delete.sort_by(|a, b| a.path.cmp(&b.path));
    out.create_dirs.sort();
    out.remove_dirs = remove_dirs
        .into_iter()
        .filter_map(|k| dir_paths.remove(&k))
        .collect();
    // Deepest first, so a parent is tried after its children are gone.
    out.remove_dirs.sort_by(|a, b| b.as_str().cmp(a.as_str()));
    out.uncaptured = uncaptured.into_iter().collect();
    out.uncaptured.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BlobHash, ProjectSettings, SkipReason};

    fn rp(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    fn file(p: &str, content: &str) -> Entry {
        Entry {
            path: rp(p),
            kind: EntryKind::File,
            blob: Some(BlobHash::of(content.as_bytes())),
            size: content.len() as u64,
            mtime_ns: 0,
            readonly: false,
        }
    }

    fn dir(p: &str) -> Entry {
        Entry {
            path: rp(p),
            kind: EntryKind::Dir,
            blob: None,
            size: 0,
            mtime_ns: 0,
            readonly: false,
        }
    }

    fn paths(v: &[Entry]) -> Vec<&str> {
        v.iter().map(|e| e.path.as_str()).collect()
    }

    fn strs(v: &[RelPath]) -> Vec<&str> {
        v.iter().map(RelPath::as_str).collect()
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        root: std::path::PathBuf,
        rules: IgnoreRules,
    }

    /// A folder holding `files` (path, content) with `.gitignore` text.
    fn fx(files: &[(&str, &str)], gitignore: &str) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("p");
        fs::create_dir(&root).unwrap();
        for (p, c) in files {
            let abs = rp(p).to_path(&root);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, c).unwrap();
        }
        if !gitignore.is_empty() {
            fs::write(root.join(".gitignore"), gitignore).unwrap();
        }
        let rules = IgnoreRules::new(&root, &ProjectSettings::default()).unwrap();
        Fx {
            _tmp: tmp,
            root,
            rules,
        }
    }

    fn too_large(p: &str) -> SkippedFile {
        SkippedFile {
            path: p.into(),
            reason: SkipReason::TooLarge { size: 60 << 20 },
        }
    }

    #[test]
    fn writes_changed_and_deleted_and_deletes_added() {
        let f = fx(
            &[("a.txt", "new"), ("added.txt", "x"), ("same.txt", "s")],
            "",
        );
        let cur = Current::new(
            vec![
                file("a.txt", "new"),
                file("added.txt", "x"),
                file("same.txt", "s"),
            ],
            &[],
            &[],
            false,
        );
        let target = [
            file("a.txt", "old"),
            file("gone.txt", "g"),
            file("same.txt", "s"),
        ];
        let p = plan(&f.root, &target, &cur, &f.rules, None).unwrap();
        assert_eq!(paths(&p.write), ["a.txt", "gone.txt"]);
        assert_eq!(paths(&p.delete), ["added.txt"]);
        assert!(p.uncaptured.is_empty());
    }

    #[test]
    fn over_cap_file_is_uncaptured_not_written() {
        // DSNA-87: the safety snapshot skipped data.bin, so its content is in no version.
        let f = fx(&[("data.bin", "huge")], "");
        let cur = Current::new(
            vec![file("data.bin", "small")],
            &[too_large("data.bin")],
            &[],
            false,
        );
        let p = plan(
            &f.root,
            &[file("data.bin", "small-old")],
            &cur,
            &f.rules,
            None,
        )
        .unwrap();
        assert!(p.write.is_empty());
        assert_eq!(strs(&p.uncaptured), ["data.bin"]);
    }

    #[test]
    fn ignored_path_in_target_is_uncaptured() {
        let f = fx(&[(".env", "secret now")], ".env\n");
        let cur = Current::new(vec![file(".gitignore", ".env\n")], &[], &[], false);
        let target = [file(".env", "old secret"), file(".gitignore", ".env\n")];
        let p = plan(&f.root, &target, &cur, &f.rules, None).unwrap();
        assert!(p.write.is_empty());
        assert_eq!(strs(&p.uncaptured), [".env"]);
    }

    #[test]
    fn held_ancestor_file_blocks_a_write_below_it() {
        // Version has pkg/a.txt; the folder has a 60 MB file `pkg`.
        let f = fx(&[("pkg", "huge")], "");
        let cur = Current::new(vec![], &[too_large("pkg")], &[], false);
        let p = plan(&f.root, &[file("pkg/a.txt", "a")], &cur, &f.rules, None).unwrap();
        assert!(p.write.is_empty());
        assert!(p.delete.is_empty());
        assert_eq!(strs(&p.uncaptured), ["pkg", "pkg/a.txt"]);
    }

    #[test]
    fn dir_with_ignored_child_blocks_a_file_write_over_it() {
        // Version has file `out`; the folder has out/ holding ignored out/precious.dat.
        let f = fx(&[("out/precious.dat", "p"), ("out/x.txt", "x")], "*.dat\n");
        let cur = Current::new(
            vec![file(".gitignore", "*.dat\n"), file("out/x.txt", "x")],
            &[],
            &[],
            false,
        );
        let target = [file(".gitignore", "*.dat\n"), file("out", "o")];
        let p = plan(&f.root, &target, &cur, &f.rules, None).unwrap();
        assert!(p.write.is_empty());
        assert_eq!(strs(&p.uncaptured), ["out"]);
    }

    #[test]
    fn tracked_dir_in_the_way_is_emptied_then_replaced() {
        let f = fx(&[("out/x.txt", "x")], "");
        let cur = Current::new(vec![file("out/x.txt", "x")], &[], &[], false);
        let p = plan(&f.root, &[file("out", "o")], &cur, &f.rules, None).unwrap();
        assert_eq!(paths(&p.write), ["out"]);
        assert_eq!(paths(&p.delete), ["out/x.txt"]);
        assert_eq!(strs(&p.remove_dirs), ["out"]);
    }

    #[test]
    fn file_where_target_has_dir_is_deleted_and_dir_created() {
        let f = fx(&[("d", "file")], "");
        let cur = Current::new(vec![file("d", "file")], &[], &[], false);
        let p = plan(&f.root, &[dir("d")], &cur, &f.rules, None).unwrap();
        assert_eq!(paths(&p.delete), ["d"]);
        assert_eq!(strs(&p.create_dirs), ["d"]);
    }

    #[test]
    fn unstable_and_unhashed_files_are_held() {
        let f = fx(&[("log.txt", "l"), ("locked.db", "d")], "");
        let mut locked = file("locked.db", "d");
        locked.blob = None;
        let cur = Current::new(
            vec![file("log.txt", "l"), locked],
            &[],
            &[rp("log.txt")],
            false,
        );
        // Neither is in the target: they would be deleted if they were captured.
        let p = plan(&f.root, &[], &cur, &f.rules, None).unwrap();
        assert!(p.delete.is_empty());
        assert_eq!(strs(&p.uncaptured), ["locked.db", "log.txt"]);
    }

    #[test]
    fn case_only_difference_is_one_write_when_case_insensitive() {
        // DSNA-89: Foo.rs in the version, foo.rs (edited) in the folder.
        let f = fx(&[("foo.rs", "y")], "");
        let cur = Current::new(vec![file("foo.rs", "y")], &[], &[], true);
        let p = plan(&f.root, &[file("Foo.rs", "x")], &cur, &f.rules, None).unwrap();
        assert_eq!(paths(&p.write), ["Foo.rs"]);
        assert!(p.delete.is_empty());
    }

    #[test]
    fn scope_limits_the_plan_to_one_path() {
        let f = fx(&[("a.txt", "new"), ("b.txt", "b")], "");
        let cur = Current::new(
            vec![file("a.txt", "new"), file("b.txt", "b")],
            &[],
            &[],
            false,
        );
        let target = [file("a.txt", "old"), file("c.txt", "c")];
        let p = plan(&f.root, &target, &cur, &f.rules, Some(&rp("a.txt"))).unwrap();
        assert_eq!(paths(&p.write), ["a.txt"]);
        assert!(p.delete.is_empty());
    }

    #[test]
    fn scoped_dir_restore_keeps_files_inside() {
        let f = fx(&[("d/x.txt", "x")], "");
        let cur = Current::new(vec![file("d/x.txt", "x")], &[], &[], false);
        let p = plan(&f.root, &[dir("d")], &cur, &f.rules, Some(&rp("d"))).unwrap();
        assert!(p.delete.is_empty());
        assert!(p.remove_dirs.is_empty());
        assert!(p.create_dirs.is_empty());
    }

    #[test]
    fn removes_dirs_left_empty_but_not_target_dirs() {
        let f = fx(&[("new/sub/x.txt", "x"), ("keep/y.txt", "y")], "");
        let cur = Current::new(
            vec![file("keep/y.txt", "y"), file("new/sub/x.txt", "x")],
            &[],
            &[],
            false,
        );
        let target = [dir("keep")];
        let p = plan(&f.root, &target, &cur, &f.rules, None).unwrap();
        assert_eq!(paths(&p.delete), ["keep/y.txt", "new/sub/x.txt"]);
        assert_eq!(strs(&p.remove_dirs), ["new/sub", "new"]);
    }
}
