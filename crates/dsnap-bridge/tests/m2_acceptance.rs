//! M2 acceptance (DSNA-69), scripted: the P0 flows through `Backend`, the code every app
//! command runs, on a temp copy of this repository's source. Each step names its F number.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dsnap_bridge::backend::{Backend, EVENT_PROGRESS, EVENT_VERSIONS_CHANGED, Emit, Op};
use dsnap_core::{ChangeStatus, DiffBody, DiffOptions, RelPath, VersionKind, VersionRef};
use dsnap_test_support::{TestHome, tree_bytes};

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap().flatten() {
        let name = e.file_name();
        if matches!(name.to_str(), Some("target" | "node_modules" | ".git")) {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst.join(&name));
        } else {
            fs::copy(e.path(), dst.join(&name)).unwrap();
        }
    }
}

fn rp(s: &str) -> RelPath {
    RelPath::new(s).unwrap()
}

#[test]
fn m2_p0_flows_through_the_app_backend() {
    let home = TestHome::new();
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&events);
    let emit: Emit = Arc::new(move |name, _| sink.lock().unwrap().push(name.to_owned()));
    let b = Backend::open(Some(home.path().to_path_buf()), emit);

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dsnap-src");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../dsnap-core"),
        &root,
    );
    fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    fs::write(root.join("node_modules/pkg/index.js"), "ignored").unwrap();
    let original = tree_bytes(&root);

    // F1: add a project.
    let p = b.add_project(root.to_str().unwrap()).unwrap();
    assert_eq!(b.list_projects().unwrap().len(), 1);

    // F4, F5, F7: snapshot; ignored paths are skipped; no changes → no version.
    let first = b
        .snapshot(&b.begin(p.id, Op::Snapshot), Some("first".into()))
        .unwrap();
    let v1 = first.version.unwrap();
    assert_eq!(v1.kind, VersionKind::Manual);
    assert!(
        b.status(p.id).unwrap().is_empty(),
        "F5: node_modules ignored"
    );
    let again = b.snapshot(&b.begin(p.id, Op::Snapshot), None).unwrap();
    assert!(again.version.is_none(), "F7");

    // F6: an over-cap file is skipped and reported.
    let mut g = b.get_global_settings().unwrap();
    g.size_cap_bytes = 1024 * 1024;
    b.set_global_settings(&g).unwrap();
    fs::write(root.join("big.bin"), vec![0u8; 2 * 1024 * 1024]).unwrap();

    // Edit like an agent.
    fs::write(root.join("src/lib.rs"), "// rewritten by an agent\n").unwrap();
    fs::remove_file(root.join("Cargo.toml")).unwrap();
    fs::write(root.join("src/new_module.rs"), "pub fn added() {}\n").unwrap();

    // F13: unsaved changes with statuses and line counts.
    let changes = b.status(p.id).unwrap();
    let find = |path: &str| changes.iter().find(|c| c.path.as_str() == path).unwrap();
    assert_eq!(find("src/lib.rs").status, ChangeStatus::Modified);
    assert_eq!(find("Cargo.toml").status, ChangeStatus::Deleted);
    assert_eq!(find("src/new_module.rs").status, ChangeStatus::Added);

    // F14: line diff of a file.
    let d = b
        .file_diff(
            p.id,
            None,
            VersionRef::WorkingTree,
            &rp("src/lib.rs"),
            &DiffOptions::default(),
        )
        .unwrap();
    let DiffBody::Text { hunks } = d.body else {
        panic!("text diff")
    };
    assert!(!hunks.is_empty());

    let second = b
        .snapshot(&b.begin(p.id, Op::Snapshot), Some("edited".into()))
        .unwrap();
    assert!(second.skipped.iter().any(|s| s.path == "big.bin"), "F6");
    let v2 = second.version.unwrap();

    // F10: versions newest first with counts.
    let versions = b.list_versions(p.id).unwrap();
    assert_eq!(versions[0].id, v2.id);
    assert_eq!(versions[0].counts.added, 1);

    // F19 + F21: restore one file (a deleted one); a safety version comes first.
    let r = b
        .restore_file(&b.begin(p.id, Op::Restore), v1.id, &rp("Cargo.toml"))
        .unwrap();
    assert_eq!(r.written, [rp("Cargo.toml")]);
    let safety = b.list_versions(p.id).unwrap()[0].clone();
    assert_eq!(safety.id, r.safety_version);
    assert_eq!(safety.kind, VersionKind::Safety);

    // F20: restore the project; big.bin (over the cap, in no version) is left alone.
    let edited = tree_bytes(&root);
    let plan = b.restore_plan(p.id, v1.id).unwrap();
    assert!(plan.delete.contains(&rp("src/new_module.rs")));
    assert!(plan.uncaptured.contains(&rp("big.bin")));
    let r = b
        .restore_project(&b.begin(p.id, Op::Restore), v1.id)
        .unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    let mut now = tree_bytes(&root);
    assert!(now.remove(&rp("big.bin")).is_some(), "uncaptured file kept");
    assert_eq!(now, original, "byte-identical to the first snapshot");

    // F21: undo via the safety version.
    b.restore_project(&b.begin(p.id, Op::Restore), r.safety_version)
        .unwrap();
    assert_eq!(tree_bytes(&root), edited);

    // F2: rename, then remove the project with its snapshots; the folder stays.
    assert_eq!(b.rename_project(p.id, "renamed").unwrap().name, "renamed");
    b.remove_project(p.id, true).unwrap();
    assert!(b.list_projects().unwrap().is_empty());
    assert!(root.join("src/lib.rs").exists());

    let names = events.lock().unwrap();
    assert!(names.iter().any(|n| n == EVENT_PROGRESS));
    assert!(names.iter().any(|n| n == EVENT_VERSIONS_CHANGED));
}
