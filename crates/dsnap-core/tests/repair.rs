//! Blob repair from working files (DSNA-99).
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::fs;

use dsnap_core::retention::{BlobRepair, BlobRepairOutcome};
use dsnap_core::store::hash_bytes;
use dsnap_core::{Dsnap, Error, ProjectId};
use dsnap_test_support::TestHome;

fn setup() -> (TestHome, tempfile::TempDir, Dsnap, common::Cli, ProjectId) {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tempfile::tempdir().unwrap();
    let p = ds.add_project(dir.path(), None).unwrap();
    let cli = common::Cli::open(home.path());
    (home, dir, ds, cli, p.id)
}

fn damage(cli: &common::Cli, content: &[u8]) {
    fs::write(cli.store.path_of(&hash_bytes(content)), b"not a zstd frame").unwrap();
}

#[test]
fn damaged_object_behind_unchanged_file_is_repaired() {
    let (_h, dir, ds, cli, p) = setup();
    let content: &[u8] = b"precious bytes";
    fs::write(dir.path().join("f"), content).unwrap();
    cli.commit(p, &[("f", content)]);
    damage(&cli, content);
    // A later version deduplicates against the damaged object (put trusts it).
    cli.commit(p, &[("f", content), ("g", b"other")]);
    assert!(matches!(
        ds.read_blob(&hash_bytes(content)),
        Err(Error::Corrupt(_))
    ));

    let report = ds.repair_blobs().unwrap();
    let source = dsnap_core::RelPath::new("f")
        .unwrap()
        .to_path(&ds.project(p).unwrap().root);
    assert_eq!(
        report,
        [BlobRepair {
            hash: hash_bytes(content),
            outcome: BlobRepairOutcome::Repaired { source },
        }]
    );
    assert_eq!(ds.read_blob(&hash_bytes(content)).unwrap(), content);
    assert!(ds.repair_blobs().unwrap().is_empty());
}

#[test]
fn changed_working_file_gives_no_intact_source_and_writes_nothing() {
    let (_h, dir, ds, cli, p) = setup();
    let content: &[u8] = b"original";
    let v1 = cli.commit(p, &[("f", content)]);
    let v2 = cli.commit(p, &[("f", content), ("g", b"x")]);
    fs::write(dir.path().join("f"), b"edited since").unwrap();
    damage(&cli, content);
    let object = fs::read(cli.store.path_of(&hash_bytes(content))).unwrap();

    let report = ds.repair_blobs().unwrap();
    assert_eq!(
        report,
        [BlobRepair {
            hash: hash_bytes(content),
            outcome: BlobRepairOutcome::NoIntactSource {
                versions: vec![v2.id, v1.id],
                error: None,
            },
        }]
    );
    assert_eq!(
        fs::read(cli.store.path_of(&hash_bytes(content))).unwrap(),
        object
    );
    assert_eq!(fs::read(dir.path().join("f")).unwrap(), b"edited since");
}

#[test]
fn source_is_found_under_another_path_or_project() {
    let (_h, dir, ds, cli, p) = setup();
    let content: &[u8] = b"copied around";
    // Newest version's file was edited; an older version had it at another path that
    // still holds the content.
    fs::write(dir.path().join("old"), content).unwrap();
    fs::write(dir.path().join("new"), b"edited").unwrap();
    cli.commit(p, &[("old", content)]);
    cli.commit(p, &[("new", content)]);
    damage(&cli, content);
    let report = ds.repair_blobs().unwrap();
    assert!(
        matches!(&report[0].outcome, BlobRepairOutcome::Repaired { source } if source.ends_with("old")),
        "{report:?}"
    );
    assert_eq!(ds.read_blob(&hash_bytes(content)).unwrap(), content);
}

#[test]
fn unreferenced_damaged_blob_is_not_repaired() {
    let (_h, dir, ds, cli, p) = setup();
    let content: &[u8] = b"no longer used";
    fs::write(dir.path().join("f"), content).unwrap();
    let v = cli.commit(p, &[("f", content)]);
    cli.commit(p, &[("g", b"other")]);
    cli.db.delete_version(v.id).unwrap();
    damage(&cli, content);
    let report = ds.repair_blobs().unwrap();
    assert_eq!(report[0].outcome, BlobRepairOutcome::Unreferenced);
    assert!(ds.read_blob(&hash_bytes(content)).is_err());
}

/// DSNA-108: referenced blobs whose object is missing (or empty) are found and repaired too.
#[test]
fn missing_and_empty_objects_are_repaired() {
    let (_h, dir, ds, cli, p) = setup();
    fs::write(dir.path().join("f"), b"f bytes").unwrap();
    fs::write(dir.path().join("g"), b"g bytes").unwrap();
    cli.commit(p, &[("f", b"f bytes"), ("g", b"g bytes")]);
    fs::write(cli.store.path_of(&hash_bytes(b"f bytes")), b"").unwrap();
    fs::remove_file(cli.store.path_of(&hash_bytes(b"g bytes"))).unwrap();

    let report = ds.repair_blobs().unwrap();
    assert_eq!(report.len(), 2, "{report:?}");
    assert!(
        report
            .iter()
            .all(|r| matches!(r.outcome, BlobRepairOutcome::Repaired { .. })),
        "{report:?}"
    );
    assert_eq!(ds.read_blob(&hash_bytes(b"f bytes")).unwrap(), b"f bytes");
    assert_eq!(ds.read_blob(&hash_bytes(b"g bytes")).unwrap(), b"g bytes");
}
