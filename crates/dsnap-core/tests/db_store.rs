//! Index database against the real blob store: the DSNA-80 blob lifetime protocol end to end.
#![allow(clippy::unwrap_used)] // helpers outside #[test] fns are test code too

use std::path::Path;

use dsnap_core::db::{Db, NewVersion, PRUNE_BATCH};
use dsnap_core::store::Store;
use dsnap_core::{
    ChangeCounts, Entry, EntryKind, Error, ProjectId, ProjectSettings, RelPath, VersionKind,
};
use dsnap_test_support::TestHome;

fn open(home: &Path) -> (Db, Store) {
    let db = Db::open(&home.join("dsnap.db")).unwrap();
    let store = Store::open(home.join("objects")).unwrap();
    (db, store)
}

fn version(project: ProjectId, path: &str, info: dsnap_core::BlobInfo, new: bool) -> NewVersion {
    NewVersion {
        project_id: project,
        label: path.into(),
        created_at_ms: 0,
        kind: VersionKind::Cli,
        unstable: false,
        counts: ChangeCounts::default(),
        entries: vec![Entry {
            path: RelPath::new(path).unwrap(),
            kind: EntryKind::File,
            blob: Some(info.hash),
            size: info.size,
            mtime_ns: 0,
            readonly: false,
        }],
        new_blobs: if new { vec![info] } else { vec![] },
    }
}

#[test]
fn prune_then_deduped_insert_is_blob_missing_and_retry_succeeds() {
    let home = TestHome::new();
    let (cli, cli_store) = open(home.path());
    let (app, app_store) = open(home.path());
    let p = cli
        .insert_project("p", &home.path().join("proj"), &ProjectSettings::default())
        .unwrap();

    // X was used by a version that has since been deleted.
    let x = cli_store.put(b"content x").unwrap();
    let old = cli
        .insert_version(&version(p, "old", x, true), &cli_store)
        .unwrap();
    cli.delete_version(old.id).unwrap();

    // CLI snapshot dedupes against X (the file exists, put is a no-op) ...
    assert!(cli_store.contains(&x.hash));
    let nv = version(p, "f", x, false);
    // ... but the app prunes X before the CLI commits.
    let r = app.prune_unreferenced(&app_store, PRUNE_BATCH).unwrap();
    assert_eq!(r.blobs_pruned, 1);
    assert!(!cli_store.contains(&x.hash));

    let err = cli.insert_version(&nv, &cli_store).unwrap_err();
    assert!(matches!(err, Error::BlobMissing(h) if h == x.hash), "{err}");
    assert!(cli.list_versions(p).unwrap().is_empty());

    // Retry: re-store and insert again.
    let x2 = cli_store.put(b"content x").unwrap();
    cli.insert_version(&version(p, "f", x2, true), &cli_store)
        .unwrap();
    assert_eq!(cli_store.get(&x.hash).unwrap(), b"content x");
    assert_eq!(
        app.prune_unreferenced(&app_store, PRUNE_BATCH)
            .unwrap()
            .blobs_pruned,
        0
    );
    assert!(app_store.contains(&x.hash));
}

#[test]
fn sweep_removes_orphan_files_and_uncommitted_blobs_are_rechecked() {
    let home = TestHome::new();
    let (cli, cli_store) = open(home.path());
    let (app, app_store) = open(home.path());
    let p = cli
        .insert_project("p", &home.path().join("proj"), &ProjectSettings::default())
        .unwrap();

    let kept = cli_store.put(b"kept").unwrap();
    cli.insert_version(&version(p, "k", kept, true), &cli_store)
        .unwrap();
    let orphan = cli_store.put(b"orphan from a crashed snapshot").unwrap();
    // A snapshot in flight: stored, not committed yet.
    let pending = cli_store.put(b"pending").unwrap();

    let r = app.sweep_orphans(&app_store).unwrap();
    assert_eq!(r.blobs_pruned, 2);
    assert!(r.bytes_freed > 0);
    assert!(app_store.contains(&kept.hash));
    assert!(!app_store.contains(&orphan.hash));
    assert!(!app_store.contains(&pending.hash));

    let err = cli
        .insert_version(&version(p, "pending", pending, true), &cli_store)
        .unwrap_err();
    assert!(
        matches!(err, Error::BlobMissing(h) if h == pending.hash),
        "{err}"
    );
}
