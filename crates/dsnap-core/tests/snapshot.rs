//! `Dsnap::snapshot` end to end (DSNA-49): capture, fast path, F7, counts, ignore rules,
//! size cap, cancel.
#![allow(clippy::unwrap_used)] // helpers outside #[test] fns are test code too

#[path = "support/project.rs"]
mod project;

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};

use dsnap_core::db::Db;
use dsnap_core::{
    CancelToken, ChangeCounts, Dsnap, EntryKind, Error, GlobalSettings, Progress, ProjectSettings,
    RelPath, SkipReason, SnapshotOptions, Stage, VersionKind,
};
use dsnap_test_support::{FixtureProject, TestHome, tree_bytes, tree_dirs};

use project::{Env, rp, snap, store_files, versions};

/// Every file of the latest version, read back from the store.
fn stored_bytes(env: &Env) -> BTreeMap<RelPath, Vec<u8>> {
    let latest = env.db.latest_version(env.project).unwrap().unwrap();
    env.db
        .entries(latest.id)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            EntryKind::File => Some((e.path, env.dsnap.read_blob(&e.blob.unwrap()).unwrap())),
            EntryKind::Symlink { target } => {
                let mut v = b"symlink:".to_vec();
                v.extend_from_slice(target.as_bytes());
                Some((e.path, v))
            }
            EntryKind::Dir => None,
        })
        .collect()
}

#[test]
fn first_snapshot_captures_every_file_byte_for_byte() {
    let fx = FixtureProject::new()
        .file("a.txt", "hello\n")
        .file("src/main.rs", "fn main() {}\n")
        .file("bin/data.bin", [0u8, 1, 2, 255, 0, 7])
        .file("empty.txt", "")
        .file("deep/er/still/x", "x")
        .dir("empty_dir")
        .readonly("a.txt")
        .build();
    let env = Env::new(fx.root());
    let r = snap(&env, Some("first"));
    let v = r.version.unwrap();
    assert_eq!(v.label, "first");
    assert_eq!(v.kind, VersionKind::Manual);
    assert!(!v.unstable);
    assert_eq!(
        v.counts,
        ChangeCounts {
            added: 6,
            modified: 0,
            deleted: 0
        }
    );
    assert!(r.skipped.is_empty());
    assert_eq!(stored_bytes(&env), tree_bytes(fx.root()));

    let entries = env.db.entries(v.id).unwrap();
    let ro = entries.iter().find(|e| e.path == rp("a.txt")).unwrap();
    assert!(ro.readonly);
    let dir = entries.iter().find(|e| e.path == rp("empty_dir")).unwrap();
    assert_eq!(dir.kind, EntryKind::Dir);
    assert!(tree_dirs(fx.root()).contains(&rp("empty_dir")));
}

#[test]
fn unchanged_second_snapshot_writes_nothing() {
    let fx = FixtureProject::new().file("a.txt", "1").build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    let token = env.db.change_token().unwrap();
    let blobs = project::store_files(&env);

    let r = snap(&env, None);
    assert!(r.version.is_none());
    assert_eq!(versions(&env), 1);
    assert_eq!(env.db.change_token().unwrap(), token);
    assert_eq!(project::store_files(&env), blobs);
}

#[test]
fn mtime_only_change_is_not_a_change() {
    let fx = FixtureProject::new().file("a.txt", "same").build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    filetime::set_file_mtime(
        fx.path("a.txt"),
        filetime::FileTime::from_system_time(later),
    )
    .unwrap();
    assert!(snap(&env, None).version.is_none());
}

#[test]
fn counts_added_modified_deleted() {
    let fx = FixtureProject::new()
        .file("keep.txt", "k")
        .file("edit.txt", "before")
        .file("gone.txt", "bye")
        .file("gone2.txt", "bye2")
        .build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();

    project::rewrite(&fx.path("edit.txt"), b"after, longer");
    fs::remove_file(fx.path("gone.txt")).unwrap();
    fs::remove_file(fx.path("gone2.txt")).unwrap();
    fs::write(fx.path("new.txt"), "fresh").unwrap();
    fs::create_dir(fx.path("newdir")).unwrap();

    let v = snap(&env, Some("second")).version.unwrap();
    assert_eq!(
        v.counts,
        ChangeCounts {
            added: 2,
            modified: 1,
            deleted: 2
        }
    );
    assert_eq!(stored_bytes(&env), tree_bytes(fx.root()));
    assert_eq!(versions(&env), 2);
}

#[test]
fn ignored_and_oversize_files_are_excluded_and_oversize_reported() {
    let fx = FixtureProject::new()
        .file("a.txt", "a")
        .file("debug.log", "ignored by .gitignore")
        .file("node_modules/pkg/index.js", "built-in ignore")
        .file("secret/key", "extra_ignore")
        .file("big.bin", vec![7u8; 2048])
        .gitignore(&["*.log"])
        .build();
    let env = Env::with_settings(
        fx.root(),
        ProjectSettings {
            extra_ignore: vec!["secret/".into()],
            ..ProjectSettings::default()
        },
    );
    env.db
        .set_global_settings(&GlobalSettings {
            size_cap_bytes: 1024,
            ..GlobalSettings::default()
        })
        .unwrap();

    let r = snap(&env, None);
    let v = r.version.unwrap();
    let paths: Vec<String> = env
        .db
        .entries(v.id)
        .unwrap()
        .into_iter()
        .map(|e| e.path.to_string())
        .collect();
    assert_eq!(paths, [".gitignore", "a.txt"]);
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].path, "big.bin");
    assert_eq!(r.skipped[0].reason, SkipReason::TooLarge { size: 2048 });
}

#[test]
fn cancel_before_commit_leaves_no_version() {
    let fx = FixtureProject::new()
        .file("a.txt", "a")
        .file("b.txt", "b")
        .build();
    let env = Env::new(fx.root());

    // Already cancelled.
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = env
        .dsnap
        .snapshot(
            env.project,
            SnapshotOptions {
                cancel: Some(cancel),
                ..SnapshotOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert_eq!(versions(&env), 0);

    // Cancelled during the hash phase, after blobs were written.
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let progress = Progress::new(move |e| {
        if e.stage == Stage::Hash {
            c.cancel();
        }
    });
    let err = env
        .dsnap
        .snapshot(
            env.project,
            SnapshotOptions {
                cancel: Some(cancel),
                progress: Some(progress),
                ..SnapshotOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert_eq!(versions(&env), 0);
    // The last hash event comes after the batch was committed: its blobs stay in the store
    // unreferenced (removed by the next prune), but no temp file is left behind.
    assert_eq!(temp_files(&env), 0);

    // A later snapshot still works.
    assert!(snap(&env, None).version.is_some());
}

#[test]
fn progress_reports_walk_and_hash() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    let stages = Arc::new(Mutex::new(Vec::new()));
    let s = stages.clone();
    env.dsnap
        .snapshot(
            env.project,
            SnapshotOptions {
                progress: Some(Progress::new(move |e| s.lock().unwrap().push(e.stage))),
                kind: VersionKind::Cli,
                ..SnapshotOptions::default()
            },
        )
        .unwrap();
    let stages = stages.lock().unwrap();
    assert!(stages.contains(&Stage::Walk) && stages.contains(&Stage::Hash));
    let latest = env.db.latest_version(env.project).unwrap().unwrap();
    assert_eq!(latest.kind, VersionKind::Cli);
}

#[test]
fn default_label_is_a_timestamp() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    let v = snap(&env, Some("  ")).version.unwrap();
    // YYYY-MM-DD HH:MM:SS
    let b = v.label.as_bytes();
    assert_eq!(b.len(), 19, "{}", v.label);
    assert_eq!(
        (b[4], b[7], b[10], b[13], b[16]),
        (b'-', b'-', b' ', b':', b':')
    );
    assert!(v.created_at_ms > 0);
}

#[test]
fn first_snapshot_of_an_empty_folder_is_a_baseline() {
    let fx = FixtureProject::new().build();
    let env = Env::new(fx.root());
    let v = snap(&env, None).version.unwrap();
    assert_eq!(v.counts, ChangeCounts::default());
    assert!(snap(&env, None).version.is_none());
}

#[test]
fn missing_root_is_project_missing() {
    let home = TestHome::new();
    let dsnap = home.open();
    let db = Db::open(&home.path().join("dsnap.db")).unwrap();
    let root = home.path().join("gone");
    let p = db
        .insert_project("gone", &root, &ProjectSettings::default())
        .unwrap();
    let err = dsnap.snapshot(p, SnapshotOptions::default()).unwrap_err();
    assert!(matches!(err, Error::ProjectMissing { .. }), "{err}");
    let _: &Dsnap = &dsnap;
}

#[test]
fn symlinks_are_stored_as_links() {
    if !dsnap_test_support::symlinks_supported() {
        eprintln!("symlinks unsupported here; skipping");
        return;
    }
    let fx = FixtureProject::new()
        .file("target.txt", "t")
        .symlink("link", "target.txt")
        .build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    assert_eq!(stored_bytes(&env), tree_bytes(fx.root()));
}

/// Fast path (DSNA-49 review): same size, later mtime must be re-read. Every other modify
/// test also changes the size, so only this one depends on the mtime check.
#[test]
fn same_size_edit_with_later_mtime_is_captured() {
    let fx = FixtureProject::new().file("a.txt", "abc").build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();

    project::rewrite(&fx.path("a.txt"), b"xyz");
    let st = env.dsnap.status(env.project).unwrap();
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].path, rp("a.txt"));
    assert_eq!(st[0].status, dsnap_core::ChangeStatus::Modified);

    let v = snap(&env, None).version.unwrap();
    assert_eq!(
        v.counts,
        ChangeCounts {
            added: 0,
            modified: 1,
            deleted: 0
        }
    );
    let e = env.db.entry(v.id, &rp("a.txt")).unwrap().unwrap();
    assert_eq!(env.dsnap.read_blob(&e.blob.unwrap()).unwrap(), b"xyz");
}

/// DSNA-103: a same-size edit that keeps the recorded mtime (same timestamp tick as the
/// capture) is still seen, because an entry whose mtime is not older than the capture start
/// is racily clean and re-read.
#[test]
fn racily_clean_same_size_edit_with_same_mtime_is_captured() {
    let fx = FixtureProject::new().file("a.txt", "abc").build();
    let env = Env::new(fx.root());
    // An mtime at (or after) the capture start, as when a file is written in the same tick.
    let tick = filetime::FileTime::from_system_time(
        std::time::SystemTime::now() + std::time::Duration::from_millis(500),
    );
    filetime::set_file_mtime(fx.path("a.txt"), tick).unwrap();
    let v1 = snap(&env, None).version.unwrap();
    let recorded = env.db.entry(v1.id, &rp("a.txt")).unwrap().unwrap();

    fs::write(fx.path("a.txt"), "xyz").unwrap();
    filetime::set_file_mtime(fx.path("a.txt"), tick).unwrap();
    let now = env.dsnap.working_entries(env.project).unwrap();
    assert_eq!(now[0].mtime_ns, recorded.mtime_ns, "same size and mtime");
    assert_eq!(now[0].size, recorded.size);

    assert_eq!(env.dsnap.status(env.project).unwrap().len(), 1);
    let v2 = snap(&env, None).version.unwrap();
    assert_eq!(v2.counts.modified, 1);
    let e = env.db.entry(v2.id, &rp("a.txt")).unwrap().unwrap();
    assert_eq!(env.dsnap.read_blob(&e.blob.unwrap()).unwrap(), b"xyz");
}

/// DSNA-105: labels are trimmed and cut to `MAX_LABEL_CHARS`.
#[test]
fn labels_are_trimmed_and_capped() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    assert_eq!(
        snap(&env, Some("  after agent turn \n"))
            .version
            .unwrap()
            .label,
        "after agent turn"
    );
    fs::write(fx.path("b.txt"), "b").unwrap();
    let long = "x".repeat(dsnap_core::snapshot::MAX_LABEL_CHARS + 10);
    let v = snap(&env, Some(&long)).version.unwrap();
    assert_eq!(v.label.len(), dsnap_core::snapshot::MAX_LABEL_CHARS);
}

/// Keep only `keep` unpinned versions.
fn set_keep(env: &Env, keep: u32) {
    env.dsnap
        .set_global_settings(&GlobalSettings {
            retention_keep: keep,
            ..GlobalSettings::default()
        })
        .unwrap();
}

/// DSNA-109: each committed snapshot runs retention.
#[test]
fn snapshot_runs_retention_after_commit() {
    let fx = FixtureProject::new().file("a.txt", "1").build();
    let env = Env::new(fx.root());
    set_keep(&env, 2);
    for content in ["2", "3", "4"] {
        snap(&env, None).version.unwrap();
        fs::write(fx.path("a.txt"), content).unwrap();
    }
    let v4 = snap(&env, None).version.unwrap();
    let kept = env.db.list_versions(env.project).unwrap();
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].id, v4.id, "newest is kept");
    assert_eq!(stored_bytes(&env)[&rp("a.txt")], b"4");
}

/// DSNA-109 / DSNA-55: a safety snapshot never runs retention, so a restore's own safety
/// version cannot prune the version being restored.
#[test]
fn safety_snapshot_does_not_run_retention() {
    let fx = FixtureProject::new().file("a.txt", "1").build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    fs::write(fx.path("a.txt"), "2").unwrap();
    snap(&env, None).version.unwrap();
    set_keep(&env, 1);
    fs::write(fx.path("a.txt"), "3").unwrap();
    env.dsnap
        .snapshot(
            env.project,
            SnapshotOptions {
                kind: VersionKind::Safety,
                ..SnapshotOptions::default()
            },
        )
        .unwrap()
        .version
        .unwrap();
    assert_eq!(versions(&env), 3);
}

/// Temp files left in the store (`XXXXXXXX.TMP` in a shard, or legacy `.tmp-*`).
fn temp_files(env: &Env) -> usize {
    let mut n = 0;
    let mut dirs = vec![env.dsnap.home().objects_dir()];
    while let Some(d) = dirs.pop() {
        for e in fs::read_dir(d).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().unwrap().is_dir() {
                dirs.push(e.path());
            } else if name.starts_with(".tmp-") || name.ends_with(".TMP") {
                n += 1;
            }
        }
    }
    n
}

/// DSNA-110: a snapshot cancelled while files are being stored commits nothing; the store
/// batch is dropped with its temp files, so neither blobs nor temp files are left.
#[test]
fn cancel_while_storing_leaves_no_blobs_or_temp_files() {
    let mut fx = FixtureProject::new();
    for i in 0..200 {
        fx = fx.file(&format!("f{i:03}.txt"), format!("content {i}"));
    }
    let fx = fx.build();
    let env = Env::new(fx.root());
    let cancel = CancelToken::new();
    let c = cancel.clone();
    // The first hash event comes after 32 of 200 files: cancel mid-way.
    let progress = Progress::new(move |e| {
        if e.stage == Stage::Hash && e.total.is_some_and(|t| e.done < t) {
            c.cancel();
        }
    });
    let err = env
        .dsnap
        .snapshot(
            env.project,
            SnapshotOptions {
                cancel: Some(cancel),
                progress: Some(progress),
                ..SnapshotOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert_eq!(versions(&env), 0);
    assert_eq!(store_files(&env), 0, "no blobs or temp files left");
}
