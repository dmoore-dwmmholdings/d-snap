//! Projects service and settings (DSNA-53, DSNA-97).
#![allow(clippy::unwrap_used, clippy::panic)] // helpers outside #[test] fns are test code too

use std::fs;
use std::path::Path;

mod common;

use dsnap_core::{AutoSnapshot, Dsnap, Error, GlobalSettings, ProjectId, ProjectSettings};
use dsnap_test_support::TestHome;

fn invalid<T: std::fmt::Debug>(r: dsnap_core::Result<T>) -> String {
    match r {
        Err(Error::InvalidInput(m)) => m,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn blob_exists(ds: &Dsnap, hash: &dsnap_core::BlobHash) -> bool {
    ds.read_blob(hash).is_ok()
}

#[test]
fn add_uses_folder_name_and_canonical_root() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    let root = dir.path().join("My Proj");
    fs::create_dir(&root).unwrap();

    let p = ds.add_project(&root.join("."), None).unwrap();
    assert_eq!(p.name, "My Proj");
    assert!(!p.missing);
    assert_eq!(p.settings, ProjectSettings::default());
    assert_eq!(p.root, dsnap_core::projects::normalize_root(&root).unwrap());
    assert!(p.root.is_absolute());
    assert!(!p.root.to_string_lossy().starts_with(r"\\?\"));
    assert_eq!(ds.project(p.id).unwrap(), p);
    assert_eq!(ds.list_projects().unwrap(), vec![p]);

    let q = ds.add_project(&dir.path().join("other_named"), Some("x"));
    invalid(q); // does not exist
    fs::create_dir(dir.path().join("b")).unwrap();
    let q = ds
        .add_project(&dir.path().join("b"), Some("  Named  "))
        .unwrap();
    assert_eq!(q.name, "Named");
}

#[test]
fn add_rejects_bad_paths_and_names() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    fs::write(dir.path().join("file"), b"x").unwrap();
    invalid(ds.add_project(&dir.path().join("nope"), None));
    invalid(ds.add_project(&dir.path().join("file"), None));
    invalid(ds.add_project(Path::new(""), None));
    invalid(ds.add_project(dir.path(), Some("   ")));
    invalid(ds.add_project(dir.path(), Some(&"n".repeat(201))));
    assert!(ds.list_projects().unwrap().is_empty());
}

#[test]
fn add_rejects_duplicates_and_nesting() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    let a = dir.path().join("a");
    fs::create_dir_all(a.join("inner")).unwrap();
    fs::create_dir(dir.path().join("ab")).unwrap();
    ds.add_project(&a, None).unwrap();

    assert!(invalid(ds.add_project(&a, None)).contains("already tracked"));
    assert!(invalid(ds.add_project(&a.join("inner"), None)).contains("inside"));
    assert!(invalid(ds.add_project(dir.path(), None)).contains("contains"));
    // A sibling whose name starts with the same characters is fine.
    ds.add_project(&dir.path().join("ab"), None).unwrap();
}

#[test]
fn add_rejects_the_dsnap_home() {
    let home = TestHome::new();
    let ds = home.open();
    invalid(ds.add_project(home.path(), None));
    invalid(ds.add_project(&home.path().join("objects"), None));
    invalid(ds.add_project(home.path().parent().unwrap(), None));
}

/// DSNA-97: one folder spelled differently is still the same project.
#[cfg(windows)]
#[test]
fn same_folder_with_other_spelling_is_a_duplicate_on_windows() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    let root = dir.path().join("Proj");
    fs::create_dir(&root).unwrap();
    let p = ds.add_project(&root, None).unwrap();

    let s = root.to_str().unwrap();
    let spellings = [
        s.to_uppercase(),
        s.to_lowercase(),
        format!(r"{s}\"),
        format!(r"\\?\{}", p.root.display()),
    ];
    for other in spellings {
        let msg = invalid(ds.add_project(Path::new(&other), None));
        assert!(msg.contains("already tracked"), "{other}: {msg}");
    }

    // A row stored before normalization (different case) is still caught.
    let db = dsnap_core::db::Db::open(&home.path().join("dsnap.db")).unwrap();
    let dir2 = tmp();
    let legacy = dir2.path().join("Legacy");
    fs::create_dir(&legacy).unwrap();
    let canon = dsnap_core::projects::normalize_root(&legacy).unwrap();
    let lower = std::path::PathBuf::from(canon.to_str().unwrap().to_lowercase());
    db.insert_project("legacy", &lower, &ProjectSettings::default())
        .unwrap();
    invalid(ds.add_project(&legacy, None));
}

#[test]
fn missing_folder_is_reported_and_relocate_fixes_it() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    let root = dir.path().join("p");
    fs::create_dir(&root).unwrap();
    let p = ds.add_project(&root, None).unwrap();

    fs::remove_dir(&root).unwrap();
    assert!(ds.project(p.id).unwrap().missing);
    assert!(ds.list_projects().unwrap()[0].missing);

    // A file where the folder was still counts as missing.
    fs::write(&root, b"x").unwrap();
    assert!(ds.project(p.id).unwrap().missing);

    let moved = dir.path().join("moved");
    fs::create_dir(&moved).unwrap();
    let r = ds.relocate_project(p.id, &moved).unwrap();
    assert!(!r.missing);
    assert_eq!(
        r.root,
        dsnap_core::projects::normalize_root(&moved).unwrap()
    );
    assert_eq!(r.name, p.name);
}

#[test]
fn relocate_validates_like_add() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    for n in ["a", "b", "c"] {
        fs::create_dir(dir.path().join(n)).unwrap();
    }
    let a = ds.add_project(&dir.path().join("a"), None).unwrap();
    ds.add_project(&dir.path().join("b"), None).unwrap();

    invalid(ds.relocate_project(a.id, &dir.path().join("b")));
    invalid(ds.relocate_project(a.id, dir.path()));
    invalid(ds.relocate_project(a.id, &dir.path().join("nope")));
    invalid(ds.relocate_project(a.id, home.path()));
    // Its own folder (or one inside it) is fine.
    ds.relocate_project(a.id, &dir.path().join("a")).unwrap();
    fs::create_dir(dir.path().join("a").join("sub")).unwrap();
    ds.relocate_project(a.id, &dir.path().join("a").join("sub"))
        .unwrap();
    ds.relocate_project(a.id, &dir.path().join("c")).unwrap();
    assert!(matches!(
        ds.relocate_project(ProjectId(999), &dir.path().join("a")),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn rename_validates_and_trims() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    let p = ds.add_project(dir.path(), None).unwrap();
    ds.rename_project(p.id, "  New  ").unwrap();
    assert_eq!(ds.project(p.id).unwrap().name, "New");
    invalid(ds.rename_project(p.id, ""));
    invalid(ds.rename_project(p.id, &"x".repeat(201)));
    assert!(matches!(
        ds.rename_project(ProjectId(999), "x"),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn remove_keeps_or_prunes_blobs() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    for n in ["a", "b", "c"] {
        fs::create_dir(dir.path().join(n)).unwrap();
    }
    let a = ds.add_project(&dir.path().join("a"), None).unwrap();
    let b = ds.add_project(&dir.path().join("b"), None).unwrap();
    let c = ds.add_project(&dir.path().join("c"), None).unwrap();
    let cli = common::Cli::open(home.path());
    let only_a = cli.commit_one(a.id, b"only a");
    let shared = cli.commit_one(a.id, b"shared");
    cli.commit_one(c.id, b"shared");
    let only_b = cli.commit_one(b.id, b"only b");

    // Without deleting snapshots: rows go, blobs stay for the next prune.
    ds.remove_project(b.id, false).unwrap();
    assert!(matches!(ds.project(b.id), Err(Error::NotFound(_))));
    assert!(blob_exists(&ds, &only_b));

    // With deleting snapshots: unreferenced blobs go (including b's leftover), shared stay.
    ds.remove_project(a.id, true).unwrap();
    let db = &cli.db;
    assert!(db.list_versions(a.id).unwrap().is_empty());
    assert!(!blob_exists(&ds, &only_a));
    assert!(!blob_exists(&ds, &only_b));
    assert!(blob_exists(&ds, &shared));
    assert_eq!(db.list_versions(c.id).unwrap().len(), 1);

    assert!(matches!(
        ds.remove_project(a.id, true),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn project_settings_round_trip_and_validation() {
    let home = TestHome::new();
    let ds = home.open();
    let dir = tmp();
    let p = ds.add_project(dir.path(), None).unwrap();
    assert_eq!(
        ds.project_settings(p.id).unwrap(),
        ProjectSettings::default()
    );

    let s = ProjectSettings {
        extra_ignore: vec!["*.log".into(), "/build/".into(), "!keep.log".into()],
        respect_gitignore: false,
        auto_snapshot: AutoSnapshot::AfterIdle { secs: 30 },
    };
    ds.set_project_settings(p.id, &s).unwrap();
    assert_eq!(ds.project_settings(p.id).unwrap(), s);
    assert_eq!(ds.project(p.id).unwrap().settings, s);

    let bad = |f: &dyn Fn(&mut ProjectSettings)| {
        let mut b = s.clone();
        f(&mut b);
        invalid(ds.set_project_settings(p.id, &b));
    };
    bad(&|b| b.extra_ignore.push("  ".into()));
    bad(&|b| b.extra_ignore.push("a\nb".into()));
    bad(&|b| b.extra_ignore.push("a{b".into()));
    bad(&|b| b.auto_snapshot = AutoSnapshot::Every { secs: 0 });
    bad(&|b| {
        b.auto_snapshot = AutoSnapshot::AfterIdle {
            secs: 8 * 24 * 3600,
        }
    });
    assert_eq!(ds.project_settings(p.id).unwrap(), s);

    assert!(matches!(
        ds.set_project_settings(ProjectId(999), &s),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        ds.project_settings(ProjectId(999)),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn global_settings_round_trip_and_validation() {
    let home = TestHome::new();
    let ds = home.open();
    assert_eq!(ds.global_settings().unwrap(), GlobalSettings::default());
    let g = GlobalSettings {
        size_cap_bytes: 1024,
        retention_keep: 5,
    };
    ds.set_global_settings(&g).unwrap();
    assert_eq!(ds.global_settings().unwrap(), g);
    // Shared with another handle (the CLI).
    assert_eq!(home.open().global_settings().unwrap(), g);

    invalid(ds.set_global_settings(&GlobalSettings {
        retention_keep: 0,
        ..g.clone()
    }));
    invalid(ds.set_global_settings(&GlobalSettings {
        size_cap_bytes: 0,
        ..g.clone()
    }));
    invalid(ds.set_global_settings(&GlobalSettings {
        size_cap_bytes: u64::MAX,
        ..g.clone()
    }));
    assert_eq!(ds.global_settings().unwrap(), g);
}
