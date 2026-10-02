#![allow(clippy::unwrap_used)]

use std::fs;
use std::sync::{Arc, Mutex};

use dsnap_core::{RelPath, VersionRef};
use dsnap_test_support::{FixtureProject, TestHome};

use super::*;

type Events = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

fn backend(home: &TestHome) -> (Backend, Events) {
    let events: Events = Arc::default();
    let sink = Arc::clone(&events);
    let emit: Emit = Arc::new(move |name, v| sink.lock().unwrap().push((name.to_owned(), v)));
    (Backend::open(Some(home.path().to_path_buf()), emit), events)
}

fn named(events: &Events, name: &str) -> Vec<serde_json::Value> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
        .collect()
}

#[test]
fn projects_are_listed_by_name() {
    let home = TestHome::new();
    let (b, _) = backend(&home);
    let zeta = FixtureProject::new().build();
    let alpha = FixtureProject::new().build();
    let z = b.add_project(zeta.root().to_str().unwrap()).unwrap();
    let a = b.add_project(alpha.root().to_str().unwrap()).unwrap();
    b.rename_project(z.id, "zeta").unwrap();
    let a = b.rename_project(a.id, "Alpha").unwrap();
    assert_eq!(a.name, "Alpha");
    let names: Vec<String> = b
        .list_projects()
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["Alpha", "zeta"]);
}

#[test]
fn snapshot_reports_progress_and_versions_changed() {
    let home = TestHome::new();
    let (b, events) = backend(&home);
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let p = b.add_project(fx.root().to_str().unwrap()).unwrap();

    let ctx = b.begin(p.id, Op::Snapshot);
    let first = named(&events, EVENT_PROGRESS);
    assert_eq!(first.len(), 1, "announced before running");
    assert_eq!(first[0]["phase"], "walk");
    assert_eq!(first[0]["opId"], ctx.op_id.as_str());
    assert_eq!(first[0]["op"], "snapshot");

    let r = b.snapshot(&ctx, Some("first".into())).unwrap();
    assert_eq!(r.version.unwrap().label, "first");
    let progress = named(&events, EVENT_PROGRESS);
    assert_eq!(progress.last().unwrap()["phase"], "done");
    let changed = named(&events, EVENT_VERSIONS_CHANGED);
    assert_eq!(changed, [serde_json::json!({ "projectId": p.id.0 })]);
    assert!(
        b.cancel_operation(&ctx.op_id).is_err(),
        "finished ops are forgotten"
    );
}

#[test]
fn a_second_write_is_queued_and_can_be_cancelled() {
    let home = TestHome::new();
    let (b, events) = backend(&home);
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let p = b.add_project(fx.root().to_str().unwrap()).unwrap();

    let held = b.project_lock(p.id);
    let guard = held.lock().unwrap();
    let ctx = b.begin(p.id, Op::Snapshot);
    assert_eq!(named(&events, EVENT_PROGRESS)[0]["phase"], "queued");
    b.cancel_operation(&ctx.op_id).unwrap();
    drop(guard);
    let e = b.snapshot(&ctx, None).unwrap_err();
    assert_eq!(e.code, "cancelled");
    assert!(b.list_versions(p.id).unwrap().is_empty());
    assert_eq!(b.cancel_operation("op-999").unwrap_err().code, "not_found");
}

#[test]
fn restore_and_version_edits() {
    let home = TestHome::new();
    let (b, events) = backend(&home);
    let fx = FixtureProject::new().file("a.txt", "v1").build();
    let p = b.add_project(fx.root().to_str().unwrap()).unwrap();
    let v1 = b
        .snapshot(&b.begin(p.id, Op::Snapshot), None)
        .unwrap()
        .version
        .unwrap();
    fs::write(fx.path("a.txt"), "edited").unwrap();
    assert_eq!(b.status(p.id).unwrap().len(), 1);

    let plan = b.restore_plan(p.id, v1.id).unwrap();
    assert_eq!(plan.write, [RelPath::new("a.txt").unwrap()]);
    let r = b
        .restore_project(&b.begin(p.id, Op::Restore), v1.id)
        .unwrap();
    assert_eq!(fs::read_to_string(fx.path("a.txt")).unwrap(), "v1");
    let phases: Vec<String> = named(&events, EVENT_PROGRESS)
        .iter()
        .filter(|e| e["op"] == "restore")
        .map(|e| e["phase"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(phases, ["walk", "restore", "done"]);

    let v = b.set_label(r.safety_version, "undo point").unwrap();
    assert_eq!(v.label, "undo point");
    assert!(b.set_pinned(v.id, true).unwrap().pinned);
    b.delete_version(v1.id).unwrap();
    assert_eq!(b.list_versions(p.id).unwrap().len(), 1);
    // The latest version is now the safety one ("edited"); the folder holds "v1".
    let changes = b.changes(p.id, None, VersionRef::WorkingTree).unwrap();
    assert_eq!(changes.len(), 1);
}

#[test]
fn image_blobs_become_data_urls() {
    let home = TestHome::new();
    let (b, _) = backend(&home);
    let fx = FixtureProject::new()
        .file("logo.png", [137u8, 80, 78, 71])
        .build();
    let p = b.add_project(fx.root().to_str().unwrap()).unwrap();
    let v = b
        .snapshot(&b.begin(p.id, Op::Snapshot), None)
        .unwrap()
        .version
        .unwrap();
    let changes = b.changes(p.id, None, VersionRef::Version(v.id)).unwrap();
    let hash = changes[0].new.as_ref().unwrap().blob.unwrap();
    assert_eq!(
        b.read_blob_as_data_url(&hash, "image/png").unwrap(),
        "data:image/png;base64,iVBORw=="
    );
    assert_eq!(
        b.read_blob_as_data_url(&hash, "text/html")
            .unwrap_err()
            .code,
        "invalid_input"
    );
}

#[test]
fn an_unopenable_home_is_reported_by_every_command() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("not-a-dir");
    fs::write(&file, "x").unwrap();
    let emit: Emit = Arc::new(|_, _| {});
    let b = Backend::open(Some(file), emit);
    let e = b.list_projects().unwrap_err();
    assert_eq!(e.code, "db");
    assert!(e.message.contains("could not open"), "{}", e.message);
}

#[test]
fn base64_matches_the_standard_alphabet() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(&[0xFB, 0xFF]), "+/8=");
}

#[test]
fn a_write_waits_for_another_process_and_says_so() {
    let home = TestHome::new();
    let (b, events) = backend(&home);
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let p = b.add_project(fx.root().to_str().unwrap()).unwrap();
    let path = b.dsnap().unwrap().project_lock_path(p.id);
    let cli = ProjectLock::try_acquire(&path).unwrap().unwrap();

    let b = Arc::new(b);
    let ctx = b.begin(p.id, Op::Snapshot);
    let worker = {
        let b = Arc::clone(&b);
        std::thread::spawn(move || b.snapshot(&ctx, None))
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    let phases: Vec<String> = named(&events, EVENT_PROGRESS)
        .iter()
        .map(|e| e["phase"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(phases, ["walk", "queued"], "waiting is visible");
    drop(cli);
    assert!(worker.join().unwrap().unwrap().version.is_some());
}
