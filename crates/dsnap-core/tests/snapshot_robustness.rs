//! Snapshot robustness (DSNA-50): files changing mid-hash, locked and unreadable files, crash
//! safety.
#![allow(clippy::unwrap_used)] // helpers outside #[test] fns are test code too

#[path = "support/project.rs"]
mod project;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use dsnap_core::{EntryKind, Error, SnapshotOptions};
use dsnap_test_support::FixtureProject;

use project::{Env, rp, snap, versions};

/// Rows in an index table, read with a separate SQLite connection.
fn rows(env: &Env, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(env.home.path().join("dsnap.db")).unwrap();
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn concurrent_writer_never_breaks_a_snapshot() {
    let fx = FixtureProject::new()
        .file("hot.bin", vec![0u8; 1 << 20])
        .file("calm.txt", "calm")
        .build();
    let env = Env::new(fx.root());
    let stop = Arc::new(AtomicBool::new(false));
    let hot = fx.path("hot.bin");
    let writer = {
        let stop = stop.clone();
        thread::spawn(move || {
            let mut n = 0u8;
            while !stop.load(Ordering::Relaxed) {
                n = n.wrapping_add(1);
                // Size changes too, so every rewrite is visible to the size/mtime check.
                let len = (1 << 20) + usize::from(n);
                let _ = fs::write(&hot, vec![n; len]);
            }
        })
    };
    let mut saw_unstable = false;
    for _ in 0..5 {
        let r = snap(&env, None);
        saw_unstable |= !r.unstable_paths.is_empty();
        if let Some(v) = &r.version {
            assert_eq!(v.unstable, !r.unstable_paths.is_empty());
            assert!(r.unstable_paths.iter().all(|p| *p == rp("hot.bin")));
        }
        // Every entry of the latest version matches its stored blob.
        let latest = env.db.latest_version(env.project).unwrap().unwrap();
        for e in env.db.entries(latest.id).unwrap() {
            if e.kind == EntryKind::File {
                let bytes = env.dsnap.read_blob(&e.blob.unwrap()).unwrap();
                assert_eq!(bytes.len() as u64, e.size, "{}", e.path);
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
    eprintln!("unstable seen: {saw_unstable}");

    // Once the writer stops, the next snapshot is stable and captures the final bytes.
    let r = snap(&env, None);
    assert!(r.unstable_paths.is_empty());
    let latest = env.db.latest_version(env.project).unwrap().unwrap();
    let hot = env
        .db
        .entry(latest.id, &rp("hot.bin"))
        .unwrap()
        .unwrap()
        .blob
        .unwrap();
    assert_eq!(
        env.dsnap.read_blob(&hot).unwrap(),
        fs::read(fx.path("hot.bin")).unwrap()
    );
}

#[cfg(windows)]
#[test]
fn locked_file_is_skipped_and_its_previous_entry_carried_forward() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::Instant;

    use dsnap_core::{ChangeCounts, SkipReason};

    let fx = FixtureProject::new()
        .file("locked.txt", "v1")
        .file("other.txt", "o1")
        .build();
    let env = Env::new(fx.root());
    let v1 = snap(&env, None).version.unwrap();
    let before = env.db.entry(v1.id, &rp("locked.txt")).unwrap().unwrap();

    // Change both files (so the fast path does not apply), then hold one open exclusively.
    project::rewrite(&fx.path("locked.txt"), b"v2 changed");
    project::rewrite(&fx.path("other.txt"), b"o2 changed");
    let guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fx.path("locked.txt"))
        .unwrap();

    let t = Instant::now();
    let r = snap(&env, None);
    let took = t.elapsed();
    drop(guard);

    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].path, "locked.txt");
    assert_eq!(r.skipped[0].reason, SkipReason::Locked);
    assert!(took.as_millis() >= 450, "retried for {took:?}");
    let v2 = r.version.unwrap();
    assert_eq!(
        v2.counts,
        ChangeCounts {
            added: 0,
            modified: 1,
            deleted: 0
        },
        "the locked file must not show as deleted"
    );
    let carried = env.db.entry(v2.id, &rp("locked.txt")).unwrap().unwrap();
    assert_eq!(carried, before);

    // Unlocked again: the next snapshot captures it.
    let v3 = snap(&env, None).version.unwrap();
    let now = env.db.entry(v3.id, &rp("locked.txt")).unwrap().unwrap();
    assert_eq!(
        env.dsnap.read_blob(&now.blob.unwrap()).unwrap(),
        b"v2 changed"
    );
}

/// On unix a file or directory without read permission is unreadable (unless running as root).
#[cfg(unix)]
#[test]
fn unreadable_file_and_directory_are_carried_forward() {
    use std::os::unix::fs::PermissionsExt;

    use dsnap_core::SkipReason;

    let fx = FixtureProject::new()
        .file("secret.txt", "s1")
        .file("closed/inner.txt", "i1")
        .file("open.txt", "o1")
        .build();
    let env = Env::new(fx.root());
    let v1 = snap(&env, None).version.unwrap();

    project::rewrite(&fx.path("secret.txt"), b"s2 changed");
    project::rewrite(&fx.path("open.txt"), b"o2 changed");
    let set_mode = |p: &str, mode: u32| {
        fs::set_permissions(fx.path(p), fs::Permissions::from_mode(mode)).unwrap()
    };
    set_mode("secret.txt", 0o000);
    set_mode("closed", 0o000);
    if fs::read(fx.path("secret.txt")).is_ok() {
        set_mode("closed", 0o755);
        eprintln!("running as root; permissions are not enforced; skipping");
        return;
    }

    let r = snap(&env, None);
    set_mode("closed", 0o755);
    set_mode("secret.txt", 0o644);

    let skipped: Vec<&str> = r.skipped.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(skipped, ["closed", "secret.txt"]);
    assert!(
        r.skipped
            .iter()
            .all(|s| matches!(s.reason, SkipReason::Unreadable { .. }))
    );
    let v2 = r.version.unwrap();
    assert_eq!(v2.counts.deleted, 0);
    assert_eq!(v2.counts.modified, 1);
    for p in ["secret.txt", "closed/inner.txt"] {
        assert_eq!(
            env.db.entry(v2.id, &rp(p)).unwrap(),
            env.db.entry(v1.id, &rp(p)).unwrap(),
            "{p}"
        );
    }
}

#[test]
fn failure_inside_the_transaction_leaves_no_partial_version() {
    let fx = FixtureProject::new()
        .file("a.txt", "a")
        .file("b.txt", "b")
        .file("c.txt", "c")
        .build();
    let env = Env::new(fx.root());

    // Abort the insert after the version row and some entries are written.
    let conn = rusqlite::Connection::open(env.home.path().join("dsnap.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER inject_crash BEFORE INSERT ON entries WHEN NEW.path = 'b.txt'
         BEGIN SELECT RAISE(ABORT, 'injected crash'); END;",
    )
    .unwrap();

    let err = env
        .dsnap
        .snapshot(env.project, SnapshotOptions::default())
        .unwrap_err();
    // RAISE(ABORT) is a constraint error, which `Db` reports as a duplicate path.
    assert!(
        matches!(err, Error::Db(_) | Error::InvalidInput(_)),
        "{err}"
    );
    assert_eq!(versions(&env), 0);
    assert_eq!(rows(&env, "versions"), 0);
    assert_eq!(rows(&env, "entries"), 0);
    assert_eq!(rows(&env, "blobs"), 0);

    // The blobs were written before the transaction: they are orphans, and the sweep
    // removes them.
    assert_eq!(project::store_files(&env), 3);
    let swept = env.db.sweep_orphans(&store(&env)).unwrap();
    assert_eq!(swept.blobs_pruned, 3);
    assert_eq!(project::store_files(&env), 0);

    conn.execute_batch("DROP TRIGGER inject_crash;").unwrap();
    let v = snap(&env, None).version.unwrap();
    assert_eq!(v.counts.added, 3);
}

fn store(env: &Env) -> dsnap_core::store::Store {
    dsnap_core::store::Store::open(env.home.path().join("objects")).unwrap()
}

/// DSNA-100: another process holding the write lock past the busy timeout gives the
/// retryable `Error::Busy`, not a raw database error, and nothing is committed.
#[test]
fn write_lock_held_past_busy_timeout_is_busy() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    let conn = rusqlite::Connection::open(env.home.path().join("dsnap.db")).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();

    let t = std::time::Instant::now();
    let err = env
        .dsnap
        .snapshot(env.project, SnapshotOptions::default())
        .unwrap_err();
    assert!(matches!(err, Error::Busy), "{err}");
    assert!(err.to_string().contains("try again"), "{err}");
    assert!(t.elapsed().as_secs_f64() >= 4.5, "waited {:?}", t.elapsed());

    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(versions(&env), 0);
    assert!(snap(&env, None).version.is_some());
}
