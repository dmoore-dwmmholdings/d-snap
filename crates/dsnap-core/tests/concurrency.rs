//! App and CLI writing the same database at once (DSNA-68): two `Dsnap` handles in threads.
//! No busy error reaches the caller and no version is lost.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dsnap_core::lock::ProjectLock;
use dsnap_core::{Dsnap, ProjectId, RelPath, SnapshotOptions, VersionKind};
use dsnap_test_support::{FixtureProject, TestHome, generate_tree};

const ROUNDS: usize = 12;

fn snap(d: &Dsnap, p: ProjectId, kind: VersionKind) -> bool {
    d.snapshot(
        p,
        SnapshotOptions {
            kind,
            ..SnapshotOptions::default()
        },
    )
    .unwrap()
    .version
    .is_some()
}

fn edit(root: &Path, name: &str, i: usize) {
    fs::write(root.join(name), format!("{name} round {i}\n")).unwrap();
}

/// Two handles, one project, both taking the shared lock: snapshots, retention and restores
/// interleave without errors, and every committed version is there.
#[test]
fn app_and_cli_on_one_project_serialize_through_the_lock() {
    let home = TestHome::new();
    let fx = FixtureProject::new().file("seed.txt", "s").build();
    generate_tree(&fx.path("gen"), 300, 68);
    let app = Arc::new(home.open());
    let p = app.add_project(fx.root(), None).unwrap().id;
    assert!(snap(&app, p, VersionKind::Manual));
    let root = fx.root().to_path_buf();

    let worker = |d: Arc<Dsnap>, name: &'static str, kind: VersionKind| {
        let root = root.clone();
        std::thread::spawn(move || {
            let mut made = 0;
            for i in 0..ROUNDS {
                let path = d.project_lock_path(p);
                let _l = ProjectLock::acquire(&path, Duration::from_secs(60), None, || {})
                    .unwrap()
                    .unwrap();
                edit(&root, name, i);
                if snap(&d, p, kind) {
                    made += 1;
                }
                if i % 4 == 3 {
                    d.apply_retention(p).unwrap();
                    let first = d.list_versions(p).unwrap().last().unwrap().id;
                    d.restore_file(p, first, &RelPath::new("seed.txt").unwrap())
                        .unwrap();
                }
            }
            made
        })
    };
    let a = worker(Arc::clone(&app), "app.txt", VersionKind::Manual);
    let b = worker(Arc::new(home.open()), "cli.txt", VersionKind::Cli);
    let made = a.join().unwrap() + b.join().unwrap();

    // 1 baseline + every snapshot that wrote a version + one safety version per restore.
    let restores = 2 * (ROUNDS / 4);
    assert_eq!(made, 2 * ROUNDS);
    assert_eq!(app.list_versions(p).unwrap().len(), 1 + made + restores);
}

/// Two handles on two projects, no lock between them: the busy timeout and commit retry
/// absorb the contention.
#[test]
fn two_projects_write_at_once_without_busy_errors() {
    let home = TestHome::new();
    let fx1 = FixtureProject::new().build();
    let fx2 = FixtureProject::new().build();
    generate_tree(fx1.root(), 400, 1);
    generate_tree(fx2.root(), 400, 2);
    let d1 = home.open();
    let p1 = d1.add_project(fx1.root(), None).unwrap().id;
    let p2 = d1.add_project(fx2.root(), None).unwrap().id;

    let run = |d: Dsnap, p: ProjectId, root: std::path::PathBuf| {
        std::thread::spawn(move || {
            for i in 0..ROUNDS {
                edit(&root, "x.txt", i);
                assert!(snap(&d, p, VersionKind::Auto));
                d.apply_retention(p).unwrap();
            }
            d.list_versions(p).unwrap().len()
        })
    };
    let a = run(d1, p1, fx1.root().to_path_buf());
    let b = run(home.open(), p2, fx2.root().to_path_buf());
    assert_eq!(a.join().unwrap(), ROUNDS);
    assert_eq!(b.join().unwrap(), ROUNDS);
}
