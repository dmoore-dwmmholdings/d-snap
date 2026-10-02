//! Restore end to end (Chain M: DSNA-57, DSNA-58, DSNA-85, DSNA-87).
#![allow(clippy::unwrap_used)] // helpers outside #[test] fns are test code too

#[path = "support/project.rs"]
mod project;

use std::fs;

use dsnap_core::store::Store;
use dsnap_core::{BlobHash, Error, GlobalSettings, RelPath, VersionId, VersionKind};
use dsnap_test_support::{FixtureProject, symlinks_supported, tree_bytes, tree_dirs};

use project::{Env, rewrite, rp, snap, versions};

fn v(env: &Env, label: &str) -> VersionId {
    snap(env, Some(label)).version.unwrap().id
}

fn read(env: &Env, p: &str) -> String {
    fs::read_to_string(rp(p).to_path(&root(env))).unwrap()
}

fn root(env: &Env) -> std::path::PathBuf {
    env.db.get_project(env.project).unwrap().root
}

fn strs(v: &[RelPath]) -> Vec<&str> {
    v.iter().map(RelPath::as_str).collect()
}

fn set_cap(env: &Env, cap: u64) {
    env.db
        .set_global_settings(&GlobalSettings {
            size_cap_bytes: cap,
            ..GlobalSettings::default()
        })
        .unwrap();
}

// ---- restore_file (DSNA-57) ----

#[test]
fn restore_file_brings_back_modified_content_and_takes_a_safety_version() {
    let fx = FixtureProject::new()
        .file("a.txt", "v1\n")
        .file("b.txt", "b\n")
        .build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "first");
    rewrite(&fx.path("a.txt"), b"local edit\n");
    rewrite(&fx.path("b.txt"), b"b edit\n");

    let r = env
        .dsnap
        .restore_file(env.project, v1, &rp("a.txt"))
        .unwrap();
    assert_eq!(strs(&r.written), ["a.txt"]);
    assert!(r.failed.is_empty() && r.uncaptured.is_empty() && r.deleted.is_empty());
    assert_eq!(read(&env, "a.txt"), "v1\n");
    assert_eq!(read(&env, "b.txt"), "b edit\n", "other files untouched");

    let safety = env.db.get_version(r.safety_version).unwrap();
    assert_eq!(safety.kind, VersionKind::Safety);
    assert_eq!(safety.label, "Before restore to first");

    // Restoring the safety version undoes the restore.
    env.dsnap
        .restore_file(env.project, r.safety_version, &rp("a.txt"))
        .unwrap();
    assert_eq!(read(&env, "a.txt"), "local edit\n");
}

#[test]
fn restore_file_recreates_a_deleted_file_and_its_directory() {
    let fx = FixtureProject::new().file("src/deep/x.rs", "x").build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    fs::remove_dir_all(fx.path("src")).unwrap();
    let r = env
        .dsnap
        .restore_file(env.project, v1, &rp("src/deep/x.rs"))
        .unwrap();
    assert_eq!(strs(&r.written), ["src/deep/x.rs"]);
    assert_eq!(read(&env, "src/deep/x.rs"), "x");
}

#[test]
fn restore_file_on_unchanged_folder_still_makes_an_undo_version() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    let r = env
        .dsnap
        .restore_file(env.project, v1, &rp("a.txt"))
        .unwrap();
    assert_ne!(r.safety_version, v1, "a version of its own (DSNA-85)");
    assert!(r.written.is_empty(), "nothing to change");
    assert_eq!(versions(&env), 2);
}

#[test]
fn restore_file_unknown_path_or_version_is_not_found_and_writes_nothing() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    let e = env.dsnap.restore_file(env.project, v1, &rp("nope.txt"));
    assert!(matches!(e, Err(Error::NotFound(_))), "{e:?}");
    let e = env
        .dsnap
        .restore_file(env.project, VersionId(999), &rp("a.txt"));
    assert!(matches!(e, Err(Error::NotFound(_))), "{e:?}");
    assert_eq!(versions(&env), 1, "no safety version either");
}

#[test]
fn failed_safety_snapshot_writes_nothing() {
    let fx = FixtureProject::new().file("a.txt", "v1").build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    let edit = b"edit that must survive";
    rewrite(&fx.path("a.txt"), edit);
    // Block the store directory the new content's blob would go to.
    let store = Store::open(env.dsnap.home().objects_dir()).unwrap();
    let blob_dir = store
        .path_of(&BlobHash::of(edit))
        .parent()
        .unwrap()
        .to_path_buf();
    let _ = fs::remove_dir_all(&blob_dir);
    fs::write(&blob_dir, "not a directory").unwrap();

    let e = env.dsnap.restore_file(env.project, v1, &rp("a.txt"));
    assert!(
        matches!(e, Err(Error::SafetySnapshotFailed { .. })),
        "{e:?}"
    );
    assert_eq!(fs::read(fx.path("a.txt")).unwrap(), edit);
    let e = env.dsnap.restore_project(env.project, v1);
    assert!(
        matches!(e, Err(Error::SafetySnapshotFailed { .. })),
        "{e:?}"
    );
    assert_eq!(fs::read(fx.path("a.txt")).unwrap(), edit);
}

// ---- Rule 1: uncaptured paths (DSNA-87) ----

#[test]
fn over_cap_file_is_never_overwritten() {
    let fx = FixtureProject::new().file("data.bin", "small").build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    set_cap(&env, 1000);
    let big = vec![7u8; 5000];
    rewrite(&fx.path("data.bin"), &big);

    let plan = env.dsnap.restore_plan(env.project, v1).unwrap();
    assert!(plan.write.is_empty());
    assert_eq!(strs(&plan.uncaptured), ["data.bin"]);

    let r = env.dsnap.restore_project(env.project, v1).unwrap();
    assert_eq!(strs(&r.uncaptured), ["data.bin"]);
    assert!(r.written.is_empty());
    assert_eq!(fs::read(fx.path("data.bin")).unwrap(), big);

    let r = env
        .dsnap
        .restore_file(env.project, v1, &rp("data.bin"))
        .unwrap();
    assert_eq!(strs(&r.uncaptured), ["data.bin"]);
    assert_eq!(fs::read(fx.path("data.bin")).unwrap(), big);
}

#[test]
fn path_ignored_since_the_version_is_never_overwritten() {
    let fx = FixtureProject::new().file("config.env", "old").build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    fs::write(fx.path(".gitignore"), "*.env\n").unwrap();
    rewrite(&fx.path("config.env"), b"current secret");

    let r = env
        .dsnap
        .restore_file(env.project, v1, &rp("config.env"))
        .unwrap();
    assert_eq!(strs(&r.uncaptured), ["config.env"]);
    assert_eq!(read(&env, "config.env"), "current secret");

    let r = env.dsnap.restore_project(env.project, v1).unwrap();
    assert!(r.uncaptured.iter().any(|p| p.as_str() == "config.env"));
    assert_eq!(read(&env, "config.env"), "current secret");
    // The .gitignore was added since v1, so it is deleted.
    assert!(!fx.path(".gitignore").exists());
}

#[test]
fn ignored_files_are_never_touched() {
    let fx = FixtureProject::new()
        .gitignore(&["node_modules/", ".env"])
        .file("src/a.rs", "a")
        .build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    fs::create_dir_all(fx.path("node_modules/pkg")).unwrap();
    fs::write(fx.path("node_modules/pkg/x.js"), "x").unwrap();
    fs::write(fx.path(".env"), "KEY=1").unwrap();
    fs::write(fx.path("src/new.rs"), "new").unwrap();

    let r = env.dsnap.restore_project(env.project, v1).unwrap();
    assert_eq!(strs(&r.deleted), ["src/new.rs"]);
    assert_eq!(read(&env, "node_modules/pkg/x.js"), "x");
    assert_eq!(read(&env, ".env"), "KEY=1");
}

#[test]
fn file_over_a_dir_with_ignored_content_is_left_alone() {
    // The version has file `out`; the folder has out/ holding an ignored file.
    let fx = FixtureProject::new()
        .gitignore(&["*.dat"])
        .file("out", "o")
        .build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    fs::remove_file(fx.path("out")).unwrap();
    fs::create_dir(fx.path("out")).unwrap();
    fs::write(fx.path("out/precious.dat"), "p").unwrap();

    let r = env.dsnap.restore_project(env.project, v1).unwrap();
    assert_eq!(strs(&r.uncaptured), ["out"]);
    assert_eq!(read(&env, "out/precious.dat"), "p");
}

// ---- restore_project (DSNA-58) ----

#[test]
fn project_round_trip_is_byte_identical() {
    let mut b = FixtureProject::new()
        .file("a.txt", "hello\n")
        .file("src/main.rs", "fn main() {}\n")
        .file("bin/data.bin", [0u8, 1, 2, 255])
        .file("ro.txt", "read only")
        .readonly("ro.txt")
        .dir("empty_dir");
    if symlinks_supported() {
        b = b.symlink("link", "a.txt");
    }
    let fx = b.build();
    let env = Env::new(fx.root());
    let before = tree_bytes(fx.root());
    let dirs = tree_dirs(fx.root());
    let v1 = v(&env, "v1");

    rewrite(&fx.path("a.txt"), b"changed");
    fs::remove_file(fx.path("src/main.rs")).unwrap();
    fs::write(fx.path("added.txt"), "added").unwrap();
    fs::create_dir_all(fx.path("newdir/sub")).unwrap();
    fs::write(fx.path("newdir/sub/f"), "f").unwrap();
    fs::remove_dir(fx.path("empty_dir")).unwrap();
    if symlinks_supported() {
        fs::remove_file(fx.path("link")).unwrap();
    }

    let plan = env.dsnap.restore_plan(env.project, v1).unwrap();
    let mut want_write = vec!["a.txt", "src/main.rs"];
    if symlinks_supported() {
        want_write.insert(1, "link");
    }
    assert_eq!(strs(&plan.write), want_write);
    assert_eq!(strs(&plan.delete), ["added.txt", "newdir/sub/f"]);
    assert_eq!(strs(&plan.create_dirs), ["empty_dir"]);

    let r = env.dsnap.restore_project(env.project, v1).unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!(tree_bytes(fx.root()), before);
    assert_eq!(tree_dirs(fx.root()), dirs);
    assert!(
        fs::metadata(fx.path("ro.txt"))
            .unwrap()
            .permissions()
            .readonly()
    );

    // The safety version holds the pre-restore folder; restoring it puts that back.
    let r2 = env
        .dsnap
        .restore_project(env.project, r.safety_version)
        .unwrap();
    assert!(r2.failed.is_empty(), "{:?}", r2.failed);
    assert_eq!(read(&env, "a.txt"), "changed");
    assert_eq!(read(&env, "newdir/sub/f"), "f");
    assert!(!fx.path("src/main.rs").exists());
}

#[test]
fn restore_replaces_a_file_with_a_directory_and_back() {
    let fx = FixtureProject::new().file("x/inner.txt", "inner").build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    fs::remove_dir_all(fx.path("x")).unwrap();
    fs::write(fx.path("x"), "now a file").unwrap();
    let v2 = v(&env, "v2");

    env.dsnap.restore_project(env.project, v1).unwrap();
    assert_eq!(read(&env, "x/inner.txt"), "inner");
    env.dsnap.restore_project(env.project, v2).unwrap();
    assert_eq!(read(&env, "x"), "now a file");
}

#[test]
fn retention_after_restore_keeps_the_target_and_undo_point() {
    let fx = FixtureProject::new().file("a.txt", "1").build();
    let env = Env::new(fx.root());
    env.db
        .set_global_settings(&GlobalSettings {
            retention_keep: 2,
            ..GlobalSettings::default()
        })
        .unwrap();
    let target = v(&env, "v1");
    rewrite(&fx.path("a.txt"), b"2");
    v(&env, "v2");
    // The safety version makes three unpinned versions; without protection retention
    // would delete the oldest, which is the target.
    let r = env.dsnap.restore_project(env.project, target).unwrap();
    assert!(env.db.get_version(target).is_ok(), "target protected");
    assert!(
        env.db.get_version(r.safety_version).is_ok(),
        "undo point kept"
    );
    assert_eq!(read(&env, "a.txt"), "1");
}

#[test]
fn missing_blob_fails_before_any_write() {
    // DSNA-85: another process pruned a blob of the target. Staging fails first.
    let fx = FixtureProject::new()
        .file("a.txt", "v1 a")
        .file("b.txt", "v1 b")
        .build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    rewrite(&fx.path("a.txt"), b"edit a");
    rewrite(&fx.path("b.txt"), b"edit b");
    fs::write(fx.path("added.txt"), "added").unwrap();
    let before = tree_bytes(fx.root());
    let store = Store::open(env.dsnap.home().objects_dir()).unwrap();
    fs::remove_file(store.path_of(&BlobHash::of(b"v1 b"))).unwrap();

    assert!(env.dsnap.restore_project(env.project, v1).is_err());
    assert_eq!(tree_bytes(fx.root()), before, "folder untouched");
    let stray: Vec<_> = fs::read_dir(fx.root())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".dsnap-restore-")
        })
        .collect();
    assert!(stray.is_empty(), "temp files cleaned up");
}

#[cfg(windows)]
#[test]
fn locked_file_stops_the_restore_and_the_safety_version_undoes_it() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;

    let fx = FixtureProject::new()
        .file("a.txt", "a1")
        .file("b.txt", "b1")
        .file("c.txt", "c1")
        .build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    for f in ["a.txt", "b.txt", "c.txt"] {
        rewrite(&fx.path(f), format!("{f} edited").as_bytes());
    }
    let edited = tree_bytes(fx.root());
    // Readable (so the safety snapshot captures it) but not replaceable.
    let held = fs::File::options()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(fx.path("b.txt"))
        .unwrap();

    let r = env.dsnap.restore_project(env.project, v1).unwrap();
    assert_eq!(strs(&r.written), ["a.txt"]);
    let failed: Vec<&str> = r.failed.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(failed, ["b.txt", "c.txt"]);
    assert!(r.failed[0].1.contains("locked"), "{}", r.failed[0].1);
    assert_eq!(read(&env, "b.txt"), "b.txt edited");
    drop(held);

    let undo = env
        .dsnap
        .restore_project(env.project, r.safety_version)
        .unwrap();
    assert!(undo.failed.is_empty(), "{:?}", undo.failed);
    assert_eq!(tree_bytes(fx.root()), edited);
}

#[cfg(windows)]
#[test]
fn case_only_rename_restores_name_and_content() {
    // DSNA-89: Foo.rs = "x"; renamed to foo.rs and edited to "y".
    let fx = FixtureProject::new().file("Foo.rs", "x").build();
    let env = Env::new(fx.root());
    let v1 = v(&env, "v1");
    fs::rename(fx.path("Foo.rs"), fx.path("foo.rs")).unwrap();
    rewrite(&fx.path("foo.rs"), b"y");

    let r = env.dsnap.restore_project(env.project, v1).unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    let names: Vec<String> = fs::read_dir(fx.root())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["Foo.rs"]);
    assert_eq!(read(&env, "Foo.rs"), "x");
}
