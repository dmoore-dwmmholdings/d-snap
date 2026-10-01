//! Entry-level comparison of two file lists. Owner: Chain F (DSNA-9).
//!
//! Everything here except [`Dsnap::changes`] is a pure function over entry lists: no I/O.

use std::collections::{BTreeMap, HashMap};

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{
    BlobHash, ChangeCounts, ChangeStatus, Entry, EntryKind, FileChange, ProjectId, RelPath,
    VersionId, VersionRef,
};

/// Options for [`diff_entries`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryDiffOptions {
    /// Treat paths that differ only in case as the same file: a deleted and an added path that
    /// differ only in case become one [`ChangeStatus::Renamed`], even when the content or kind
    /// also changed (on a case-insensitive file system both names are one file). Default:
    /// `cfg!(windows)`.
    pub case_insensitive: bool,
    /// Pair a deleted and an added file with the same non-empty content as a
    /// [`ChangeStatus::Renamed`] (F18). Default: `true`.
    pub detect_renames: bool,
}

impl Default for EntryDiffOptions {
    fn default() -> Self {
        Self {
            case_insensitive: cfg!(windows),
            detect_renames: true,
        }
    }
}

/// Above this many candidate pairs for one content hash, rename pairing skips the
/// directory-distance ranking (which is quadratic) and pairs by file name, then path order.
const MAX_RANKED_PAIRS: usize = 512 * 512;

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
///   in case become one [`ChangeStatus::Renamed`], whatever their content or kind; compare
///   the change's `old` and `new` entries to see what else changed. Among several candidates
///   identical content wins, then the same kind, then path order.
/// - With [`EntryDiffOptions::detect_renames`], a remaining deleted and added file with the
///   same blob hash become one [`ChangeStatus::Renamed`]. Matching is one-to-one. Among
///   several candidates the same file name wins, then the nearest directory, then path order.
///   Empty files never pair, and a move with a content change stays a delete plus an add.
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
    if opts.detect_renames {
        pair_content_renames(&mut deleted, &mut added, &mut changes);
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

/// Pair deleted and added entries whose paths differ only in case, whatever their content or
/// kind (DSNA-89: on a case-insensitive file system both names are one file, so a delete plus
/// an add would be wrong). Paired entries are removed from `deleted` and `added`.
///
/// Pairing is one-to-one and runs in three passes over all added paths: identical content,
/// then the same kind, then anything. Within a pass, each free added path (in path order)
/// takes the first free fold-equal old path (in path order). So an exact-content match is
/// never taken by an earlier added path that only matches by case (DSNA-95), and the result
/// is deterministic.
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
    let cands: Vec<Option<&Vec<usize>>> =
        added.iter().map(|n| by_key.get(&fold(&n.path))).collect();
    // Pass 0: identical content; pass 1: same kind; pass 2: anything.
    let accept = |pass: u8, o: &Entry, n: &Entry| match pass {
        0 => same_content(o, n),
        1 => std::mem::discriminant(&o.kind) == std::mem::discriminant(&n.kind),
        _ => true,
    };
    let mut used_old = vec![false; deleted.len()];
    let mut used_new = vec![false; added.len()];
    for pass in 0..3u8 {
        for (j, n) in added.iter().enumerate() {
            let Some(olds) = cands[j] else {
                continue;
            };
            if used_new[j] {
                continue;
            }
            if let Some(&i) = olds
                .iter()
                .find(|&&i| !used_old[i] && accept(pass, deleted[i], n))
            {
                used_old[i] = true;
                used_new[j] = true;
                out.push(renamed(deleted[i], n));
            }
        }
    }
    retain_unused(deleted, &used_old);
    retain_unused(added, &used_new);
}

/// Pair deleted and added files that have the same non-empty blob hash. Paired entries are
/// removed from `deleted` and `added`.
fn pair_content_renames<'a>(
    deleted: &mut Vec<&'a Entry>,
    added: &mut Vec<&'a Entry>,
    out: &mut Vec<FileChange>,
) {
    if deleted.is_empty() || added.is_empty() {
        return;
    }
    let empty = BlobHash::of(b"");
    let key = |e: &Entry| match (&e.kind, e.blob) {
        (EntryKind::File, Some(h)) if h != empty => Some(h),
        _ => None,
    };
    // BTreeMap so groups are visited in a fixed order; indexes stay in path order.
    let mut groups: BTreeMap<BlobHash, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, o) in deleted.iter().enumerate() {
        if let Some(h) = key(o) {
            groups.entry(h).or_default().0.push(i);
        }
    }
    for (j, n) in added.iter().enumerate() {
        if let Some(h) = key(n) {
            if let Some(g) = groups.get_mut(&h) {
                g.1.push(j);
            }
        }
    }
    let mut used_old = vec![false; deleted.len()];
    let mut used_new = vec![false; added.len()];
    for (olds, news) in groups.values() {
        if news.is_empty() {
            continue;
        }
        let pairs = if olds.len().saturating_mul(news.len()) <= MAX_RANKED_PAIRS {
            ranked_pairs(deleted, added, olds, news)
        } else {
            name_order_pairs(deleted, added, olds, news)
        };
        for (i, j) in pairs {
            used_old[i] = true;
            used_new[j] = true;
            out.push(renamed(deleted[i], added[j]));
        }
    }
    retain_unused(deleted, &used_old);
    retain_unused(added, &used_new);
}

/// One-to-one pairs from `olds` x `news`, best first: same file name, then fewest directory
/// steps between the parents, then old path, then new path. Greedy over the sorted list.
fn ranked_pairs(
    deleted: &[&Entry],
    added: &[&Entry],
    olds: &[usize],
    news: &[usize],
) -> Vec<(usize, usize)> {
    let mut cands: Vec<(bool, usize, usize, usize)> = Vec::with_capacity(olds.len() * news.len());
    for &i in olds {
        for &j in news {
            let (o, n) = (&deleted[i].path, &added[j].path);
            cands.push((o.file_name() != n.file_name(), dir_distance(o, n), i, j));
        }
    }
    // `deleted` and `added` are in path order, so comparing indexes compares paths.
    cands.sort_unstable();
    let mut taken_old = vec![false; deleted.len()];
    let mut taken_new = vec![false; added.len()];
    let mut out = Vec::new();
    for (_, _, i, j) in cands {
        if !taken_old[i] && !taken_new[j] {
            taken_old[i] = true;
            taken_new[j] = true;
            out.push((i, j));
        }
    }
    out
}

/// Cheap pairing for very large groups: same file name first, then the rest in path order.
fn name_order_pairs(
    deleted: &[&Entry],
    added: &[&Entry],
    olds: &[usize],
    news: &[usize],
) -> Vec<(usize, usize)> {
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for &i in olds.iter().rev() {
        by_name
            .entry(deleted[i].path.file_name())
            .or_default()
            .push(i);
    }
    let mut taken_old = vec![false; deleted.len()];
    let mut out = Vec::new();
    let mut rest = Vec::new();
    for &j in news {
        match by_name
            .get_mut(added[j].path.file_name())
            .and_then(Vec::pop)
        {
            Some(i) => {
                taken_old[i] = true;
                out.push((i, j));
            }
            None => rest.push(j),
        }
    }
    let free_olds = olds.iter().copied().filter(|&i| !taken_old[i]);
    out.extend(free_olds.zip(rest));
    out
}

/// Number of directory steps between the parent directories of `a` and `b`.
fn dir_distance(a: &RelPath, b: &RelPath) -> usize {
    let parent = |p: &RelPath| -> Vec<String> {
        p.parent()
            .map(|d| d.as_str().split('/').map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let (pa, pb) = (parent(a), parent(b));
    let common = pa.iter().zip(&pb).take_while(|(x, y)| x == y).count();
    pa.len() + pb.len() - 2 * common
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
        detect_renames: false,
    };
    const CI: EntryDiffOptions = EntryDiffOptions {
        case_insensitive: true,
        detect_renames: false,
    };
    const RN: EntryDiffOptions = EntryDiffOptions {
        case_insensitive: false,
        detect_renames: true,
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
        let mut ro_foo = file("foo.rs", "x");
        ro_foo.readonly = true;

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
                vec![("foo.rs", "R<-Foo.rs")],
            ),
            (
                "case rename with content change, case-sensitive",
                vec![file("Foo.rs", "x")],
                vec![file("foo.rs", "y")],
                CS,
                vec![("Foo.rs", "D"), ("foo.rs", "A")],
            ),
            (
                "case rename with read-only change",
                vec![file("Foo.rs", "x")],
                vec![ro_foo],
                CI,
                vec![("foo.rs", "R<-Foo.rs")],
            ),
            (
                "case rename with kind change",
                vec![file("Foo", "x")],
                vec![dir("foo")],
                CI,
                vec![("foo", "R<-Foo")],
            ),
            (
                "case rename of symlink with new target",
                vec![link("L", "x")],
                vec![link("l", "y")],
                CI,
                vec![("l", "R<-L")],
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
    fn case_rename_with_edit_carries_both_blobs() {
        let changes = diff_entries(&[file("Foo.rs", "x")], &[file("foo.rs", "y")], CI);
        let [c] = changes.as_slice() else {
            panic!("want one change, got {changes:?}");
        };
        assert_eq!(c.status, ChangeStatus::Renamed { from: p("Foo.rs") });
        assert_eq!(
            c.old.as_ref().and_then(|e| e.blob),
            Some(BlobHash::of(b"x"))
        );
        assert_eq!(
            c.new.as_ref().and_then(|e| e.blob),
            Some(BlobHash::of(b"y"))
        );
        assert_eq!(count_changes(&changes).modified, 1);
    }

    #[test]
    fn case_pairing_prefers_same_content_then_same_kind() {
        // Several old paths fold to `foo` (possible after a snapshot on a case-sensitive FS).
        let old = vec![dir("FOO"), file("FoO", "edited"), file("fOO", "x")];
        let new = vec![file("foo", "x")];
        let got = summary(&diff_entries(&old, &new, CI));
        assert_eq!(
            got,
            owned(vec![("FOO", "D"), ("FoO", "D"), ("foo", "R<-fOO")])
        );

        let old = vec![dir("FOO"), file("FoO", "y"), file("fOO", "z")];
        let got = summary(&diff_entries(&old, &new, CI));
        assert_eq!(
            got,
            owned(vec![("FOO", "D"), ("fOO", "D"), ("foo", "R<-FoO")])
        );
    }

    #[test]
    fn case_pairing_exact_content_is_not_taken_by_earlier_path() {
        // DSNA-95: `aB` comes first but has no exact match; it must not take `AB`, the exact
        // match of `ab`.
        let old = vec![file("AB", "x"), file("Ab", "y")];
        let new = vec![file("aB", "z"), file("ab", "x")];
        let got = summary(&diff_entries(&old, &new, CI));
        assert_eq!(got, owned(vec![("aB", "R<-Ab"), ("ab", "R<-AB")]));
    }

    #[test]
    fn case_pairing_same_kind_is_not_taken_by_earlier_path() {
        // `aB` (a dir) has no same-kind candidate; it must not take `AB`, the only symlink
        // candidate of `ab`.
        let old = vec![link("AB", "t"), file("Ab", "x")];
        let new = vec![dir("aB"), link("ab", "u")];
        let got = summary(&diff_entries(&old, &new, CI));
        assert_eq!(got, owned(vec![("aB", "R<-Ab"), ("ab", "R<-AB")]));
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
        assert!(EntryDiffOptions::default().detect_renames);
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

    fn renames(old: &[Entry], new: &[Entry], opts: EntryDiffOptions) -> Vec<(String, String)> {
        summary(&diff_entries(old, new, opts))
    }

    #[test]
    fn rename_simple_move() {
        let got = renames(&[file("a.txt", "x")], &[file("b.txt", "x")], RN);
        assert_eq!(got, owned(vec![("b.txt", "R<-a.txt")]));
    }

    #[test]
    fn rename_across_dirs() {
        let old = [file("src/deep/x.rs", "x"), file("keep.rs", "k")];
        let new = [file("lib/x.rs", "x"), file("keep.rs", "k")];
        let got = renames(&old, &new, RN);
        assert_eq!(got, owned(vec![("lib/x.rs", "R<-src/deep/x.rs")]));
    }

    #[test]
    fn rename_two_identical_files_pair_by_name() {
        let old = [file("a/one.txt", "same"), file("a/two.txt", "same")];
        let new = [file("b/two.txt", "same"), file("b/one.txt", "same")];
        let got = renames(&old, &new, RN);
        let want = vec![("b/one.txt", "R<-a/one.txt"), ("b/two.txt", "R<-a/two.txt")];
        assert_eq!(got, owned(want));
    }

    #[test]
    fn rename_prefers_nearest_directory() {
        let old = [file("x/a.txt", "same"), file("y/z/b.txt", "same")];
        let new = [file("y/z/w/d.txt", "same"), file("x/c.txt", "same")];
        let got = renames(&old, &new, RN);
        let want = vec![("x/c.txt", "R<-x/a.txt"), ("y/z/w/d.txt", "R<-y/z/b.txt")];
        assert_eq!(got, owned(want));
    }

    #[test]
    fn rename_name_beats_directory() {
        let old = [file("x/a.txt", "same"), file("far/away/b.txt", "same")];
        let new = [file("x/b.txt", "same"), file("x/c.txt", "same")];
        let got = renames(&old, &new, RN);
        let want = vec![("x/b.txt", "R<-far/away/b.txt"), ("x/c.txt", "R<-x/a.txt")];
        assert_eq!(got, owned(want));
    }

    #[test]
    fn rename_empty_file_not_paired() {
        let got = renames(&[file("empty.txt", "")], &[file("moved.txt", "")], RN);
        assert_eq!(got, owned(vec![("empty.txt", "D"), ("moved.txt", "A")]));
    }

    #[test]
    fn rename_with_content_change_is_delete_and_add() {
        let got = renames(&[file("a.txt", "x")], &[file("b.txt", "y")], RN);
        assert_eq!(got, owned(vec![("a.txt", "D"), ("b.txt", "A")]));
    }

    #[test]
    fn rename_detection_off() {
        let got = renames(&[file("a.txt", "x")], &[file("b.txt", "x")], CS);
        assert_eq!(got, owned(vec![("a.txt", "D"), ("b.txt", "A")]));
    }

    #[test]
    fn rename_ignores_dirs_and_symlinks() {
        let old = [dir("d1"), link("l1", "t")];
        let new = [dir("d2"), link("l2", "t")];
        let got = renames(&old, &new, RN);
        let want = vec![("d1", "D"), ("d2", "A"), ("l1", "D"), ("l2", "A")];
        assert_eq!(got, owned(want));
    }

    #[test]
    fn rename_ignores_dirs_and_symlinks_with_blob() {
        // Only files pair by content, even if a dir or symlink entry carries a blob hash.
        let blob = Some(BlobHash::of(b"same"));
        let (mut d1, mut d2) = (dir("d1"), dir("d2"));
        let (mut l1, mut l2) = (link("l1", "t"), link("l2", "t"));
        for e in [&mut d1, &mut d2, &mut l1, &mut l2] {
            e.blob = blob;
        }
        let got = renames(&[d1, l1], &[d2, l2], RN);
        let want = vec![("d1", "D"), ("d2", "A"), ("l1", "D"), ("l2", "A")];
        assert_eq!(got, owned(want));
    }

    #[test]
    fn rename_one_to_one_with_leftovers() {
        let old = [file("a", "x"), file("b", "x"), file("c", "x")];
        let new = [file("d", "x")];
        let got = renames(&old, &new, RN);
        assert_eq!(got, owned(vec![("b", "D"), ("c", "D"), ("d", "R<-a")]));
    }

    #[test]
    fn rename_tie_break_is_deterministic() {
        // Same name rank and directory distance everywhere: pair by old path, then new path.
        let old = vec![file("p/a1", "x"), file("p/a2", "x")];
        let new = vec![file("p/b2", "x"), file("p/b1", "x")];
        let want = owned(vec![("p/b1", "R<-p/a1"), ("p/b2", "R<-p/a2")]);
        assert_eq!(renames(&old, &new, RN), want);
        let (mut old_rev, mut new_rev) = (old.clone(), new.clone());
        old_rev.reverse();
        new_rev.reverse();
        assert_eq!(renames(&old_rev, &new_rev, RN), want);
    }

    #[test]
    fn case_rename_is_matched_before_content_rename() {
        let opts = EntryDiffOptions {
            case_insensitive: true,
            detect_renames: true,
        };
        let new = [file("bar", "x"), file("foo", "x")];
        let got = renames(&[file("Foo", "x")], &new, opts);
        assert_eq!(got, owned(vec![("bar", "A"), ("foo", "R<-Foo")]));
    }

    #[test]
    fn rename_large_group_uses_name_fallback() {
        let n = 600;
        assert!(n * n > MAX_RANKED_PAIRS);
        let old: Vec<Entry> = (0..n).map(|i| file(&format!("a/f{i:04}"), "x")).collect();
        let mut new: Vec<Entry> = (0..n).map(|i| file(&format!("b/f{i:04}"), "x")).collect();
        new.push(file("b/extra", "x"));
        let changes = diff_entries(&old, &new, RN);
        assert_eq!(changes.len(), n + 1);
        for c in &changes {
            match &c.status {
                ChangeStatus::Renamed { from } => assert_eq!(from.file_name(), c.path.file_name()),
                ChangeStatus::Added => assert_eq!(c.path.as_str(), "b/extra"),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn rename_large_group_fallback_pairs_leftovers_in_path_order() {
        let n = 600;
        let old: Vec<Entry> = (0..n).map(|i| file(&format!("a/o{i:04}"), "x")).collect();
        let new: Vec<Entry> = (0..n).map(|i| file(&format!("b/n{i:04}"), "x")).collect();
        let changes = diff_entries(&old, &new, RN);
        assert_eq!(changes.len(), n);
        for c in &changes {
            let ChangeStatus::Renamed { from } = &c.status else {
                panic!("unexpected {:?}", c.status);
            };
            assert_eq!(from.file_name()[1..], c.path.file_name()[1..]);
        }
    }

    #[test]
    fn rename_large_group_fallback_prefers_lower_path_among_same_name() {
        let n = 600;
        let mut old: Vec<Entry> = (0..n).map(|i| file(&format!("a/o{i:04}"), "x")).collect();
        old.extend([file("a/y/f", "x"), file("a/x/f", "x")]);
        let mut new: Vec<Entry> = (0..n).map(|i| file(&format!("b/n{i:04}"), "x")).collect();
        new.push(file("b/f", "x"));
        assert!(old.len() * new.len() > MAX_RANKED_PAIRS);
        let changes = diff_entries(&old, &new, RN);
        let f = changes
            .iter()
            .find(|c| c.path.as_str() == "b/f")
            .map(|c| &c.status);
        assert_eq!(f, Some(&ChangeStatus::Renamed { from: p("a/x/f") }));
    }

    #[test]
    fn dir_distance_counts_steps() {
        assert_eq!(dir_distance(&p("a"), &p("b")), 0);
        assert_eq!(dir_distance(&p("x/a"), &p("x/b")), 0);
        assert_eq!(dir_distance(&p("x/a"), &p("b")), 1);
        assert_eq!(dir_distance(&p("x/y/a"), &p("x/z/b")), 2);
        assert_eq!(dir_distance(&p("x/y/a"), &p("q/b")), 3);
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
            renames in any::<bool>(),
        ) {
            let opts = EntryDiffOptions { case_insensitive: ci, detect_renames: renames };
            let changes = diff_entries(&old, &new, opts);
            check_applies(&old, &new, &changes, ci || renames)?;
            // Rename detection only merges D+A pairs: the counts of the plain diff agree.
            let plain = diff_entries(&old, &new, CS);
            let (c, p) = (count_changes(&changes), count_changes(&plain));
            let n_renames = changes
                .iter()
                .filter(|c| matches!(c.status, ChangeStatus::Renamed { .. }))
                .count() as u32;
            prop_assert_eq!(c.added + n_renames, p.added);
            prop_assert_eq!(c.deleted + n_renames, p.deleted);
            prop_assert_eq!(c.modified, p.modified + n_renames);
        }

        #[test]
        fn diff_of_identical_lists_is_empty(
            old in arb_entries(),
            ci in any::<bool>(),
            renames in any::<bool>(),
        ) {
            let opts = EntryDiffOptions { case_insensitive: ci, detect_renames: renames };
            prop_assert!(diff_entries(&old, &old, opts).is_empty());
        }

        #[test]
        fn rename_detection_ignores_input_order(
            old in arb_entries(),
            new in arb_entries(),
        ) {
            let opts = EntryDiffOptions { case_insensitive: true, detect_renames: true };
            let mut old_rev = old.clone();
            old_rev.reverse();
            let mut new_rev = new.clone();
            new_rev.reverse();
            prop_assert_eq!(
                diff_entries(&old, &new, opts),
                diff_entries(&old_rev, &new_rev, opts)
            );
        }
    }
}
