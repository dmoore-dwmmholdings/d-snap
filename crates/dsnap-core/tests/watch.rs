//! File watcher, DB change watcher and auto-snapshot scheduler (Chain Q: DSNA-60/61/62).
#![allow(clippy::unwrap_used)]

use std::fs;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsnap_core::auto::{AutoScheduler, ManualClock};
use dsnap_core::watch::{DbChangeWatcher, WatchEvent, Watcher};
use dsnap_core::{AutoSnapshot, Dsnap, Project, ProjectSettings, SnapshotOptions, VersionKind};
use dsnap_test_support::{FixtureProject, TestHome};

fn setup(fx: &dsnap_test_support::Fixture) -> (TestHome, Arc<Dsnap>, Project) {
    let home = TestHome::new();
    let dsnap = Arc::new(home.open());
    let p = dsnap.add_project(fx.root(), None).unwrap();
    dsnap.snapshot(p.id, SnapshotOptions::default()).unwrap();
    (home, dsnap, p)
}

fn channel() -> (Arc<dyn Fn(WatchEvent) + Send + Sync>, Receiver<WatchEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    (
        Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        }),
        rx,
    )
}

/// Every event that arrives within `wait`.
fn collect(rx: &Receiver<WatchEvent>, wait: Duration) -> Vec<WatchEvent> {
    let mut out = Vec::new();
    let end = std::time::Instant::now() + wait;
    while let Ok(e) = rx.recv_timeout(end.saturating_duration_since(std::time::Instant::now())) {
        out.push(e);
    }
    out
}

fn changed(events: &[WatchEvent]) -> Vec<u32> {
    events
        .iter()
        .filter_map(|e| match e {
            WatchEvent::Changed { changed_count, .. } => Some(*changed_count),
            _ => None,
        })
        .collect()
}

const DEBOUNCE: Duration = Duration::from_millis(200);
const SETTLE: Duration = Duration::from_millis(1500);

#[test]
fn a_write_gives_one_changed_event_with_its_count() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let (_home, dsnap, p) = setup(&fx);
    let (sink, rx) = channel();
    let w = Watcher::with_debounce(dsnap, sink, DEBOUNCE);
    w.watch(p.id).unwrap();
    assert_eq!(changed(&collect(&rx, SETTLE)), [0], "initial report");

    fs::write(fx.path("a.txt"), "changed").unwrap();
    assert_eq!(changed(&collect(&rx, SETTLE)), [1]);
}

#[test]
fn ignored_writes_wake_nothing() {
    let fx = FixtureProject::new()
        .gitignore(&["build/"])
        .file("a.txt", "a")
        .dir("build")
        .build();
    let (_home, dsnap, p) = setup(&fx);
    let (sink, rx) = channel();
    let w = Watcher::with_debounce(dsnap, sink, DEBOUNCE);
    w.watch(p.id).unwrap();
    collect(&rx, SETTLE);

    fs::create_dir_all(fx.path("node_modules/pkg")).unwrap();
    for i in 0..20 {
        fs::write(fx.path(&format!("node_modules/pkg/{i}.js")), "x").unwrap();
        fs::write(fx.path(&format!("build/{i}.o")), "x").unwrap();
    }
    let events = collect(&rx, SETTLE);
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn a_burst_is_coalesced() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let (_home, dsnap, p) = setup(&fx);
    let (sink, rx) = channel();
    let w = Watcher::with_debounce(dsnap, sink, DEBOUNCE);
    w.watch(p.id).unwrap();
    collect(&rx, SETTLE);

    for i in 0..100 {
        fs::write(fx.path(&format!("f{i}.txt")), "x").unwrap();
    }
    let counts = changed(&collect(&rx, Duration::from_secs(3)));
    assert!(!counts.is_empty() && counts.len() <= 2, "{counts:?}");
    assert_eq!(*counts.last().unwrap(), 100);
}

#[test]
fn removed_root_is_reported_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("proj");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.txt"), "a").unwrap();
    let home = TestHome::new();
    let dsnap = Arc::new(home.open());
    let p = dsnap.add_project(&root, None).unwrap();
    let (sink, rx) = channel();
    let w = Watcher::with_debounce(dsnap, sink, DEBOUNCE);
    w.watch(p.id).unwrap();
    collect(&rx, SETTLE);

    // Unwatch-free removal: the OS watch may hold the folder on Windows, so move it away.
    let moved = tmp.path().join("moved");
    let mut ok = false;
    for _ in 0..20 {
        if fs::rename(&root, &moved).is_ok() || fs::remove_dir_all(&root).is_ok() {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !ok {
        // The OS refuses to move a watched folder here; dropping the watch is the user's
        // only way to remove it then, which `unwatch` covers.
        eprintln!("watched root could not be moved; Missing path not exercised");
        w.unwatch(p.id);
        return;
    }
    let events = collect(&rx, Duration::from_secs(5));
    assert!(
        events.contains(&WatchEvent::Missing { project: p.id }),
        "{events:?}"
    );
}

#[test]
fn unwatch_stops_events() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let (_home, dsnap, p) = setup(&fx);
    let (sink, rx) = channel();
    let w = Watcher::with_debounce(dsnap, sink, DEBOUNCE);
    w.watch(p.id).unwrap();
    collect(&rx, SETTLE);
    w.unwatch(p.id);
    assert!(w.watched().is_empty());
    fs::write(fx.path("a.txt"), "changed").unwrap();
    assert!(collect(&rx, SETTLE).is_empty());
}

// ---- DbChangeWatcher (DSNA-62) ----

#[test]
fn versions_added_by_another_handle_are_seen_within_a_second() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let (home, dsnap, p) = setup(&fx);
    let (sink, rx) = channel();
    let _w = DbChangeWatcher::with_interval(dsnap, sink, Duration::from_millis(100));
    std::thread::sleep(Duration::from_millis(300));

    // "The CLI": a second handle on the same home.
    let cli = home.open();
    fs::write(fx.path("a.txt"), "changed").unwrap();
    cli.snapshot(
        p.id,
        SnapshotOptions {
            kind: VersionKind::Cli,
            ..SnapshotOptions::default()
        },
    )
    .unwrap();
    let events = collect(&rx, Duration::from_secs(1));
    assert!(
        events.contains(&WatchEvent::VersionsChanged { project: p.id }),
        "{events:?}"
    );
}

// ---- AutoScheduler (DSNA-61) ----

fn set_auto(dsnap: &Dsnap, p: &Project, mode: AutoSnapshot) {
    dsnap
        .set_project_settings(
            p.id,
            &ProjectSettings {
                auto_snapshot: mode,
                ..p.settings.clone()
            },
        )
        .unwrap();
}

fn versions(dsnap: &Dsnap, p: &Project) -> usize {
    dsnap.list_versions(p.id).unwrap().len()
}

#[test]
fn interval_mode_fires_and_skips_when_clean() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let (_home, dsnap, p) = setup(&fx);
    set_auto(&dsnap, &p, AutoSnapshot::Every { secs: 60 });
    let clock = Arc::new(ManualClock::new());
    let s = AutoScheduler::new(Arc::clone(&dsnap), clock.clone());

    assert!(
        s.tick().unwrap().is_empty(),
        "first tick only starts the timer"
    );
    fs::write(fx.path("a.txt"), "changed").unwrap();
    clock.advance(Duration::from_secs(30));
    assert!(s.tick().unwrap().is_empty());
    clock.advance(Duration::from_secs(31));
    let runs = s.tick().unwrap();
    assert_eq!(runs.len(), 1);
    let v = runs[0].result.as_ref().unwrap().version.clone().unwrap();
    assert_eq!(v.kind, VersionKind::Auto);
    assert_eq!(versions(&dsnap, &p), 2);

    // Clean folder: the next interval writes no version.
    clock.advance(Duration::from_secs(61));
    let runs = s.tick().unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].result.as_ref().unwrap().version.is_none());
    assert_eq!(versions(&dsnap, &p), 2);
}

#[test]
fn idle_mode_waits_for_quiet_and_resets_on_changes() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let (_home, dsnap, p) = setup(&fx);
    set_auto(&dsnap, &p, AutoSnapshot::AfterIdle { secs: 10 });
    let clock = Arc::new(ManualClock::new());
    let s = AutoScheduler::new(Arc::clone(&dsnap), clock.clone());

    fs::write(fx.path("a.txt"), "1").unwrap();
    s.on_change(p.id);
    clock.advance(Duration::from_secs(8));
    assert!(s.tick().unwrap().is_empty());
    fs::write(fx.path("a.txt"), "22").unwrap();
    s.on_change(p.id); // restarts the wait
    clock.advance(Duration::from_secs(8));
    assert!(s.tick().unwrap().is_empty());
    clock.advance(Duration::from_secs(3));
    assert_eq!(s.tick().unwrap().len(), 1);
    assert_eq!(versions(&dsnap, &p), 2);
    // No new change: nothing more.
    clock.advance(Duration::from_secs(60));
    assert!(s.tick().unwrap().is_empty());
}

#[test]
fn settings_changes_and_removal_apply_without_restart() {
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let (_home, dsnap, p) = setup(&fx);
    let clock = Arc::new(ManualClock::new());
    let s = AutoScheduler::new(Arc::clone(&dsnap), clock.clone());
    fs::write(fx.path("a.txt"), "changed").unwrap();
    s.on_change(p.id);
    clock.advance(Duration::from_secs(3600));
    assert!(s.tick().unwrap().is_empty(), "Off by default");

    set_auto(&dsnap, &p, AutoSnapshot::AfterIdle { secs: 5 });
    assert_eq!(
        s.tick().unwrap().len(),
        1,
        "pending change, now idle long enough"
    );

    set_auto(&dsnap, &p, AutoSnapshot::Every { secs: 5 });
    s.tick().unwrap();
    dsnap.remove_project(p.id, false).unwrap();
    clock.advance(Duration::from_secs(10));
    assert!(s.tick().unwrap().is_empty(), "removed project");
}
