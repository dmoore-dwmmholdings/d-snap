//! Retention and blob pruning (DSNA-55, DSNA-84, DSNA-98).
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use dsnap_core::store::hash_bytes;
use dsnap_core::versions::version_counts;
use dsnap_core::{Dsnap, Error, GlobalSettings, ProjectId, RetentionReport};
use dsnap_test_support::TestHome;

fn setup() -> (TestHome, tempfile::TempDir, Dsnap, common::Cli, ProjectId) {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tempfile::tempdir().unwrap();
    let p = ds.add_project(dir.path(), None).unwrap();
    let cli = common::Cli::open(home.path());
    (home, dir, ds, cli, p.id)
}

fn set_keep(ds: &Dsnap, keep: u32) {
    ds.set_global_settings(&GlobalSettings {
        retention_keep: keep,
        ..GlobalSettings::default()
    })
    .unwrap();
}

/// Every stored count equals a recount against the version's current predecessor.
fn assert_counts_consistent(cli: &common::Cli, p: ProjectId) {
    let mut versions = cli.db.list_versions(p).unwrap();
    versions.reverse();
    let mut prev = Vec::new();
    for v in versions {
        let entries = cli.db.entries(v.id).unwrap();
        assert_eq!(
            v.counts,
            version_counts(&prev, &entries),
            "version {}",
            v.id
        );
        prev = entries;
    }
}

#[test]
fn keeps_pinned_plus_newest_n() {
    let (_h, _d, ds, cli, p) = setup();
    let contents: Vec<String> = (0..250).map(|i| format!("content {i}")).collect();
    let ids: Vec<_> = contents
        .iter()
        .map(|c| {
            cli.commit(p, &[("f", c.as_bytes()), ("same", b"shared")])
                .id
        })
        .collect();
    for i in [0, 9, 99] {
        ds.set_pinned(ids[i], true).unwrap();
    }

    let r = ds.apply_retention(p).unwrap();
    assert_eq!(r.versions_deleted, 47);
    // Each deleted unpinned version had one blob of its own; "shared" stays referenced.
    assert_eq!(r.blobs_pruned, 47);
    assert!(r.bytes_freed > 0);
    assert_eq!(r.blobs_failed, 0);

    let kept: Vec<_> = ds.list_versions(p).unwrap().iter().map(|v| v.id).collect();
    assert_eq!(kept.len(), 203);
    for (i, id) in ids.iter().enumerate() {
        // Index 99 is pinned inside the newest 200, so the unpinned window reaches back to 49.
        let should_keep = i >= 49 || [0, 9].contains(&i);
        assert_eq!(kept.contains(id), should_keep, "version index {i}");
        let blob = hash_bytes(contents[i].as_bytes());
        assert_eq!(
            ds.read_blob(&blob).is_ok(),
            should_keep,
            "blob of index {i}"
        );
    }
    assert_eq!(ds.read_blob(&hash_bytes(b"shared")).unwrap(), b"shared");
    assert_counts_consistent(&cli, p);

    // Nothing more to do.
    assert_eq!(ds.apply_retention(p).unwrap(), RetentionReport::default());
}

#[test]
fn blob_of_a_kept_version_survives() {
    let (_h, _d, ds, cli, p) = setup();
    set_keep(&ds, 1);
    let old_only = cli.commit_one(p, b"old only");
    cli.commit(p, &[("f", b"both"), ("g", b"old too")]);
    cli.commit(p, &[("f", b"both"), ("h", b"new")]);
    let r = ds.apply_retention(p).unwrap();
    assert_eq!(r.versions_deleted, 2);
    assert_eq!(r.blobs_pruned, 2);
    assert!(ds.read_blob(&old_only).is_err());
    assert!(ds.read_blob(&hash_bytes(b"old too")).is_err());
    assert_eq!(ds.read_blob(&hash_bytes(b"both")).unwrap(), b"both");
    assert_eq!(ds.read_blob(&hash_bytes(b"new")).unwrap(), b"new");
    assert_counts_consistent(&cli, p);
}

#[test]
fn pinned_versions_do_not_count_toward_n() {
    let (_h, _d, ds, cli, p) = setup();
    set_keep(&ds, 2);
    let ids: Vec<_> = (0..6)
        .map(|i| cli.commit(p, &[("f", format!("{i}").as_bytes())]).id)
        .collect();
    ds.set_pinned(ids[5], true).unwrap();
    ds.set_pinned(ids[1], true).unwrap();
    let r = ds.apply_retention(p).unwrap();
    assert_eq!(r.versions_deleted, 2);
    let mut kept: Vec<_> = ds.list_versions(p).unwrap().iter().map(|v| v.id).collect();
    kept.sort();
    assert_eq!(kept, [ids[1], ids[3], ids[4], ids[5]]);
    assert_counts_consistent(&cli, p);
}

#[test]
fn retention_is_per_project() {
    let (_h, _d, ds, cli, p) = setup();
    set_keep(&ds, 1);
    let other = tempfile::tempdir().unwrap();
    let q = ds.add_project(other.path(), None).unwrap().id;
    cli.commit_one(p, b"p1");
    cli.commit_one(p, b"p2");
    cli.commit_one(q, b"q1");
    cli.commit_one(q, b"q2");
    assert_eq!(ds.apply_retention(p).unwrap().versions_deleted, 1);
    assert_eq!(ds.list_versions(q).unwrap().len(), 2);
    assert_eq!(ds.read_blob(&hash_bytes(b"q1")).unwrap(), b"q1");
}

#[test]
fn after_snapshot_hook_applies_retention() {
    let (_h, _d, ds, cli, p) = setup();
    set_keep(&ds, 1);
    cli.commit_one(p, b"a");
    cli.commit_one(p, b"b");
    assert_eq!(ds.after_snapshot(p).unwrap().versions_deleted, 1);
    assert!(matches!(
        ds.apply_retention(ProjectId(999)),
        Err(Error::NotFound(_))
    ));
}

/// A blob an in-flight snapshot has stored but not yet committed (no `blobs` row) is not
/// touched by retention. (DSNA-55 asked for a 10-minute mtime guard; the DSNA-80 protocol
/// replaces it: retention never deletes a blob file that has no row.)
#[test]
fn uncommitted_object_survives_retention() {
    let (_h, _d, ds, cli, p) = setup();
    set_keep(&ds, 1);
    cli.commit_one(p, b"a");
    cli.commit_one(p, b"b");
    let in_flight = cli.store.put(b"in flight").unwrap();
    let r = ds.apply_retention(p).unwrap();
    assert_eq!(r.versions_deleted, 1);
    assert_eq!(r.blobs_pruned, 1);
    assert_eq!(ds.read_blob(&in_flight.hash).unwrap(), b"in flight");
}
