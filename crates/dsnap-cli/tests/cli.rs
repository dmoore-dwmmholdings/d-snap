//! `dsnap` end to end on fixture projects with a temporary home (DSNA-63).
#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use dsnap_test_support::{FixtureProject, TestHome};

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn dsnap(home: &TestHome, args: &[&str]) -> Run {
    let o: Output = Command::new(env!("CARGO_BIN_EXE_dsnap"))
        .arg("--home")
        .arg(home.path())
        .args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Run {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn ok(home: &TestHome, args: &[&str]) -> String {
    let r = dsnap(home, args);
    assert_eq!(
        r.code, 0,
        "dsnap {args:?}\nstdout: {}\nstderr: {}",
        r.out, r.err
    );
    r.out
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// A project with one CLI snapshot; returns its version id.
fn snapped(home: &TestHome, root: &Path) -> i64 {
    let out = ok(home, &["--json", "snap", s(root), "-m", "first"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["version"]["kind"], "cli");
    v["version"]["id"].as_i64().unwrap()
}

#[test]
fn snap_tracks_the_folder_and_reports_no_changes() {
    let home = TestHome::new();
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let out = ok(&home, &["snap", s(fx.root()), "-m", "first"]);
    assert!(
        out.starts_with("Saved version 1 \"first\" (+1 ~0 -0)"),
        "{out}"
    );
    let out = ok(&home, &["snap", s(fx.root())]);
    assert!(out.contains("No changes since version 1"), "{out}");
    fs::write(fx.path("b.txt"), "b").unwrap();
    assert_eq!(ok(&home, &["snap", "--quiet", s(fx.root())]), "");
    let list = ok(&home, &["projects"]);
    assert_eq!(list.lines().count(), 1, "{list}");
}

#[test]
fn snap_inside_a_project_uses_that_project() {
    let home = TestHome::new();
    let fx = FixtureProject::new().file("src/a.rs", "a").build();
    snapped(&home, fx.root());
    let out = ok(&home, &["snap", s(&fx.path("src"))]);
    assert!(out.contains("No changes"), "{out}");
}

#[test]
fn list_status_and_diff() {
    let home = TestHome::new();
    let fx = FixtureProject::new().file("a.txt", "one\ntwo\n").build();
    let v1 = snapped(&home, fx.root());
    fs::write(fx.path("a.txt"), "one\nTWO\n").unwrap();
    fs::write(fx.path("new.txt"), "n").unwrap();

    let status = ok(&home, &["status", s(fx.root())]);
    assert!(status.contains("M a.txt"), "{status}");
    assert!(status.contains("A new.txt"), "{status}");

    let diff = ok(&home, &["diff", s(fx.root()), "--file", "a.txt"]);
    assert!(diff.contains("-two\n+TWO"), "{diff}");

    ok(&home, &["snap", s(fx.root()), "-m", "second"]);
    let list = ok(&home, &["list", s(fx.root())]);
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 2, "{list}");
    assert!(
        lines[0].contains("second") && lines[0].contains("cli"),
        "{list}"
    );
    assert_eq!(
        ok(&home, &["list", s(fx.root()), "-n", "1"])
            .lines()
            .count(),
        1
    );

    let between = ok(&home, &["diff", s(fx.root()), &v1.to_string(), "2"]);
    assert!(
        between.contains("M a.txt") && between.contains("A new.txt"),
        "{between}"
    );
    let json = ok(&home, &["--json", "diff", s(fx.root())]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap(),
        serde_json::json!([])
    );
}

#[test]
fn restore_needs_yes_then_restores_and_prints_undo() {
    let home = TestHome::new();
    let fx = FixtureProject::new().file("a.txt", "v1").build();
    let v1 = snapped(&home, fx.root());
    fs::write(fx.path("a.txt"), "edited").unwrap();
    fs::write(fx.path("added.txt"), "x").unwrap();
    let v = v1.to_string();

    let dry = ok(&home, &["restore", s(fx.root()), &v, "--dry-run"]);
    assert!(
        dry.contains("write  a.txt") && dry.contains("delete added.txt"),
        "{dry}"
    );

    let r = dsnap(&home, &["restore", s(fx.root()), &v]);
    assert_eq!(r.code, 2, "no --yes and no terminal: {}", r.err);
    assert_eq!(fs::read_to_string(fx.path("a.txt")).unwrap(), "edited");

    let out = ok(&home, &["restore", s(fx.root()), &v, "--yes"]);
    assert!(out.contains("Restored: 1 written, 1 deleted."), "{out}");
    assert!(out.contains("Undo with: dsnap restore"), "{out}");
    assert_eq!(fs::read_to_string(fx.path("a.txt")).unwrap(), "v1");
    assert!(!fx.path("added.txt").exists());
}

#[test]
fn restore_one_file_with_json() {
    let home = TestHome::new();
    let fx = FixtureProject::new()
        .file("a.txt", "v1")
        .file("b.txt", "b")
        .build();
    let v1 = snapped(&home, fx.root());
    fs::write(fx.path("a.txt"), "edited").unwrap();
    fs::write(fx.path("b.txt"), "b edited").unwrap();
    let out = ok(
        &home,
        &[
            "--json",
            "restore",
            s(fx.root()),
            &v1.to_string(),
            "--file",
            "a.txt",
            "-y",
        ],
    );
    let r: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r["written"], serde_json::json!(["a.txt"]));
    assert!(r["safetyVersion"].as_i64().unwrap() > v1);
    assert_eq!(fs::read_to_string(fx.path("a.txt")).unwrap(), "v1");
    assert_eq!(fs::read_to_string(fx.path("b.txt")).unwrap(), "b edited");
}

#[test]
fn projects_add_rename_remove() {
    let home = TestHome::new();
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let out = ok(&home, &["projects", "add", s(fx.root()), "--name", "demo"]);
    assert!(out.contains("as \"demo\" (id 1)"), "{out}");
    ok(&home, &["projects", "rename", "1", "renamed"]);
    assert!(ok(&home, &["projects", "list"]).contains("renamed"));
    ok(&home, &["projects", "remove", s(fx.root())]);
    assert_eq!(ok(&home, &["projects"]), "No projects tracked.\n");
    assert!(fx.path("a.txt").exists(), "folder untouched");
}

#[test]
fn errors_and_usage_have_their_exit_codes() {
    let home = TestHome::new();
    let fx = FixtureProject::new().file("a.txt", "a").build();
    let r = dsnap(&home, &["status", s(fx.root())]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("is not tracked"), "{}", r.err);
    assert_eq!(dsnap(&home, &["frobnicate"]).code, 2);
    assert_eq!(dsnap(&home, &["restore", s(fx.root())]).code, 2);
}

#[cfg(windows)]
#[test]
fn partial_restore_exits_3() {
    use std::os::windows::fs::OpenOptionsExt;
    let home = TestHome::new();
    let fx = FixtureProject::new()
        .file("a.txt", "a1")
        .file("b.txt", "b1")
        .build();
    let v1 = snapped(&home, fx.root());
    fs::write(fx.path("a.txt"), "a2").unwrap();
    fs::write(fx.path("b.txt"), "b2").unwrap();
    let _held = fs::File::options()
        .read(true)
        .share_mode(1) // FILE_SHARE_READ: readable, not replaceable
        .open(fx.path("b.txt"))
        .unwrap();
    let r = dsnap(&home, &["restore", s(fx.root()), &v1.to_string(), "-y"]);
    assert_eq!(r.code, 3, "{}\n{}", r.out, r.err);
    assert!(r.out.contains("Not restored: b.txt"), "{}", r.out);
}
