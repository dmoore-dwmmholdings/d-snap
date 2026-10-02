//! Snapshot vs a concurrent prune (DSNA-83, DSNA-80 blob lifetime protocol): a blob the
//! snapshot deduplicated against is pruned before the insert, so the snapshot must store it
//! again and still commit.
#![allow(clippy::unwrap_used)] // helpers outside #[test] fns are test code too

#[path = "support/project.rs"]
mod project;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dsnap_core::db::{Db, PRUNE_BATCH};
use dsnap_core::store::Store;
use dsnap_core::{
    BlobHash, EntryKind, Progress, RetentionReport, SnapshotOptions, SnapshotReport, Stage,
};
use dsnap_test_support::FixtureProject;

use project::{Env, rp, snap};

/// The other process: its own `Db` and `Store` on the same home.
fn other_process(env: &Env) -> (Db, Store) {
    (
        Db::open(&env.home.path().join("dsnap.db")).unwrap(),
        Store::open(env.home.path().join("objects")).unwrap(),
    )
}

/// Snapshot with a hook that runs once, after hashing and before the insert.
fn snap_with_hook(env: &Env, hook: impl Fn() + Send + Sync + 'static) -> SnapshotReport {
    let fired = AtomicBool::new(false);
    let progress = Progress::new(move |e| {
        let last_hash_event = e.stage == Stage::Hash && e.path.is_none();
        if last_hash_event && !fired.swap(true, Ordering::SeqCst) {
            hook();
        }
    });
    env.dsnap
        .snapshot(
            env.project,
            SnapshotOptions {
                progress: Some(progress),
                ..SnapshotOptions::default()
            },
        )
        .unwrap()
}

/// Every blob the latest version references is readable and matches its hash.
fn assert_all_blobs_readable(env: &Env) {
    let latest = env.db.latest_version(env.project).unwrap().unwrap();
    for e in env.db.entries(latest.id).unwrap() {
        if e.kind == EntryKind::File {
            let h = e.blob.unwrap();
            let bytes = env.dsnap.read_blob(&h).unwrap();
            assert_eq!(BlobHash::of(&bytes), h, "{}", e.path);
        }
    }
}

/// v1 holds `a.txt` and `b.txt`; v1 is deleted, so their blobs are unreferenced but still
/// in the store, and the next snapshot deduplicates against them.
fn unreferenced_setup() -> (dsnap_test_support::Fixture, Env) {
    let fx = FixtureProject::new()
        .file("a.txt", "content a")
        .file("b.txt", "content b")
        .build();
    let env = Env::new(fx.root());
    let v1 = snap(&env, None).version.unwrap();
    env.db.delete_version(v1.id).unwrap();
    (fx, env)
}

#[test]
fn prune_between_dedupe_and_insert_is_retried_and_commits() {
    let (_fx, env) = unreferenced_setup();
    let (db, store) = other_process(&env);
    let pruned = Arc::new(Mutex::new(RetentionReport::default()));
    let p = pruned.clone();

    let r = snap_with_hook(&env, move || {
        *p.lock().unwrap() = db.prune_unreferenced(&store, PRUNE_BATCH).unwrap();
    });

    assert_eq!(pruned.lock().unwrap().blobs_pruned, 2, "both blobs pruned");
    let v = r.version.expect("the snapshot still commits");
    assert_eq!(v.counts.added, 2);
    assert!(r.unstable_paths.is_empty());
    assert_all_blobs_readable(&env);
    let a = env.db.entry(v.id, &rp("a.txt")).unwrap().unwrap();
    assert_eq!(env.dsnap.read_blob(&a.blob.unwrap()).unwrap(), b"content a");
    // The re-stored blobs have rows again, so a later prune keeps them.
    let (db, store) = other_process(&env);
    assert_eq!(
        db.prune_unreferenced(&store, PRUNE_BATCH)
            .unwrap()
            .blobs_pruned,
        0
    );
    assert_all_blobs_readable(&env);
}

#[test]
fn file_changed_before_the_retry_is_recaptured() {
    let (fx, env) = unreferenced_setup();
    let (db, store) = other_process(&env);
    let a: PathBuf = fx.path("a.txt");
    let b: PathBuf = fx.path("b.txt");

    let r = snap_with_hook(&env, move || {
        db.prune_unreferenced(&store, PRUNE_BATCH).unwrap();
        project::rewrite(&a, b"content a, edited before the retry");
        std::fs::remove_file(&b).unwrap();
    });

    let v = r.version.unwrap();
    assert_eq!(v.counts.added, 1, "b.txt is gone before the retry");
    let entries = env.db.entries(v.id).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        env.dsnap.read_blob(&entries[0].blob.unwrap()).unwrap(),
        b"content a, edited before the retry"
    );
    assert_eq!(entries[0].size, 34);
    assert_all_blobs_readable(&env);
}

/// Keeps `path` unreadable until dropped: an exclusive open on Windows, mode 000 on unix.
/// `None` when that does not stop reads (unix as root).
struct Unreadable {
    #[cfg(windows)]
    _guard: std::fs::File,
    #[cfg(unix)]
    path: PathBuf,
}

impl Unreadable {
    fn hold(path: PathBuf) -> Option<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let guard = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap();
            Some(Self { _guard: guard })
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let held = Self { path };
            std::fs::read(&held.path).is_err().then_some(held)
        }
    }
}

#[cfg(unix)]
impl Drop for Unreadable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o644));
    }
}

/// DSNA-104: when the retry fails too, the error is returned and nothing is committed.
///
/// The unreadable file is carried forward with v1's blob. The hook deletes v1 and prunes that
/// blob, the recapture skips the file again (still unreadable), so the retry inserts the same
/// missing blob and fails.
#[test]
fn retry_double_failure_commits_nothing() {
    let fx = FixtureProject::new()
        .file("locked.txt", "v1")
        .file("other.txt", "o1")
        .build();
    let env = Env::new(fx.root());
    let v1 = snap(&env, None).version.unwrap();
    project::rewrite(&fx.path("locked.txt"), b"v2 changed");
    project::rewrite(&fx.path("other.txt"), b"o2 changed");
    let Some(held) = Unreadable::hold(fx.path("locked.txt")) else {
        eprintln!("cannot make a file unreadable here (root?); skipping");
        return;
    };

    let (db, store) = other_process(&env);
    let pruned = Arc::new(Mutex::new(RetentionReport::default()));
    let p = pruned.clone();
    let fired = AtomicBool::new(false);
    let progress = Progress::new(move |e| {
        if e.stage == Stage::Hash && e.path.is_none() && !fired.swap(true, Ordering::SeqCst) {
            db.delete_version(v1.id).unwrap();
            *p.lock().unwrap() = db.prune_unreferenced(&store, PRUNE_BATCH).unwrap();
        }
    });
    let err = env
        .dsnap
        .snapshot(
            env.project,
            SnapshotOptions {
                progress: Some(progress),
                ..SnapshotOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(pruned.lock().unwrap().blobs_pruned, 2);
    assert!(matches!(err, dsnap_core::Error::BlobMissing(_)), "{err}");
    assert_eq!(project::versions(&env), 0);

    drop(held);
    let v = snap(&env, None).version.unwrap();
    assert_eq!(v.counts.added, 2);
    assert_all_blobs_readable(&env);
}
