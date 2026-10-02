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
    let v = cli.commit(p, &[("f", b"b")]);
    assert_eq!(ds.after_snapshot(&v).unwrap().versions_deleted, 1);
    assert!(matches!(
        ds.apply_retention(ProjectId(999)),
        Err(Error::NotFound(_))
    ));
}

/// Review round 1 (Rule 1): a restore's safety snapshot must not let retention delete the
/// version being restored. With N unpinned versions and keep = N, restoring to the oldest
/// commits a safety version; `after_snapshot` skips it, and the restore's own retention run
/// protects the target.
#[test]
fn safety_snapshot_never_prunes_the_restore_target() {
    let (_h, _d, ds, cli, p) = setup();
    const N: u32 = 5;
    set_keep(&ds, N);
    let target = cli.commit(p, &[("f", b"target only")]);
    for i in 1..N {
        cli.commit(p, &[("f", format!("v{i}").as_bytes())]);
    }
    let target_blob = hash_bytes(b"target only");

    let safety = cli.commit_kind(p, dsnap_core::VersionKind::Safety, &[("f", b"now")]);
    assert_eq!(
        ds.after_snapshot(&safety).unwrap(),
        RetentionReport::default()
    );
    assert!(ds.version(target.id).is_ok());
    assert_eq!(ds.read_blob(&target_blob).unwrap(), b"target only");

    // Restore finished: its retention run protects the target and the safety version.
    let r = ds
        .apply_retention_protecting(p, &[target.id, safety.id])
        .unwrap();
    assert_eq!(r.versions_deleted, 0);
    assert!(ds.version(target.id).is_ok());
    assert_eq!(ds.read_blob(&target_blob).unwrap(), b"target only");

    // Protection lasts only for that call; a later plain run applies the policy.
    let r = ds.apply_retention(p).unwrap();
    assert_eq!(r.versions_deleted, 1);
    assert!(ds.version(target.id).is_err());
    assert!(ds.read_blob(&target_blob).is_err());
    assert_counts_consistent(&cli, p);
}

/// Protected versions do not count toward N: the newest N others are still kept.
#[test]
fn protected_versions_are_kept_beyond_n() {
    let (_h, _d, ds, cli, p) = setup();
    set_keep(&ds, 2);
    let ids: Vec<_> = (0..5)
        .map(|i| cli.commit(p, &[("f", format!("p{i}").as_bytes())]).id)
        .collect();
    let r = ds.apply_retention_protecting(p, &[ids[0], ids[3]]).unwrap();
    assert_eq!(r.versions_deleted, 1);
    let mut kept: Vec<_> = ds.list_versions(p).unwrap().iter().map(|v| v.id).collect();
    kept.sort();
    assert_eq!(kept, [ids[0], ids[2], ids[3], ids[4]]);
    assert_counts_consistent(&cli, p);
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

/// DSNA-84: a prune running while another handle commits versions that reuse an otherwise
/// unreferenced blob never leaves a committed version without its blob.
#[test]
fn concurrent_prune_never_loses_a_reused_blob() {
    let (home, _d, ds, cli, p) = setup();
    let content: &[u8] = b"reused across versions";
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                // Both sides hammer the write lock in tight loops, so a busy timeout can
                // occur under a loaded test run; it is not what this test checks.
                match ds.prune_blobs() {
                    Ok(_) => {}
                    Err(e) if is_busy(&e) => {}
                    Err(e) => panic!("{e}"),
                }
            }
        });
        let reader = common::Cli::open(home.path());
        let mut retries = 0;
        for _ in 0..150 {
            // Snapshot-style commit: put is a no-op while the file exists (dedupe), and the
            // prune may delete it before insert_version takes the lock.
            let v = loop {
                let info = cli.store.put(content).unwrap();
                match cli.db.insert_version(
                    &dsnap_core::db::NewVersion {
                        project_id: p,
                        label: "v".into(),
                        created_at_ms: 0,
                        kind: dsnap_core::VersionKind::Cli,
                        unstable: false,
                        counts: Default::default(),
                        entries: vec![dsnap_core::Entry {
                            path: dsnap_core::RelPath::new("f").unwrap(),
                            kind: dsnap_core::EntryKind::File,
                            blob: Some(info.hash),
                            size: info.size,
                            mtime_ns: 0,
                            readonly: false,
                        }],
                        new_blobs: vec![info],
                    },
                    &cli.store,
                ) {
                    Ok(v) => break v,
                    Err(Error::BlobMissing(_)) => retries += 1,
                    Err(e) if is_busy(&e) => {}
                    Err(e) => panic!("{e}"),
                }
            };
            assert_eq!(reader.store.get(&hash_bytes(content)).unwrap(), content);
            // Make it unreferenced again for the next round.
            while let Err(e) = cli.db.delete_version(v.id) {
                assert!(is_busy(&e), "{e}");
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("BlobMissing retries: {retries}");
    });
}

fn is_busy(e: &Error) -> bool {
    matches!(e, Error::Db(d) if d.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy))
}

/// Hold `path` so it cannot be deleted until the guard drops.
#[cfg(windows)]
fn hold_undeletable(path: &std::path::Path) -> impl Drop + use<> {
    use std::os::windows::fs::OpenOptionsExt;
    struct Guard(#[allow(dead_code)] std::fs::File);
    impl Drop for Guard {
        fn drop(&mut self) {}
    }
    Guard(
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap(),
    )
}

/// Make the directory holding `path` read-only so the file cannot be unlinked.
#[cfg(unix)]
fn hold_undeletable(path: &std::path::Path) -> impl Drop + use<> {
    use std::os::unix::fs::PermissionsExt;
    struct Guard(std::path::PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let dir = path.parent().unwrap().to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    Guard(dir)
}

/// The prune loop ends with an error once only undeletable blobs remain; retention keeps
/// the bytes already freed and reports the rest as `blobs_failed`.
#[test]
fn undeletable_blob_is_a_warning_not_an_error() {
    let (_h, _d, ds, cli, p) = setup();
    // Two blobs in different shard directories (the unix guard locks a whole shard).
    let a: &[u8] = b"locked";
    let a_hash = hash_bytes(a);
    let b = (0..)
        .map(|i| format!("free {i}"))
        .find(|c| hash_bytes(c.as_bytes()).0[0] != a_hash.0[0])
        .unwrap();
    cli.commit(p, &[("a", a), ("b", b.as_bytes())]);
    cli.commit_one(p, b"latest");
    set_keep(&ds, 1);

    let guard = hold_undeletable(&cli.store.path_of(&a_hash));
    let r = ds.apply_retention(p).unwrap();
    assert_eq!(r.versions_deleted, 1);
    assert_eq!(r.blobs_pruned, 1);
    assert!(r.bytes_freed > 0);
    assert_eq!(r.blobs_failed, 1);
    assert!(ds.read_blob(&hash_bytes(b.as_bytes())).is_err());
    assert_eq!(ds.read_blob(&hash_bytes(b"latest")).unwrap(), b"latest");

    // Only the undeletable blob is left: still a warning.
    let r = ds.prune_blobs().unwrap();
    assert_eq!((r.blobs_pruned, r.blobs_failed), (0, 1));

    drop(guard);
    let r = ds.prune_blobs().unwrap();
    assert_eq!((r.blobs_pruned, r.blobs_failed), (1, 0));
    assert!(ds.read_blob(&a_hash).is_err());
}

/// DSNA-98: only the manual prune sweeps orphan files (no `blobs` row). Retention, version
/// delete and project removal leave them alone.
#[test]
fn only_manual_prune_sweeps_orphans() {
    let (_h, _d, ds, cli, p) = setup();
    set_keep(&ds, 1);
    cli.commit_one(p, b"a");
    let v = cli.commit(p, &[("f", b"b")]);
    cli.commit_one(p, b"c");
    let orphan = cli.store.put(b"orphan").unwrap();

    ds.delete_version(v.id).unwrap();
    ds.apply_retention(p).unwrap();
    let other = tempfile::tempdir().unwrap();
    let q = ds.add_project(other.path(), None).unwrap().id;
    ds.remove_project(q, true).unwrap();
    assert!(cli.store.contains(&orphan.hash));

    let r = ds.prune_blobs().unwrap();
    assert_eq!(r.blobs_pruned, 1);
    assert_eq!(r.bytes_freed, orphan.stored_size);
    assert!(!cli.store.contains(&orphan.hash));
    assert_eq!(ds.read_blob(&hash_bytes(b"c")).unwrap(), b"c");
}

/// DSNA-98 measurement: `Db::sweep_orphans` (via `prune_blobs`) on a 100k-object store.
/// Run with `cargo test -p dsnap-core --release --test retention -- --ignored --nocapture`.
#[test]
#[ignore = "slow; measurement for DSNA-98"]
fn sweep_timing_100k_objects() {
    use std::time::Instant;
    const N: usize = 100_000;
    const ORPHANS: usize = 1_000;
    let (_h, _d, ds, cli, p) = setup();

    let t = Instant::now();
    let mut entries = Vec::with_capacity(N);
    let mut blobs = Vec::with_capacity(N);
    for i in 0..N + ORPHANS {
        // Sweep looks only at file names, so write placeholder objects directly (a real
        // `put` fsyncs each one and takes far longer to set up).
        let content = format!("object {i}");
        let hash = hash_bytes(content.as_bytes());
        let path = cli.store.path_of(&hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"placeholder").unwrap();
        let info = dsnap_core::BlobInfo {
            hash,
            size: content.len() as u64,
            stored_size: 11,
        };
        if i < N {
            blobs.push(info);
            entries.push(dsnap_core::Entry {
                path: dsnap_core::RelPath::new(format!("d{}/f{i}", i % 100)).unwrap(),
                kind: dsnap_core::EntryKind::File,
                blob: Some(info.hash),
                size: info.size,
                mtime_ns: 0,
                readonly: false,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    cli.db
        .insert_version(
            &dsnap_core::db::NewVersion {
                project_id: p,
                label: "big".into(),
                created_at_ms: 0,
                kind: dsnap_core::VersionKind::Cli,
                unstable: false,
                counts: Default::default(),
                entries,
                new_blobs: blobs,
            },
            &cli.store,
        )
        .unwrap();
    eprintln!("setup: {:?}", t.elapsed());

    let t = Instant::now();
    let r = ds.prune_blobs().unwrap();
    eprintln!(
        "prune_blobs with {ORPHANS} orphans: {:?} ({r:?})",
        t.elapsed()
    );
    assert_eq!(r.blobs_pruned as usize, ORPHANS);

    let t = Instant::now();
    let r = ds.prune_blobs().unwrap();
    eprintln!("prune_blobs on a clean store: {:?} ({r:?})", t.elapsed());
    assert_eq!(r.blobs_pruned, 0);
}

/// DSNA-108: one undeletable orphan does not stop the sweep; the rest are removed and the
/// report counts the failure and keeps the bytes freed.
#[test]
fn stuck_orphan_does_not_block_the_sweep() {
    let (_h, _d, ds, cli, _p) = setup();
    // The stuck orphan's shard sorts before the free orphan's, so it is met first.
    let mut contents: Vec<String> = (0..64).map(|i| format!("orphan {i}")).collect();
    contents.sort_by_key(|c| hash_bytes(c.as_bytes()).0[0]);
    let (stuck, free) = (&contents[0], contents.last().unwrap());
    assert!(hash_bytes(stuck.as_bytes()).0[0] < hash_bytes(free.as_bytes()).0[0]);
    let stuck = cli.store.put(stuck.as_bytes()).unwrap();
    let free = cli.store.put(free.as_bytes()).unwrap();

    let guard = hold_undeletable(&cli.store.path_of(&stuck.hash));
    let r = ds.prune_blobs().unwrap();
    assert_eq!(r.blobs_pruned, 1);
    assert_eq!(r.bytes_freed, free.stored_size);
    assert_eq!(r.blobs_failed, 1);
    assert!(!cli.store.contains(&free.hash));
    drop(guard);
    assert_eq!(ds.prune_blobs().unwrap().blobs_pruned, 1);
    assert!(!cli.store.contains(&stuck.hash));
}
