//! DSNA-106: the blob lifetime protocol (DSNA-80) says only `Db` deletes blob files, under
//! its write lock. A list-then-delete prune outside the lock is a race that tests almost
//! never hit, so this test forbids the calls themselves outside `db` and `store`.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn only_db_deletes_blob_files() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 10, "found {} files", files.len());

    const FORBIDDEN: &[&str] = &[
        "Store::delete",
        "Store::sweep",
        "store.delete(",
        "store.sweep(",
        "BlobFiles::delete",
    ];
    let mut violations = Vec::new();
    let mut unreferenced_calls = Vec::new();
    for f in &files {
        let rel = f
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "db.rs" || rel.starts_with("db/") || rel.starts_with("store/") {
            continue;
        }
        for (n, line) in fs::read_to_string(f).unwrap().lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for pat in FORBIDDEN {
                if code.contains(pat) {
                    violations.push(format!("{rel}:{}: {pat}", n + 1));
                }
            }
            if code.contains("unreferenced_blobs(") {
                unreferenced_calls.push(format!("{rel}:{}", n + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "blob files deleted outside Db: {violations:?}"
    );
    // The one allowed call counts undeletable blobs for the report; it deletes nothing.
    assert!(
        unreferenced_calls.len() == 1 && unreferenced_calls[0].starts_with("retention.rs:"),
        "unreferenced_blobs() used outside the blobs_failed count: {unreferenced_calls:?}"
    );
}
