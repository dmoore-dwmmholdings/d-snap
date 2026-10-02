//! Live updates through the app backend (DSNA-73, DSNA-74): watcher, DB poll and
//! auto-snapshot events.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsnap_bridge::backend::{
    Backend, EVENT_PROJECT_CHANGED, EVENT_PROJECTS_CHANGED, EVENT_VERSIONS_CHANGED, Emit, Op,
};
use dsnap_core::{AutoSnapshot, SnapshotOptions, VersionKind};
use dsnap_test_support::{FixtureProject, TestHome};
use serde_json::Value;

type Events = Arc<Mutex<Vec<(String, Value)>>>;

fn backend(home: &TestHome) -> (Arc<Backend>, Events) {
    let events: Events = Arc::default();
    let sink = Arc::clone(&events);
    let emit: Emit = Arc::new(move |n, v| sink.lock().unwrap().push((n.to_owned(), v)));
    let b = Arc::new(Backend::open(Some(home.path().to_path_buf()), emit));
    b.start_live();
    (b, events)
}

/// Wait until an event satisfies `pred`.
fn wait_for(events: &Events, what: &str, pred: impl Fn(&str, &Value) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if events.lock().unwrap().iter().any(|(n, v)| pred(n, v)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no {what}; got {:?}", events.lock().unwrap());
}

#[test]
fn file_changes_and_cli_versions_reach_the_app() {
    let home = TestHome::new();
    let (b, events) = backend(&home);
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let p = b.add_project(fx.root().to_str().unwrap()).unwrap();
    b.snapshot(&b.begin(p.id, Op::Snapshot), None).unwrap();
    let id = p.id.0;

    fs::write(fx.path("a.txt"), "edited").unwrap();
    wait_for(&events, "change count 1", |n, v| {
        n == EVENT_PROJECT_CHANGED && v["projectId"] == id && v["changedCount"] == 1
    });

    events.lock().unwrap().clear();
    let cli = home.open();
    cli.snapshot(
        p.id,
        SnapshotOptions {
            kind: VersionKind::Cli,
            ..SnapshotOptions::default()
        },
    )
    .unwrap();
    wait_for(&events, "versions-changed from the CLI", |n, v| {
        n == EVENT_VERSIONS_CHANGED && v["projectId"] == id
    });
    b.stop_live();
}

#[test]
fn a_removed_folder_is_announced() {
    let home = TestHome::new();
    let (b, events) = backend(&home);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    fs::create_dir(&root).unwrap();
    b.add_project(root.to_str().unwrap()).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    // A watched folder may refuse to move on Windows; try both.
    let moved = (0..20).any(|_| {
        let ok = fs::rename(&root, tmp.path().join("moved")).is_ok();
        if !ok {
            std::thread::sleep(Duration::from_millis(100));
        }
        ok
    });
    if moved {
        wait_for(&events, "projects-changed", |n, _| {
            n == EVENT_PROJECTS_CHANGED
        });
    }
    b.stop_live();
}

#[test]
fn idle_auto_snapshot_runs_through_the_backend() {
    let home = TestHome::new();
    let (b, events) = backend(&home);
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let p = b.add_project(fx.root().to_str().unwrap()).unwrap();
    b.snapshot(&b.begin(p.id, Op::Snapshot), None).unwrap();
    let mut s = b.get_project_settings(p.id).unwrap();
    s.auto_snapshot = AutoSnapshot::AfterIdle { secs: 1 };
    b.set_project_settings(p.id, &s).unwrap();

    std::thread::sleep(Duration::from_millis(500));
    fs::write(fx.path("a.txt"), "edited").unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let versions = b.list_versions(p.id).unwrap();
        if versions[0].kind == VersionKind::Auto {
            break;
        }
        assert!(
            Instant::now() < end,
            "no auto snapshot; events {:?}",
            events.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    b.stop_live();
}
