//! Working-tree status, `changes()` and `file_diff()` (DSNA-51).
#![allow(clippy::unwrap_used, clippy::panic)] // helpers outside #[test] fns are test code too

#[path = "support/project.rs"]
mod project;

use std::fs;

use dsnap_core::{
    ChangeStatus, DiffBody, DiffOptions, Error, FileChange, LineTag, ProjectSettings, VersionRef,
};
use dsnap_test_support::FixtureProject;

use project::{Env, rp, snap, store_files};

fn summary(changes: &[FileChange]) -> Vec<(String, &'static str)> {
    changes
        .iter()
        .map(|c| {
            let s = match c.status {
                ChangeStatus::Added => "A",
                ChangeStatus::Modified => "M",
                ChangeStatus::Deleted => "D",
                ChangeStatus::Renamed { .. } => "R",
            };
            (c.path.to_string(), s)
        })
        .collect()
}

fn s(path: &str, status: &'static str) -> (String, &'static str) {
    (path.to_owned(), status)
}

#[test]
fn status_reports_edits_adds_and_deletes() {
    let fx = FixtureProject::new()
        .file("keep.txt", "k")
        .file("edit.txt", "one\ntwo\n")
        .file("gone.txt", "bye")
        .build();
    let env = Env::new(fx.root());

    // No version yet: everything is added.
    assert_eq!(
        summary(&env.dsnap.status(env.project).unwrap()),
        [s("edit.txt", "A"), s("gone.txt", "A"), s("keep.txt", "A")]
    );
    snap(&env, None).version.unwrap();
    assert!(env.dsnap.status(env.project).unwrap().is_empty());

    project::rewrite(&fx.path("edit.txt"), b"one\nTWO\nthree\n");
    fs::remove_file(fx.path("gone.txt")).unwrap();
    fs::write(fx.path("new.txt"), "fresh").unwrap();
    let st = env.dsnap.status(env.project).unwrap();
    assert_eq!(
        summary(&st),
        [s("edit.txt", "M"), s("gone.txt", "D"), s("new.txt", "A")]
    );
    let edit = &st[0];
    assert_ne!(
        edit.old.as_ref().unwrap().blob,
        edit.new.as_ref().unwrap().blob
    );

    // After a snapshot the folder is clean again.
    snap(&env, None).version.unwrap();
    assert!(env.dsnap.status(env.project).unwrap().is_empty());
}

#[test]
fn status_and_working_entries_write_nothing() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    project::rewrite(&fx.path("a.txt"), b"changed");
    fs::write(fx.path("b.txt"), "new").unwrap();

    let token = env.db.change_token().unwrap();
    let files = store_files(&env);
    assert_eq!(env.dsnap.status(env.project).unwrap().len(), 2);
    let entries = env.dsnap.working_entries(env.project).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.blob.is_some()));
    env.dsnap
        .changes(env.project, None, VersionRef::WorkingTree)
        .unwrap();
    assert_eq!(env.db.change_token().unwrap(), token);
    assert_eq!(store_files(&env), files);
    assert_eq!(project::versions(&env), 1);
}

#[test]
fn mtime_only_change_is_clean() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    filetime::set_file_mtime(
        fx.path("a.txt"),
        filetime::FileTime::from_system_time(later),
    )
    .unwrap();
    assert!(env.dsnap.status(env.project).unwrap().is_empty());
}

#[test]
fn newly_ignored_and_oversize_paths_do_not_show_as_deleted() {
    let fx = FixtureProject::new()
        .file("a.txt", "a")
        .file("debug.log", "log")
        .file("grows.bin", "small")
        .build();
    let env = Env::with_settings(fx.root(), ProjectSettings::default());
    snap(&env, None).version.unwrap();

    fs::write(fx.path(".gitignore"), "*.log\n").unwrap();
    env.db
        .set_global_settings(&dsnap_core::GlobalSettings {
            size_cap_bytes: 16,
            ..Default::default()
        })
        .unwrap();
    project::rewrite(&fx.path("grows.bin"), &[1u8; 64]);

    assert_eq!(
        summary(&env.dsnap.status(env.project).unwrap()),
        [s(".gitignore", "A")]
    );
}

#[test]
fn changes_between_any_two_versions_with_line_counts() {
    let fx = FixtureProject::new()
        .file("a.txt", "1\n2\n3\n")
        .file("b.txt", "b\n")
        .file("bin.dat", [0u8, 1, 2])
        .build();
    let env = Env::new(fx.root());
    let v1 = snap(&env, None).version.unwrap();

    project::rewrite(&fx.path("a.txt"), b"1\n2 changed\n3\n4\n");
    let v2 = snap(&env, None).version.unwrap();

    fs::remove_file(fx.path("b.txt")).unwrap();
    fs::write(fx.path("c.txt"), "c1\nc2\n").unwrap();
    project::rewrite(&fx.path("bin.dat"), &[0u8, 9, 9, 9]);
    let v3 = snap(&env, None).version.unwrap();

    // Default `from`: the version before.
    let c = env
        .dsnap
        .changes(env.project, None, VersionRef::Version(v2.id))
        .unwrap();
    assert_eq!(summary(&c), [s("a.txt", "M")]);
    assert_eq!((c[0].lines_added, c[0].lines_removed), (Some(2), Some(1)));

    // First version: everything added.
    let c = env
        .dsnap
        .changes(env.project, None, VersionRef::Version(v1.id))
        .unwrap();
    assert_eq!(c.len(), 3);
    let a = c.iter().find(|c| c.path == rp("a.txt")).unwrap();
    assert_eq!((a.lines_added, a.lines_removed), (Some(3), Some(0)));

    // Any pair (F16), either direction.
    let c = env
        .dsnap
        .changes(env.project, Some(v1.id), VersionRef::Version(v3.id))
        .unwrap();
    assert_eq!(
        summary(&c),
        [
            s("a.txt", "M"),
            s("b.txt", "D"),
            s("bin.dat", "M"),
            s("c.txt", "A")
        ]
    );
    let by = |p: &str| c.iter().find(|c| c.path == rp(p)).unwrap();
    assert_eq!(
        (by("b.txt").lines_added, by("b.txt").lines_removed),
        (Some(0), Some(1))
    );
    assert_eq!(
        (by("c.txt").lines_added, by("c.txt").lines_removed),
        (Some(2), Some(0))
    );
    assert_eq!(by("bin.dat").lines_added, None, "binary: no line counts");

    let back = env
        .dsnap
        .changes(env.project, Some(v3.id), VersionRef::Version(v1.id))
        .unwrap();
    assert_eq!(
        summary(&back),
        [
            s("a.txt", "M"),
            s("b.txt", "A"),
            s("bin.dat", "M"),
            s("c.txt", "D")
        ]
    );

    // Working tree against an older version.
    fs::write(fx.path("d.txt"), "d\n").unwrap();
    let c = env
        .dsnap
        .changes(env.project, Some(v2.id), VersionRef::WorkingTree)
        .unwrap();
    assert_eq!(
        summary(&c),
        [
            s("b.txt", "D"),
            s("bin.dat", "M"),
            s("c.txt", "A"),
            s("d.txt", "A")
        ]
    );
    let d = c.iter().find(|c| c.path == rp("d.txt")).unwrap();
    assert_eq!(d.lines_added, Some(1));
}

#[test]
fn versions_of_another_project_are_rejected() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    let v = snap(&env, None).version.unwrap();
    let other_root = env.home.path().join("other");
    fs::create_dir(&other_root).unwrap();
    let other = env
        .db
        .insert_project("other", &other_root, &ProjectSettings::default())
        .unwrap();
    let err = env
        .dsnap
        .changes(other, None, VersionRef::Version(v.id))
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    let err = env
        .dsnap
        .changes(other, Some(v.id), VersionRef::WorkingTree)
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
}

fn text_hunks(body: &DiffBody) -> &[dsnap_core::Hunk] {
    match body {
        DiffBody::Text { hunks } => hunks,
        other => panic!("expected a text diff, got {other:?}"),
    }
}

#[test]
fn file_diff_of_a_working_tree_file() {
    let fx = FixtureProject::new()
        .file("a.txt", "one\ntwo\nthree\n")
        .build();
    let env = Env::new(fx.root());
    let v1 = snap(&env, None).version.unwrap();
    project::rewrite(&fx.path("a.txt"), b"one\n2\nthree\n");
    fs::write(fx.path("new.txt"), "n\n").unwrap();

    let d = env
        .dsnap
        .file_diff(
            env.project,
            None,
            VersionRef::WorkingTree,
            &rp("a.txt"),
            &DiffOptions::default(),
        )
        .unwrap();
    let hunks = text_hunks(&d.body);
    assert_eq!(hunks.len(), 1);
    let changed: Vec<(LineTag, &str)> = hunks[0]
        .lines
        .iter()
        .filter(|l| l.tag != LineTag::Equal)
        .map(|l| (l.tag, l.text.as_str()))
        .collect();
    assert_eq!(changed, [(LineTag::Delete, "two"), (LineTag::Insert, "2")]);

    // An added working file, and the same file between stored versions.
    let d = env
        .dsnap
        .file_diff(
            env.project,
            Some(v1.id),
            VersionRef::WorkingTree,
            &rp("new.txt"),
            &DiffOptions::default(),
        )
        .unwrap();
    assert_eq!(text_hunks(&d.body)[0].new_len, 1);

    let v2 = snap(&env, None).version.unwrap();
    let d = env
        .dsnap
        .file_diff(
            env.project,
            None,
            VersionRef::Version(v2.id),
            &rp("a.txt"),
            &DiffOptions::default(),
        )
        .unwrap();
    assert_eq!(text_hunks(&d.body).len(), 1);

    let err = env
        .dsnap
        .file_diff(
            env.project,
            None,
            VersionRef::Version(v2.id),
            &rp("nope.txt"),
            &DiffOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
}

#[test]
fn file_diff_of_a_content_rename_loads_the_old_path() {
    let body = "line 1\nline 2\nline 3\n";
    let fx = FixtureProject::new().file("old/name.txt", body).build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    fs::create_dir(fx.path("new")).unwrap();
    fs::rename(fx.path("old/name.txt"), fx.path("new/name.txt")).unwrap();
    let v2 = snap(&env, None).version.unwrap();

    let c = env
        .dsnap
        .changes(env.project, None, VersionRef::Version(v2.id))
        .unwrap();
    let r = c.iter().find(|c| c.path == rp("new/name.txt")).unwrap();
    assert_eq!(
        r.status,
        ChangeStatus::Renamed {
            from: rp("old/name.txt")
        }
    );
    assert_eq!((r.lines_added, r.lines_removed), (Some(0), Some(0)));

    let d = env
        .dsnap
        .file_diff(
            env.project,
            None,
            VersionRef::Version(v2.id),
            &rp("new/name.txt"),
            &DiffOptions::default(),
        )
        .unwrap();
    assert!(text_hunks(&d.body).is_empty(), "{d:?}");
}

/// DSNA-89 on Windows: a case-only rename with an edit is one `Renamed` change, and its diff
/// shows only the edited lines.
#[cfg(windows)]
#[test]
fn case_rename_with_edit_diffs_against_the_old_name() {
    let fx = FixtureProject::new()
        .file("Foo.rs", "fn a() {}\nfn b() {}\nfn c() {}\n")
        .build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    fs::rename(fx.path("Foo.rs"), fx.path("foo.rs")).unwrap();
    project::rewrite(&fx.path("foo.rs"), b"fn a() {}\nfn B() {}\nfn c() {}\n");

    for take_snapshot in [false, true] {
        let to = if take_snapshot {
            VersionRef::Version(snap(&env, None).version.unwrap().id)
        } else {
            VersionRef::WorkingTree
        };
        let c = env.dsnap.changes(env.project, None, to).unwrap();
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].status, ChangeStatus::Renamed { from: rp("Foo.rs") });
        assert_eq!((c[0].lines_added, c[0].lines_removed), (Some(1), Some(1)));

        let d = env
            .dsnap
            .file_diff(
                env.project,
                None,
                to,
                &rp("foo.rs"),
                &DiffOptions::default(),
            )
            .unwrap();
        let changed: Vec<(LineTag, String)> = text_hunks(&d.body)
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.tag != LineTag::Equal)
            .map(|l| (l.tag, l.text.clone()))
            .collect();
        assert_eq!(
            changed,
            [
                (LineTag::Delete, "fn b() {}".to_owned()),
                (LineTag::Insert, "fn B() {}".to_owned())
            ]
        );
    }
}

#[test]
fn status_of_a_missing_folder_is_project_missing() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    let root = fx.root().to_path_buf();
    drop(fx);
    assert!(!root.exists());
    let err = env.dsnap.status(env.project).unwrap_err();
    assert!(matches!(err, Error::ProjectMissing { .. }), "{err}");
    // Stored versions still compare.
    let v = env.db.latest_version(env.project).unwrap().unwrap();
    assert_eq!(
        env.dsnap
            .changes(env.project, None, VersionRef::Version(v.id))
            .unwrap()
            .len(),
        1
    );
}

/// DSNA-105: a tracked file that grew past the size cap keeps its previous entry. Status lists
/// it as skipped (not as a change), and `file_diff` shows the stored content on both sides
/// instead of reading the over-cap file from disk.
#[test]
fn file_over_the_cap_is_reported_skipped_and_diffed_from_the_store() {
    let fx = FixtureProject::new()
        .file("grows.txt", "small\n")
        .file("a.txt", "a\n")
        .build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    env.db
        .set_global_settings(&dsnap_core::GlobalSettings {
            size_cap_bytes: 64,
            ..Default::default()
        })
        .unwrap();
    project::rewrite(&fx.path("grows.txt"), "big line\n".repeat(20).as_bytes());

    let report = env.dsnap.status_report(env.project).unwrap();
    assert!(report.changes.is_empty(), "{:?}", report.changes);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].path, "grows.txt");
    assert!(matches!(
        report.skipped[0].reason,
        dsnap_core::SkipReason::TooLarge { size: 180 }
    ));
    assert!(
        env.dsnap
            .changes(env.project, None, VersionRef::WorkingTree)
            .unwrap()
            .is_empty()
    );
    let d = env
        .dsnap
        .file_diff(
            env.project,
            None,
            VersionRef::WorkingTree,
            &rp("grows.txt"),
            &DiffOptions::default(),
        )
        .unwrap();
    assert!(text_hunks(&d.body).is_empty(), "{d:?}");
}

/// DSNA-105: in a working-tree comparison a locked file is not carried forward (status reads
/// once, without the snapshot's lock retry): it shows as modified by its stat, and
/// `file_diff` reports the read failure as `Error::Io` instead of a stale diff.
#[cfg(windows)]
#[test]
fn locked_working_file_is_modified_and_its_diff_is_an_io_error() {
    use std::os::windows::fs::OpenOptionsExt;

    let fx = FixtureProject::new()
        .file(
            "locked.txt",
            "one
",
        )
        .build();
    let env = Env::new(fx.root());
    snap(&env, None).version.unwrap();
    project::rewrite(
        &fx.path("locked.txt"),
        b"two
",
    );
    let guard = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fx.path("locked.txt"))
        .unwrap();
    let st = env.dsnap.status(env.project).unwrap();
    assert_eq!(summary(&st), [s("locked.txt", "M")]);
    let err = env
        .dsnap
        .file_diff(
            env.project,
            None,
            VersionRef::WorkingTree,
            &rp("locked.txt"),
            &DiffOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    drop(guard);
}
