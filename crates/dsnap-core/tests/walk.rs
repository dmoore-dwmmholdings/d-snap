//! Folder walker (DSNA-34).
#![allow(clippy::unwrap_used, clippy::panic)] // test helpers outside #[test] fns

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use dsnap_core::ignore_rules::IgnoreRules;
use dsnap_core::walk::{PROGRESS_EVERY, WalkOptions, WalkOutput, walk};
use dsnap_core::{
    CancelToken, Entry, EntryKind, Error, Progress, ProjectSettings, SkipReason, Stage,
};
use dsnap_test_support::{FixtureProject, generate_tree, symlinks_supported};

fn rules(root: &Path) -> IgnoreRules {
    IgnoreRules::new(root, &ProjectSettings::default()).unwrap()
}

fn run(root: &Path, opts: &WalkOptions) -> WalkOutput {
    walk(root, &rules(root), opts).unwrap()
}

fn paths(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|e| e.path.as_str()).collect()
}

fn find<'a>(out: &'a WalkOutput, path: &str) -> &'a Entry {
    out.entries
        .iter()
        .find(|e| e.path.as_str() == path)
        .unwrap_or_else(|| panic!("{path} not in {:?}", paths(&out.entries)))
}

#[test]
fn fixture_tree_is_walked_and_filtered() {
    let fx = FixtureProject::new()
        .file("src/main.rs", "fn main() {}")
        .file("src/util/mod.rs", "")
        .file("README.txt", "hi")
        .file("node_modules/pkg/index.js", "x")
        .file(".git/HEAD", "ref: refs/heads/main")
        .file("logs/a.log", "log")
        .file("big.bin", vec![0u8; 2048])
        .dir("empty")
        .dir("nested/empty")
        .file("only_ignored/x.log", "")
        .gitignore(&["*.log"])
        .build();
    let out = run(
        fx.root(),
        &WalkOptions {
            size_cap_bytes: 1024,
            ..Default::default()
        },
    );
    assert_eq!(
        paths(&out.entries),
        [
            ".gitignore",
            "README.txt",
            "empty",
            "logs",
            "nested/empty",
            "only_ignored",
            "src/main.rs",
            "src/util/mod.rs",
        ]
    );
    let main = find(&out, "src/main.rs");
    assert_eq!(main.kind, EntryKind::File);
    assert_eq!(main.size, 12);
    assert!(main.blob.is_none());
    assert!(main.mtime_ns > 0);
    assert!(!main.readonly);
    assert_eq!(find(&out, "empty").kind, EntryKind::Dir);
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].path, "big.bin");
    assert_eq!(out.skipped[0].reason, SkipReason::TooLarge { size: 2048 });
}

#[test]
fn size_cap_zero_means_no_cap_and_cap_is_inclusive() {
    let fx = FixtureProject::new()
        .file("a.bin", vec![1u8; 100])
        .file("b.bin", vec![1u8; 101])
        .build();
    let out = run(fx.root(), &WalkOptions::default());
    assert_eq!(paths(&out.entries), ["a.bin", "b.bin"]);
    let out = run(
        fx.root(),
        &WalkOptions {
            size_cap_bytes: 100,
            ..Default::default()
        },
    );
    assert_eq!(paths(&out.entries), ["a.bin"]);
    assert_eq!(out.skipped[0].reason, SkipReason::TooLarge { size: 101 });
}

#[test]
fn mtime_and_readonly_are_captured() {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let fx = FixtureProject::new()
        .file("a.txt", "a")
        .mtime("a.txt", t)
        .readonly("a.txt")
        .build();
    let out = run(fx.root(), &WalkOptions::default());
    let a = find(&out, "a.txt");
    assert_eq!(a.mtime_ns, 1_700_000_000 * 1_000_000_000);
    assert!(a.readonly);
}

#[test]
fn symlinks_are_recorded_not_followed() {
    if !symlinks_supported() {
        eprintln!("skipping: no symlink privilege");
        return;
    }
    let fx = FixtureProject::new()
        .file("real/inner.txt", "x")
        .symlink("link_dir", "real")
        .symlink("link_file", "real/inner.txt")
        .symlink("dangling", "nowhere")
        .build();
    let out = run(fx.root(), &WalkOptions::default());
    assert_eq!(
        paths(&out.entries),
        ["dangling", "link_dir", "link_file", "real/inner.txt"]
    );
    let target = |p: &str| match &find(&out, p).kind {
        EntryKind::Symlink { target } => target.replace('\\', "/"),
        k => panic!("{p} is {k:?}"),
    };
    assert_eq!(target("link_dir"), "real");
    assert_eq!(target("link_file"), "real/inner.txt");
    assert_eq!(target("dangling"), "nowhere");
    assert!(out.skipped.is_empty(), "{:?}", out.skipped);
}

#[test]
fn symlink_named_like_an_ignored_dir_is_kept() {
    if !symlinks_supported() {
        return;
    }
    // Links count as files for ignore rules (git semantics), so `target/` does not match.
    let fx = FixtureProject::new()
        .file("real/x", "x")
        .symlink("target", "real")
        .build();
    let out = run(fx.root(), &WalkOptions::default());
    assert_eq!(paths(&out.entries), ["real/x", "target"]);
}

#[test]
fn nested_gitignore_and_extra_rules_apply() {
    let fx = FixtureProject::new()
        .file("a/.gitignore", "*.tmp\n")
        .file("a/x.tmp", "")
        .file("a/keep.txt", "")
        .file("x.tmp", "")
        .file("secret/key", "")
        .build();
    let settings = ProjectSettings {
        extra_ignore: vec!["/secret/".into()],
        ..ProjectSettings::default()
    };
    let rules = IgnoreRules::new(fx.root(), &settings).unwrap();
    let out = walk(fx.root(), &rules, &WalkOptions::default()).unwrap();
    assert_eq!(paths(&out.entries), ["a/.gitignore", "a/keep.txt", "x.tmp"]);
}

#[test]
fn output_is_sorted_and_deterministic() {
    let fx = FixtureProject::new().build();
    generate_tree(fx.root(), 600, 42);
    std::fs::create_dir_all(fx.path("zz/empty")).unwrap();
    let a = run(fx.root(), &WalkOptions::default());
    let mut sorted = a.entries.clone();
    sorted.sort_by(|x, y| x.path.cmp(&y.path));
    assert_eq!(a.entries, sorted);
    assert_eq!(a.entries.len(), 601);
    for _ in 0..3 {
        let b = run(fx.root(), &WalkOptions::default());
        assert_eq!(a.entries, b.entries);
    }
}

#[test]
fn progress_is_reported() {
    let fx = FixtureProject::new().build();
    generate_tree(fx.root(), 600, 7);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let opts = WalkOptions {
        progress: Some(Progress::new(move |e| sink.lock().unwrap().push(e.clone()))),
        ..Default::default()
    };
    let out = run(fx.root(), &opts);
    let events = events.lock().unwrap();
    assert!(events.iter().all(|e| e.stage == Stage::Walk));
    let periodic: Vec<_> = events.iter().filter(|e| e.path.is_some()).collect();
    assert!(periodic.len() >= 2, "{} periodic events", periodic.len());
    assert!(periodic.iter().all(|e| e.done % PROGRESS_EVERY == 0));
    let last = events.last().unwrap();
    assert!(last.path.is_none());
    assert!(last.done >= out.entries.len() as u64);
}

#[test]
fn cancel_before_start() {
    let fx = FixtureProject::new().file("a", "a").build();
    let cancel = CancelToken::new();
    cancel.cancel();
    let opts = WalkOptions {
        cancel: Some(cancel),
        ..Default::default()
    };
    let err = walk(fx.root(), &rules(fx.root()), &opts).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
}

#[test]
fn cancel_during_walk() {
    let fx = FixtureProject::new().build();
    generate_tree(fx.root(), 1000, 3);
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let opts = WalkOptions {
        cancel: Some(cancel),
        progress: Some(Progress::new(move |_| trigger.cancel())),
        ..Default::default()
    };
    let err = walk(fx.root(), &rules(fx.root()), &opts).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
}

#[test]
fn missing_or_file_root_is_an_io_error() {
    let fx = FixtureProject::new().file("f", "x").build();
    let missing = fx.path("nope");
    let err = walk(&missing, &rules(&missing), &WalkOptions::default()).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    let file = fx.path("f");
    let err = walk(&file, &rules(&file), &WalkOptions::default()).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
}

#[test]
fn empty_project_has_no_entries() {
    let fx = FixtureProject::new().build();
    let out = run(fx.root(), &WalkOptions::default());
    assert!(out.entries.is_empty());
    assert!(out.skipped.is_empty());
}

#[cfg(unix)]
#[test]
fn non_utf8_name_is_skipped() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let fx = FixtureProject::new().file("ok.txt", "").build();
    let bad = fx.root().join(OsStr::from_bytes(b"bad\xff.txt"));
    if std::fs::write(&bad, "x").is_err() {
        eprintln!("skipping: filesystem rejects non-UTF-8 names");
        return;
    }
    let out = run(fx.root(), &WalkOptions::default());
    assert_eq!(paths(&out.entries), ["ok.txt"]);
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].reason, SkipReason::NonUtf8Name);
    assert!(out.skipped[0].path.starts_with("bad"));
}

#[cfg(windows)]
#[test]
fn non_utf16_name_is_skipped() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let fx = FixtureProject::new().file("ok.txt", "").build();
    // Unpaired surrogate: valid NTFS name, not valid UTF-8.
    let name = OsString::from_wide(&[b'b' as u16, 0xD800, b'.' as u16, b't' as u16]);
    std::fs::write(fx.root().join(name), "x").unwrap();
    let out = run(fx.root(), &WalkOptions::default());
    assert_eq!(paths(&out.entries), ["ok.txt"]);
    assert_eq!(out.skipped[0].reason, SkipReason::NonUtf8Name);
}

#[cfg(unix)]
#[test]
fn name_rejected_by_relpath_is_skipped_as_unreadable() {
    // Backslash is legal in unix names but never valid in a RelPath component.
    let fx = FixtureProject::new().file("ok.txt", "").build();
    std::fs::write(fx.root().join("a\\b"), "x").unwrap();
    let out = run(fx.root(), &WalkOptions::default());
    assert_eq!(paths(&out.entries), ["ok.txt"]);
    assert!(matches!(
        out.skipped[0].reason,
        SkipReason::Unreadable { .. }
    ));
    assert_eq!(out.skipped[0].path, "a\\b");
}

#[cfg(unix)]
#[test]
fn unreadable_dir_is_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let fx = FixtureProject::new()
        .file("locked/secret", "x")
        .file("ok", "")
        .build();
    let locked = fx.path("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        // Running as root: permissions are not enforced.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let out = run(fx.root(), &WalkOptions::default());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.skipped.len(), 1, "{:?}", out.skipped);
    assert_eq!(out.skipped[0].path, "locked");
    assert!(matches!(
        out.skipped[0].reason,
        SkipReason::Unreadable { .. }
    ));
    assert!(paths(&out.entries).contains(&"ok"));
}

/// Windows specifics (DSNA-35).
#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::fs::MetadataExt;
    use std::process::Command;

    /// Create an NTFS junction (needs no privilege, unlike symlinks).
    fn junction(link: &Path, target: &Path) {
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
    }

    #[test]
    fn junction_is_recorded_and_not_followed() {
        let fx = FixtureProject::new()
            .file("real/inner.txt", "x")
            .file("real/node_modules/pkg.js", "")
            .build();
        junction(&fx.path("jn"), &fx.path("real"));
        // A junction back to the root would loop forever if followed.
        junction(&fx.path("real/loop"), fx.root());
        let out = run(fx.root(), &WalkOptions::default());
        assert_eq!(paths(&out.entries), ["jn", "real/inner.txt", "real/loop"]);
        for p in ["jn", "real/loop"] {
            let e = find(&out, p);
            let EntryKind::Symlink { target } = &e.kind else {
                panic!("{p} is {:?}", e.kind);
            };
            assert!(!target.is_empty(), "{p}");
        }
        let EntryKind::Symlink { target } = &find(&out, "jn").kind else {
            unreachable!()
        };
        assert!(target.ends_with("real"), "{target}");
        assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    }

    #[test]
    fn junction_is_not_ignored_as_a_dir() {
        let fx = FixtureProject::new().file("real/a", "").build();
        junction(&fx.path("dist"), &fx.path("real"));
        let out = run(fx.root(), &WalkOptions::default());
        assert_eq!(paths(&out.entries), ["dist", "real/a"]);
    }

    #[test]
    fn long_path_is_walked() {
        let fx = FixtureProject::new().build();
        let mut rel = String::new();
        for i in 0..12 {
            if !rel.is_empty() {
                rel.push('/');
            }
            rel.push_str(&format!("{i:02}_{}", "d".repeat(22)));
        }
        let file_rel = format!("{rel}/file_{}.txt", "f".repeat(20));
        let abs = fx.path(&file_rel);
        assert!(abs.as_os_str().len() > 300, "{}", abs.as_os_str().len());
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, "deep").unwrap();
        std::fs::create_dir(fx.path(&format!("{rel}/empty_{}", "e".repeat(20)))).unwrap();

        let out = run(fx.root(), &WalkOptions::default());
        assert!(out.skipped.is_empty(), "{:?}", out.skipped);
        let ps = paths(&out.entries);
        assert_eq!(ps.len(), 2, "{ps:?}");
        assert!(ps.contains(&file_rel.as_str()));
        assert!(ps.iter().all(|p| !p.contains('\\') && !p.starts_with("//")));
        assert_eq!(find(&out, &file_rel).size, 4);
    }

    #[test]
    fn long_path_gitignore_is_read() {
        let fx = FixtureProject::new().build();
        let deep = format!("{}/{}", "a".repeat(150), "b".repeat(150));
        std::fs::create_dir_all(fx.path(&deep)).unwrap();
        std::fs::write(fx.path(&format!("{deep}/.gitignore")), "*.tmp\n").unwrap();
        std::fs::write(fx.path(&format!("{deep}/x.tmp")), "").unwrap();
        std::fs::write(fx.path(&format!("{deep}/x.txt")), "").unwrap();
        let out = run(fx.root(), &WalkOptions::default());
        assert_eq!(
            paths(&out.entries),
            [format!("{deep}/.gitignore"), format!("{deep}/x.txt")]
        );
    }

    #[test]
    fn readonly_attribute_is_captured() {
        const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
        let fx = FixtureProject::new()
            .file("ro.txt", "r")
            .file("rw.txt", "w")
            .readonly("ro.txt")
            .build();
        let attrs = std::fs::metadata(fx.path("ro.txt"))
            .unwrap()
            .file_attributes();
        assert_ne!(attrs & FILE_ATTRIBUTE_READONLY, 0);
        let out = run(fx.root(), &WalkOptions::default());
        assert!(find(&out, "ro.txt").readonly);
        assert!(!find(&out, "rw.txt").readonly);
    }
}
