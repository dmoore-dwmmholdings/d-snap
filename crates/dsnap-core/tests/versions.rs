//! Version management (DSNA-54).
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use dsnap_core::{ChangeCounts, Dsnap, Error, ProjectId, VersionId};
use dsnap_test_support::TestHome;

fn counts(added: u32, modified: u32, deleted: u32) -> ChangeCounts {
    ChangeCounts {
        added,
        modified,
        deleted,
    }
}

fn setup() -> (TestHome, tempfile::TempDir, Dsnap, common::Cli, ProjectId) {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tempfile::tempdir().unwrap();
    let p = ds.add_project(dir.path(), None).unwrap();
    let cli = common::Cli::open(home.path());
    (home, dir, ds, cli, p.id)
}

#[test]
fn list_is_newest_first_with_stored_counts() {
    let (_h, _d, ds, cli, p) = setup();
    let v1 = cli.commit(p, &[("a", b"a1"), ("b", b"b1")]);
    let v2 = cli.commit(p, &[("a", b"a2"), ("c", b"c1")]);
    let list = ds.list_versions(p).unwrap();
    assert_eq!(
        list.iter().map(|v| v.id).collect::<Vec<_>>(),
        [v2.id, v1.id]
    );
    assert_eq!(list[1].counts, counts(2, 0, 0));
    assert_eq!(list[0].counts, counts(1, 1, 1));
    assert_eq!(ds.version(v1.id).unwrap(), list[1]);
    assert!(matches!(
        ds.list_versions(ProjectId(999)),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        ds.version(VersionId(999)),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn label_is_validated_and_trimmed() {
    let (_h, _d, ds, cli, p) = setup();
    let v = cli.commit(p, &[("a", b"1")]);
    ds.set_label(v.id, "  before refactor  ").unwrap();
    assert_eq!(ds.version(v.id).unwrap().label, "before refactor");
    for bad in ["", "   ", &"x".repeat(201)] {
        assert!(matches!(
            ds.set_label(v.id, bad),
            Err(Error::InvalidInput(_))
        ));
    }
    ds.set_label(v.id, &"x".repeat(200)).unwrap();
    assert!(matches!(
        ds.set_label(VersionId(999), "x"),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn pin_and_unpin() {
    let (_h, _d, ds, cli, p) = setup();
    let v = cli.commit(p, &[("a", b"1")]);
    ds.set_pinned(v.id, true).unwrap();
    assert!(ds.version(v.id).unwrap().pinned);
    ds.set_pinned(v.id, false).unwrap();
    assert!(!ds.version(v.id).unwrap().pinned);
    assert!(matches!(
        ds.set_pinned(VersionId(999), true),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn deleting_a_middle_version_recomputes_the_successor() {
    let (_h, _d, ds, cli, p) = setup();
    let v1 = cli.commit(p, &[("a", b"a1"), ("b", b"b1")]);
    let v2 = cli.commit(p, &[("a", b"a2"), ("b", b"b1"), ("c", b"c1")]);
    let v3 = cli.commit(p, &[("a", b"a2"), ("c", b"c2"), ("d", b"d1")]);
    assert_eq!(ds.version(v3.id).unwrap().counts, counts(1, 1, 1));

    ds.delete_version(v2.id).unwrap();
    // v3 versus v1: a modified, b deleted, c and d added.
    assert_eq!(ds.version(v3.id).unwrap().counts, counts(2, 1, 1));
    assert_eq!(ds.version(v1.id).unwrap().counts, counts(2, 0, 0));
    assert!(matches!(ds.version(v2.id), Err(Error::NotFound(_))));
    assert!(matches!(ds.delete_version(v2.id), Err(Error::NotFound(_))));
}

#[test]
fn deleting_the_first_version_makes_the_next_all_added() {
    let (_h, _d, ds, cli, p) = setup();
    let v1 = cli.commit(p, &[("a", b"1")]);
    let v2 = cli.commit(p, &[("a", b"2"), ("b", b"1")]);
    ds.delete_version(v1.id).unwrap();
    assert_eq!(ds.version(v2.id).unwrap().counts, counts(2, 0, 0));
}

#[test]
fn deleting_the_latest_makes_the_previous_latest() {
    let (_h, _d, ds, cli, p) = setup();
    let v1 = cli.commit(p, &[("a", b"1")]);
    let v2 = cli.commit(p, &[("a", b"2")]);
    ds.delete_version(v2.id).unwrap();
    assert_eq!(cli.db.latest_version(p).unwrap().unwrap().id, v1.id);
    assert_eq!(ds.list_versions(p).unwrap().len(), 1);
    assert_eq!(ds.version(v1.id).unwrap().counts, counts(1, 0, 0));
    // The next snapshot's fast path compares against v1 again.
    assert_eq!(
        cli.db.latest_entry_index(p).unwrap().len(),
        1,
        "latest entries are v1's"
    );
}

#[test]
fn delete_prunes_blobs_only_that_version_used() {
    let (_h, _d, ds, cli, p) = setup();
    let keep = cli.commit_one(p, b"kept");
    let v2 = cli.commit(p, &[("f", b"gone"), ("g", b"kept")]);
    let gone = dsnap_core::store::hash_bytes(b"gone");
    ds.set_pinned(v2.id, true).unwrap();
    // Pinned and safety versions can still be deleted explicitly.
    ds.delete_version(v2.id).unwrap();
    assert!(ds.read_blob(&gone).is_err());
    assert_eq!(ds.read_blob(&keep).unwrap(), b"kept");
}
