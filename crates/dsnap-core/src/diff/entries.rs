//! Entry-level comparison of two file lists. Owner: Chain F (DSNA-9).
//!
//! Everything here except [`Dsnap::changes`] is a pure function over entry lists: no I/O.

use std::collections::{BTreeMap, HashMap};

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{
    ChangeCounts, ChangeStatus, Entry, EntryKind, FileChange, ProjectId, RelPath, VersionId,
    VersionRef,
};

/// Options for [`diff_entries`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryDiffOptions {
    /// Treat paths that differ only in case as the same file. When content is identical, the
    /// change is a [`ChangeStatus::Renamed`] instead of a delete plus an add. Default:
    /// `cfg!(windows)`.
    pub case_insensitive: bool,
}

impl Default for EntryDiffOptions {
    fn default() -> Self {
        Self {
            case_insensitive: cfg!(windows),
        }
    }
}

/// Compare two entry lists (any order) and return the changed paths, sorted by path.
///
/// - A path on one side only is [`ChangeStatus::Added`] or [`ChangeStatus::Deleted`]; this
///   includes directory entries (empty directories).
/// - A path on both sides is [`ChangeStatus::Modified`] when the kind, the content or the
///   read-only flag differs. A read-only-only change counts as modified so restore can reset
///   the flag. An mtime-only change is not a change.
/// - File content is compared by blob hash. When either side has no hash (an unhashed walk
///   result), size and mtime are compared instead, so callers should hash first.
/// - With [`EntryDiffOptions::case_insensitive`], a deleted and an added path that differ only
///   in case and have identical content become one [`ChangeStatus::Renamed`].
///
/// [`FileChange::path`] is the new path (the old path for deletions). Line counts are left
/// `None`.
pub fn diff_entries(old: &[Entry], new: &[Entry], opts: EntryDiffOptions) -> Vec<FileChange> {
    let old_by_path: BTreeMap<&RelPath, &Entry> = old.iter().map(|e| (&e.path, e)).collect();
    let new_by_path: BTreeMap<&RelPath, &Entry> = new.iter().map(|e| (&e.path, e)).collect();

    let mut changes = Vec::new();
    let mut deleted: Vec<&Entry> = Vec::new();
    let mut added: Vec<&Entry> = Vec::new();

    for (path, o) in &old_by_path {
        match new_by_path.get(path) {
            Some(n) if differs(o, n) => changes.push(change(ChangeStatus::Modified, o, n)),
            Some(_) => {}
            None => deleted.push(o),
        }
    }
    for (path, n) in &new_by_path {
        if !old_by_path.contains_key(path) {
            added.push(n);
        }
    }

    if opts.case_insensitive {
        pair_case_renames(&mut deleted, &mut added, &mut changes);
    }

    changes.extend(deleted.into_iter().map(|o| FileChange {
        path: o.path.clone(),
        status: ChangeStatus::Deleted,
        old: Some(o.clone()),
        new: None,
        lines_added: None,
        lines_removed: None,
    }));
    changes.extend(added.into_iter().map(|n| FileChange {
        path: n.path.clone(),
        status: ChangeStatus::Added,
        old: None,
        new: Some(n.clone()),
        lines_added: None,
        lines_removed: None,
    }));
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes
}

/// Count added/modified/deleted (a rename counts as modified).
pub fn count_changes(changes: &[FileChange]) -> ChangeCounts {
    let mut c = ChangeCounts::default();
    for ch in changes {
        let n = match ch.status {
            ChangeStatus::Added => &mut c.added,
            ChangeStatus::Deleted => &mut c.deleted,
            ChangeStatus::Modified | ChangeStatus::Renamed { .. } => &mut c.modified,
        };
        *n = n.saturating_add(1);
    }
    c
}

/// Changes whose path contains `query`, ignoring case (the changes-view filter box, F13).
///
/// A rename also matches on its old path. An empty or all-whitespace query matches everything.
pub fn filter_changes<'a>(changes: &'a [FileChange], query: &str) -> Vec<&'a FileChange> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return changes.iter().collect();
    }
    let hit = |p: &RelPath| p.as_str().to_lowercase().contains(&q);
    changes
        .iter()
        .filter(|c| {
            hit(&c.path) || matches!(&c.status, ChangeStatus::Renamed { from } if hit(from))
        })
        .collect()
}

/// Whether two entries at the same path differ.
fn differs(o: &Entry, n: &Entry) -> bool {
    o.readonly != n.readonly || !same_content(o, n)
}

/// Same kind and same content (ignores path, mtime and the read-only flag).
fn same_content(o: &Entry, n: &Entry) -> bool {
    match (&o.kind, &n.kind) {
        (EntryKind::File, EntryKind::File) => match (o.blob, n.blob) {
            (Some(a), Some(b)) => a == b,
            _ => o.size == n.size && o.mtime_ns == n.mtime_ns,
        },
        (EntryKind::Symlink { target: a }, EntryKind::Symlink { target: b }) => a == b,
        (EntryKind::Dir, EntryKind::Dir) => true,
        _ => false,
    }
}

fn change(status: ChangeStatus, o: &Entry, n: &Entry) -> FileChange {
    FileChange {
        path: n.path.clone(),
        status,
        old: Some(o.clone()),
        new: Some(n.clone()),
        lines_added: None,
        lines_removed: None,
    }
}

fn renamed(o: &Entry, n: &Entry) -> FileChange {
    change(
        ChangeStatus::Renamed {
            from: o.path.clone(),
        },
        o,
        n,
    )
}

/// Pair deleted and added entries whose paths differ only in case and whose content matches.
/// Paired entries are removed from `deleted` and `added`. Pairing is one-to-one and follows
/// path order, so it is deterministic.
fn pair_case_renames<'a>(
    deleted: &mut Vec<&'a Entry>,
    added: &mut Vec<&'a Entry>,
    out: &mut Vec<FileChange>,
) {
    if deleted.is_empty() || added.is_empty() {
        return;
    }
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, o) in deleted.iter().enumerate() {
        by_key.entry(fold(&o.path)).or_default().push(i);
    }
    let mut used_old = vec![false; deleted.len()];
    let mut used_new = vec![false; added.len()];
    for (j, n) in added.iter().enumerate() {
        let Some(cands) = by_key.get(&fold(&n.path)) else {
            continue;
        };
        if let Some(&i) = cands
            .iter()
            .find(|&&i| !used_old[i] && same_content(deleted[i], n))
        {
            used_old[i] = true;
            used_new[j] = true;
            out.push(renamed(deleted[i], n));
        }
    }
    retain_unused(deleted, &used_old);
    retain_unused(added, &used_new);
}

/// Drop the items whose `used` flag is set (same indexes).
fn retain_unused(v: &mut Vec<&Entry>, used: &[bool]) {
    let mut i = 0;
    v.retain(|_| {
        let keep = !used.get(i).copied().unwrap_or(false);
        i += 1;
        keep
    });
}

/// Case-folded path used to match case-only renames.
fn fold(p: &RelPath) -> String {
    p.as_str().to_lowercase()
}

impl Dsnap {
    /// Changes from `from` to `to`.
    ///
    /// `from = None` means the version before `to` (for [`VersionRef::WorkingTree`], the latest
    /// version); an empty list is used when there is none. Working-tree entries come from
    /// [`Dsnap::working_entries`].
    #[allow(unused_variables)] // stub; implemented by Chain K (DSNA-51)
    pub fn changes(
        &self,
        project: ProjectId,
        from: Option<VersionId>,
        to: VersionRef,
    ) -> Result<Vec<FileChange>> {
        todo!("DSNA-51")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::*;
    use crate::types::BlobHash;

    fn p(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    fn file(path: &str, content: &str) -> Entry {
        Entry {
            path: p(path),
            kind: EntryKind::File,
            blob: Some(BlobHash::of(content.as_bytes())),
            size: content.len() as u64,
            mtime_ns: 1,
            readonly: false,
        }
    }

    fn dir(path: &str) -> Entry {
        Entry {
            path: p(path),
            kind: EntryKind::Dir,
            blob: None,
            size: 0,
            mtime_ns: 1,
            readonly: false,
        }
    }

    fn link(path: &str, target: &str) -> Entry {
        Entry {
            path: p(path),
            kind: EntryKind::Symlink {
                target: target.into(),
            },
            blob: None,
            size: target.len() as u64,
            mtime_ns: 1,
            readonly: false,
        }
    }

    const CS: EntryDiffOptions = EntryDiffOptions {
        case_insensitive: false,
    };
    const CI: EntryDiffOptions = EntryDiffOptions {
        case_insensitive: true,
    };

    /// `(path, status)` pairs, with renames shown as `"R<-from"`.
    fn summary(changes: &[FileChange]) -> Vec<(String, String)> {
        changes
            .iter()
            .map(|c| {
                let s = match &c.status {
                    ChangeStatus::Added => "A".to_owned(),
                    ChangeStatus::Modified => "M".to_owned(),
                    ChangeStatus::Deleted => "D".to_owned(),
                    ChangeStatus::Renamed { from } => format!("R<-{from}"),
                };
                (c.path.to_string(), s)
            })
            .collect()
    }

    fn owned(v: Vec<(&str, &str)>) -> Vec<(String, String)> {
        v.into_iter()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect()
    }

    type Case = (
        &'static str,
        Vec<Entry>,
        Vec<Entry>,
        EntryDiffOptions,
        Vec<(&'static str, &'static str)>,
    );

    #[test]
    fn table() {
        let mut ro = file("a.txt", "x");
        ro.readonly = true;
        let mut touched = file("a.txt", "x");
        touched.mtime_ns = 99;
        let mut unhashed_same = file("a.txt", "x");
        unhashed_same.blob = None;
        let mut unhashed_grown = unhashed_same.clone();
        unhashed_grown.size = 5;

        #[rustfmt::skip]
        let cases: Vec<Case> = vec![
            ("empty", vec![], vec![], CS, vec![]),
            ("unchanged", vec![file("a.txt", "x")], vec![file("a.txt", "x")], CS, vec![]),
            ("mtime only", vec![file("a.txt", "x")], vec![touched], CS, vec![]),
            ("added", vec![], vec![file("a.txt", "x")], CS, vec![("a.txt", "A")]),
            ("deleted", vec![file("a.txt", "x")], vec![], CS, vec![("a.txt", "D")]),
            (
                "content",
                vec![file("a.txt", "x")],
                vec![file("a.txt", "y")],
                CS,
                vec![("a.txt", "M")],
            ),
            ("readonly only", vec![file("a.txt", "x")], vec![ro], CS, vec![("a.txt", "M")]),
            ("file to dir", vec![file("a", "x")], vec![dir("a")], CS, vec![("a", "M")]),
            ("file to symlink", vec![file("a", "x")], vec![link("a", "x")], CS, vec![("a", "M")]),
            ("symlink target", vec![link("a", "x")], vec![link("a", "y")], CS, vec![("a", "M")]),
            ("symlink same", vec![link("a", "x")], vec![link("a", "x")], CS, vec![]),
            ("empty dir added", vec![], vec![dir("d")], CS, vec![("d", "A")]),
            ("empty dir deleted", vec![dir("d")], vec![], CS, vec![("d", "D")]),
            ("dir unchanged", vec![dir("d")], vec![dir("d")], CS, vec![]),
            ("unhashed same stat", vec![file("a.txt", "x")], vec![unhashed_same], CS, vec![]),
            (
                "unhashed new size",
                vec![file("a.txt", "x")],
                vec![unhashed_grown],
                CS,
                vec![("a.txt", "M")],
            ),
            (
                "case rename, case-insensitive",
                vec![file("Foo.rs", "x")],
                vec![file("foo.rs", "x")],
                CI,
                vec![("foo.rs", "R<-Foo.rs")],
            ),
            (
                "case rename, case-sensitive",
                vec![file("Foo.rs", "x")],
                vec![file("foo.rs", "x")],
                CS,
                vec![("Foo.rs", "D"), ("foo.rs", "A")],
            ),
            (
                "case rename with content change",
                vec![file("Foo.rs", "x")],
                vec![file("foo.rs", "y")],
                CI,
                vec![("Foo.rs", "D"), ("foo.rs", "A")],
            ),
            (
                "case rename of directory part",
                vec![file("Src/a.rs", "x"), dir("Src")],
                vec![file("src/a.rs", "x"), dir("src")],
                CI,
                vec![("src", "R<-Src"), ("src/a.rs", "R<-Src/a.rs")],
            ),
            (
                "case rename, unicode",
                vec![file("Ärger.txt", "x")],
                vec![file("ärger.txt", "x")],
                CI,
                vec![("ärger.txt", "R<-Ärger.txt")],
            ),
            (
                "mixed, sorted by path",
                vec![file("b", "1"), file("c", "1"), file("d", "1")],
                vec![file("a", "1"), file("c", "2"), file("d", "1")],
                CS,
                vec![("a", "A"), ("b", "D"), ("c", "M")],
            ),
        ];

        for (name, old, new, opts, want) in cases {
            let got = summary(&diff_entries(&old, &new, opts));
            assert_eq!(got, owned(want), "case {name:?}");
        }
    }

    #[test]
    fn input_order_does_not_matter() {
        let old = vec![file("z", "1"), file("a", "1"), file("m", "1")];
        let new = vec![file("m", "2"), file("b", "1"), file("z", "1")];
        let got = summary(&diff_entries(&old, &new, CS));
        assert_eq!(got, owned(vec![("a", "D"), ("b", "A"), ("m", "M")]));
    }

    #[test]
    fn change_carries_both_entries() {
        let old = vec![file("a", "1"), file("Foo", "1"), file("gone", "1")];
        let new = vec![file("a", "2"), file("foo", "1"), file("new", "1")];
        let changes = diff_entries(&old, &new, CI);
        assert_eq!(changes.len(), 4);
        for c in changes {
            let old_path = c.old.as_ref().map(|e| e.path.clone());
            let new_path = c.new.as_ref().map(|e| e.path.clone());
            match c.status {
                ChangeStatus::Added => assert_eq!((old_path, new_path), (None, Some(c.path))),
                ChangeStatus::Deleted => assert_eq!((old_path, new_path), (Some(c.path), None)),
                ChangeStatus::Modified => {
                    assert_eq!((old_path, new_path), (Some(c.path.clone()), Some(c.path)))
                }
                ChangeStatus::Renamed { from } => {
                    assert_eq!((old_path, new_path), (Some(from), Some(c.path)))
                }
            }
            assert_eq!((c.lines_added, c.lines_removed), (None, None));
        }
    }

    #[test]
    fn case_pairing_is_one_to_one() {
        // Two old paths that fold to the same key (possible on a case-sensitive file system).
        let old = vec![file("FOO", "x"), file("Foo", "x")];
        let new = vec![file("foo", "x")];
        let got = summary(&diff_entries(&old, &new, CI));
        assert_eq!(got, owned(vec![("Foo", "D"), ("foo", "R<-FOO")]));
    }

    #[test]
    fn exact_match_beats_case_match() {
        let old = vec![file("foo", "x"), file("Foo", "x")];
        let new = vec![file("foo", "x")];
        let got = summary(&diff_entries(&old, &new, CI));
        assert_eq!(got, owned(vec![("Foo", "D")]));
    }

    #[test]
    fn default_options_follow_platform() {
        assert_eq!(EntryDiffOptions::default().case_insensitive, cfg!(windows));
    }

    #[test]
    fn counts() {
        let old = vec![file("a", "1"), file("b", "1"), file("Foo", "1")];
        let new = vec![
            file("a", "2"),
            file("c", "1"),
            file("d", "1"),
            file("foo", "1"),
        ];
        let c = count_changes(&diff_entries(&old, &new, CI));
        let want = ChangeCounts {
            added: 2,
            modified: 2,
            deleted: 1,
        };
        assert_eq!(c, want);
        assert_eq!(count_changes(&[]), ChangeCounts::default());
    }

    #[test]
    fn filter() {
        let old = vec![file("src/Main.rs", "1"), file("Old/Name.txt", "n")];
        let new = vec![
            file("src/Main.rs", "2"),
            file("README.md", "r"),
            file("old/name.txt", "n"),
        ];
        let changes = diff_entries(&old, &new, CI);
        let paths = |q: &str| -> Vec<String> {
            filter_changes(&changes, q)
                .into_iter()
                .map(|c| c.path.to_string())
                .collect()
        };
        assert_eq!(paths(""), vec!["README.md", "old/name.txt", "src/Main.rs"]);
        assert_eq!(paths("  "), paths(""));
        assert_eq!(paths("main"), vec!["src/Main.rs"]);
        assert_eq!(paths("SRC/m"), vec!["src/Main.rs"]);
        assert_eq!(paths(".md"), vec!["README.md"]);
        assert_eq!(paths("Old/Name"), vec!["old/name.txt"]);
        assert!(paths("nothing").is_empty());
    }

    #[test]
    fn filter_matches_rename_source() {
        let changes = vec![FileChange {
            path: p("b/new.txt"),
            status: ChangeStatus::Renamed {
                from: p("a/legacy.txt"),
            },
            old: None,
            new: None,
            lines_added: None,
            lines_removed: None,
        }];
        assert_eq!(filter_changes(&changes, "LEGACY").len(), 1);
        assert_eq!(filter_changes(&changes, "new").len(), 1);
        assert_eq!(filter_changes(&changes, "zzz").len(), 0);
    }

    /// Small path alphabet with case variants so case renames and collisions happen often.
    fn arb_entries() -> impl Strategy<Value = Vec<Entry>> {
        let name = prop::sample::select(vec!["a", "A", "b", "B", "c", "d/e", "d/E", "D/e", "f"]);
        let kind = 0u8..4;
        let content = prop::sample::select(vec!["", "1", "2", "3"]);
        let readonly = any::<bool>();
        prop::collection::vec((name, kind, content, readonly), 0..12).prop_map(|items| {
            let mut by_path = BTreeMap::new();
            for (n, k, c, ro) in items {
                let mut e = match k {
                    0 | 1 => file(n, c),
                    2 => dir(n),
                    _ => link(n, c),
                };
                e.readonly = ro;
                by_path.insert(n, e);
            }
            by_path.into_values().collect()
        })
    }

    /// Apply `changes` to `old`'s path set and check the result is `new`'s path set.
    fn check_applies(
        old: &[Entry],
        new: &[Entry],
        changes: &[FileChange],
        renames_allowed: bool,
    ) -> std::result::Result<(), TestCaseError> {
        let new_by_path: BTreeMap<_, _> = new.iter().map(|e| (e.path.clone(), e)).collect();
        let old_by_path: BTreeMap<_, _> = old.iter().map(|e| (e.path.clone(), e)).collect();
        let mut paths: BTreeSet<RelPath> = old_by_path.keys().cloned().collect();
        for c in changes {
            match &c.status {
                ChangeStatus::Added => prop_assert!(paths.insert(c.path.clone())),
                ChangeStatus::Deleted => prop_assert!(paths.remove(&c.path)),
                ChangeStatus::Modified => prop_assert!(paths.contains(&c.path)),
                ChangeStatus::Renamed { from } => {
                    prop_assert!(renames_allowed);
                    prop_assert!(paths.remove(from));
                    prop_assert!(paths.insert(c.path.clone()));
                    prop_assert_eq!(c.old.as_ref(), old_by_path.get(from).copied());
                }
            }
            if c.status != ChangeStatus::Deleted {
                prop_assert_eq!(c.new.as_ref(), new_by_path.get(&c.path).copied());
            }
        }
        let want: BTreeSet<RelPath> = new_by_path.keys().cloned().collect();
        prop_assert_eq!(paths, want);
        prop_assert!(changes.windows(2).all(|w| w[0].path < w[1].path));
        Ok(())
    }

    proptest! {
        #[test]
        fn applying_diff_to_old_yields_new(
            old in arb_entries(),
            new in arb_entries(),
            ci in any::<bool>(),
        ) {
            let changes = diff_entries(&old, &new, EntryDiffOptions { case_insensitive: ci });
            check_applies(&old, &new, &changes, ci)?;
        }

        #[test]
        fn diff_of_identical_lists_is_empty(old in arb_entries(), ci in any::<bool>()) {
            let changes = diff_entries(&old, &old, EntryDiffOptions { case_insensitive: ci });
            prop_assert!(changes.is_empty());
        }
    }
}
