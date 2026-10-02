//! M1 acceptance (DSNA-65): snapshot → agent-like edit storm → restore through the `dsnap`
//! binary gives a byte-identical tree, and the safety version undoes the restore.
//!
//! Release-only (`--ignored`): it copies this repository's source plus a generated
//! 10,000-file tree.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use dsnap_core::RelPath;
use dsnap_test_support::{TestHome, generate_tree, tree_bytes, tree_dirs};

fn dsnap(home: &TestHome, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_dsnap"))
        .arg("--home")
        .arg(home.path())
        .args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn json(home: &TestHome, args: &[&str]) -> serde_json::Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let (code, out, err) = dsnap(home, &all);
    assert_eq!(code, 0, "dsnap {args:?}: {err}");
    serde_json::from_str(&out).unwrap()
}

/// Files, directories and read-only flags: everything a restore must bring back.
#[derive(Debug, PartialEq, Eq)]
struct State {
    files: std::collections::BTreeMap<RelPath, Vec<u8>>,
    dirs: BTreeSet<RelPath>,
    readonly: BTreeSet<RelPath>,
}

fn state(root: &Path) -> State {
    let files = tree_bytes(root);
    let readonly = files
        .keys()
        .filter(|p| fs::symlink_metadata(p.to_path(root)).is_ok_and(|m| m.permissions().readonly()))
        .cloned()
        .collect();
    State {
        files,
        dirs: tree_dirs(root),
        readonly,
    }
}

fn assert_same(a: &State, b: &State, what: &str) {
    if a == b {
        return;
    }
    let diff: Vec<_> = a
        .files
        .keys()
        .chain(b.files.keys())
        .filter(|k| a.files.get(*k) != b.files.get(*k))
        .take(10)
        .collect();
    panic!(
        "{what}: trees differ; first differing files {diff:?}; dirs only in one: {:?}; readonly a {:?} b {:?}",
        a.dirs
            .symmetric_difference(&b.dirs)
            .take(10)
            .collect::<Vec<_>>(),
        a.readonly,
        b.readonly
    );
}

/// Copy `src` to `dst`, skipping build output and VCS folders.
fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap().flatten() {
        let name = e.file_name();
        let n = name.to_string_lossy();
        if matches!(n.as_ref(), "target" | ".git" | "node_modules" | "dist") {
            continue;
        }
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            copy_tree(&e.path(), &dst.join(&name));
        } else if ft.is_file() {
            fs::copy(e.path(), dst.join(&name)).unwrap();
        }
    }
}

/// Write `bytes` and move the mtime forward, as an editor would some time later.
fn edit(abs: &Path, bytes: &[u8]) {
    let old = fs::metadata(abs).and_then(|m| m.modified()).ok();
    fs::write(abs, bytes).unwrap();
    let t = old.map_or(SystemTime::now(), |o| o.max(SystemTime::now())) + Duration::from_secs(2);
    filetime::set_file_mtime(abs, filetime::FileTime::from_system_time(t)).unwrap();
}

fn set_readonly(abs: &Path, ro: bool) {
    let mut p = fs::metadata(abs).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    p.set_readonly(ro);
    fs::set_permissions(abs, p).unwrap();
}

/// Build the original project: this repo's source, a 10k-file tree and a few special cases.
fn original(root: &Path) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    copy_tree(&repo.join("crates"), &root.join("crates"));
    copy_tree(&repo.join("app").join("src"), &root.join("app").join("src"));
    generate_tree(&root.join("gen"), 10_000, 65);
    fs::create_dir_all(root.join("assets")).unwrap();
    let bin: Vec<u8> = (0..65_536u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    fs::write(root.join("assets/blob.bin"), bin).unwrap();
    fs::create_dir_all(root.join("empty/old")).unwrap();
    fs::write(root.join("locked-docs.txt"), "read only\n").unwrap();
    set_readonly(&root.join("locked-docs.txt"), true);
    fs::write(root.join("unix.txt"), "one\ntwo\nthree\n").unwrap();
}

/// An agent-like burst of edits.
fn edit_storm(root: &Path) {
    // Modify.
    for i in (0..300).step_by(10) {
        let p = root.join(format!(
            "gen/d{:02}/s{:02}/f{i:06}.txt",
            i % 17,
            (i / 17) % 13
        ));
        let mut b = fs::read(&p).unwrap();
        b.extend_from_slice(b"agent edit\n");
        edit(&p, &b);
    }
    let lib = root.join("crates/dsnap-core/src/lib.rs");
    let mut b = fs::read(&lib).unwrap();
    b.splice(0..0, b"// agent was here\n".iter().copied());
    edit(&lib, &b);
    // Add.
    fs::create_dir_all(root.join("new/module")).unwrap();
    fs::write(root.join("new/module/mod.rs"), "pub fn added() {}\n").unwrap();
    fs::write(root.join("new/data.bin"), [0u8, 255, 1, 254, 0, 0, 7]).unwrap();
    // Delete a file and a whole directory.
    fs::remove_file(root.join("crates/dsnap-core/src/error.rs")).unwrap();
    fs::remove_dir_all(root.join("gen/d03")).unwrap();
    // Rename a file and a directory.
    fs::rename(root.join("unix.txt"), root.join("renamed.txt")).unwrap();
    fs::rename(root.join("gen/d04"), root.join("gen/d04-moved")).unwrap();
    // Case-only rename (one file on Windows, a delete + add elsewhere).
    fs::rename(
        root.join("gen/d00/s00/f000000.txt"),
        root.join("gen/d00/s00/F000000.txt"),
    )
    .unwrap();
    // CRLF flip.
    let cargo = root.join("crates/dsnap-core/Cargo.toml");
    let text = fs::read_to_string(&cargo).unwrap().replace('\n', "\r\n");
    edit(&cargo, text.as_bytes());
    // Binary edit.
    let blob = root.join("assets/blob.bin");
    let mut b = fs::read(&blob).unwrap();
    for x in b.iter_mut().step_by(97) {
        *x ^= 0xFF;
    }
    edit(&blob, &b);
    // Directories: a new empty one, and the old empty one removed.
    fs::create_dir_all(root.join("empty/new")).unwrap();
    fs::remove_dir(root.join("empty/old")).unwrap();
    // Read-only toggles.
    set_readonly(&root.join("locked-docs.txt"), false);
    edit(&root.join("locked-docs.txt"), b"now writable and edited\n");
    set_readonly(&root.join("gen/d01/s00/f000001.txt"), true);
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Drop for Project {
    fn drop(&mut self) {
        // Let the temp dir go away on Windows.
        let _ = Command::new("attrib")
            .args(["-R", "/S", "/D"])
            .arg(self.root.join("*"))
            .output();
    }
}

fn project() -> Project {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    original(&root);
    Project { _tmp: tmp, root }
}

#[test]
#[ignore = "release-only acceptance; run with --release -- --ignored"]
fn m1_round_trip_is_byte_identical() {
    let home = TestHome::new();
    let p = project();
    let root = p.root.to_str().unwrap();
    let before = state(&p.root);

    let t = Instant::now();
    let first = json(&home, &["snap", root, "-m", "original"]);
    eprintln!("first snapshot: {:?}", t.elapsed());
    let first = first["version"]["id"].as_i64().unwrap().to_string();

    edit_storm(&p.root);
    let edited = state(&p.root);
    let t = Instant::now();
    let second = json(&home, &["snap", root, "-m", "after storm"]);
    eprintln!("snapshot after storm: {:?}", t.elapsed());
    assert!(second["version"].is_object(), "the storm is a change");

    let t = Instant::now();
    let report = json(&home, &["restore", root, &first, "--yes"]);
    eprintln!("restore: {:?}", t.elapsed());
    assert_eq!(report["failed"], serde_json::json!([]), "{report}");
    assert_eq!(report["uncaptured"], serde_json::json!([]), "{report}");
    assert_same(&state(&p.root), &before, "restore to the original");

    let safety = report["safetyVersion"].as_i64().unwrap().to_string();
    let undo = json(&home, &["restore", root, &safety, "--yes"]);
    assert_eq!(undo["failed"], serde_json::json!([]), "{undo}");
    assert_same(&state(&p.root), &edited, "undo via the safety version");
}

#[cfg(windows)]
#[test]
#[ignore = "release-only acceptance; run with --release -- --ignored"]
fn m1_locked_file_gives_a_partial_restore_that_undoes() {
    use std::os::windows::fs::OpenOptionsExt;

    let home = TestHome::new();
    let p = project();
    let root = p.root.to_str().unwrap();
    let first = json(&home, &["snap", root, "-m", "original"]);
    let first = first["version"]["id"].as_i64().unwrap().to_string();
    edit_storm(&p.root);
    let edited = state(&p.root);
    json(&home, &["snap", root, "-m", "after storm"]);

    let held = fs::File::options()
        .read(true)
        .share_mode(1) // FILE_SHARE_READ: readable, not replaceable
        .open(p.root.join("assets/blob.bin"))
        .unwrap();
    let (code, out, err) = dsnap(&home, &["--json", "restore", root, &first, "--yes"]);
    assert_eq!(code, 3, "partial restore exit code: {err}");
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    let failed = report["failed"].as_array().unwrap();
    assert!(failed.iter().any(|f| f[0] == "assets/blob.bin"), "{report}");
    drop(held);

    let safety = report["safetyVersion"].as_i64().unwrap().to_string();
    let undo = json(&home, &["restore", root, &safety, "--yes"]);
    assert_eq!(undo["failed"], serde_json::json!([]), "{undo}");
    assert_same(&state(&p.root), &edited, "undo of a partial restore");
}
