//! Blob store GC primitives and integrity check (DSNA-29).
#![allow(clippy::unwrap_used)] // helpers outside #[test] fns are test code too

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dsnap_core::store::{Store, SweepReport, TEMP_GRACE};
use dsnap_core::{BlobHash, Error};
use filetime::FileTime;
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path().join("objects")).unwrap();
    (tmp, store)
}

fn blob(i: u32) -> Vec<u8> {
    format!("blob number {i}\n").repeat(50).into_bytes()
}

/// Write a fake temp file with an mtime `age` in the past.
fn temp_file(dir: &Path, name: &str, age: Duration) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, b"partial write").unwrap();
    let t = SystemTime::now() - age;
    filetime::set_file_mtime(&p, FileTime::from_system_time(t)).unwrap();
    p
}

#[test]
fn list_hashes_returns_sorted_objects_only() {
    let (_tmp, store) = store();
    assert!(store.list_hashes().unwrap().is_empty());
    let mut hashes: Vec<BlobHash> = (0..20).map(|i| store.put(&blob(i)).unwrap().hash).collect();
    hashes.sort();

    // Noise that is not an object.
    temp_file(store.dir(), ".tmp-1-2-3", Duration::ZERO);
    fs::write(store.dir().join("README"), b"x").unwrap();
    fs::create_dir_all(store.dir().join("zz")).unwrap();
    fs::write(store.dir().join("zz").join("y".repeat(62)), b"x").unwrap();
    let shard = store.path_of(&hashes[0]).parent().unwrap().to_path_buf();
    fs::write(shard.join("short"), b"x").unwrap();

    assert_eq!(store.list_hashes().unwrap(), hashes);
}

#[test]
fn sweep_keeps_referenced_and_young_temp_files_and_deletes_the_rest() {
    let (_tmp, store) = store();
    let infos: Vec<_> = (0..10).map(|i| store.put(&blob(i)).unwrap()).collect();
    let keep: HashSet<BlobHash> = infos.iter().step_by(2).map(|b| b.hash).collect();
    let young = temp_file(store.dir(), ".tmp-young", Duration::from_secs(60));
    let old = temp_file(
        store.dir(),
        ".tmp-old",
        TEMP_GRACE + Duration::from_secs(60),
    );
    let old_len = fs::metadata(&old).unwrap().len();

    let report = store.sweep(&keep).unwrap();
    let dropped: u64 = infos
        .iter()
        .filter(|b| !keep.contains(&b.hash))
        .map(|b| b.stored_size)
        .sum();
    assert_eq!(
        report,
        SweepReport {
            deleted: 5,
            temp_removed: 1,
            bytes_freed: dropped + old_len,
            failed: 0,
        }
    );
    for b in &infos {
        assert_eq!(store.contains(&b.hash), keep.contains(&b.hash));
    }
    assert!(young.exists());
    assert!(!old.exists());
    let mut listed: Vec<_> = keep.into_iter().collect();
    listed.sort();
    assert_eq!(store.list_hashes().unwrap(), listed);

    // Nothing left to do.
    let set: HashSet<_> = listed.into_iter().collect();
    assert_eq!(store.sweep(&set).unwrap(), SweepReport::default());
}

#[test]
fn sweep_with_empty_set_deletes_every_object() {
    let (_tmp, store) = store();
    for i in 0..3 {
        store.put(&blob(i)).unwrap();
    }
    assert_eq!(store.sweep(&HashSet::new()).unwrap().deleted, 3);
    assert!(store.list_hashes().unwrap().is_empty());
}

#[test]
fn clean_temp_removes_only_stale_temp_files() {
    let (_tmp, store) = store();
    let info = store.put(&blob(1)).unwrap();
    let young = temp_file(store.dir(), ".tmp-young", Duration::from_secs(10));
    let old = temp_file(store.dir(), ".tmp-old", Duration::from_secs(2 * 3600));
    let report = store.clean_temp().unwrap();
    assert_eq!(report.temp_removed, 1);
    assert_eq!(report.deleted, 0);
    assert!(young.exists() && !old.exists());
    assert!(store.contains(&info.hash));
}

#[test]
fn verify_all_finds_corrupted_objects() {
    let (_tmp, store) = store();
    let infos: Vec<_> = (0..5).map(|i| store.put(&blob(i)).unwrap()).collect();
    assert!(store.verify_all().unwrap().is_empty());

    // Wrong content in a valid frame, and a non-zstd file.
    fs::write(
        store.path_of(&infos[1].hash),
        zstd::encode_all(&b"tampered"[..], 3).unwrap(),
    )
    .unwrap();
    fs::write(store.path_of(&infos[3].hash), b"garbage").unwrap();

    let bad = store.verify_all().unwrap();
    let mut got: Vec<_> = bad.iter().map(|(h, _)| *h).collect();
    got.sort();
    let mut want = vec![infos[1].hash, infos[3].hash];
    want.sort();
    assert_eq!(got, want);
    assert!(bad.iter().all(|(_, e)| matches!(e, Error::Corrupt(_))));
}

#[test]
fn primitives_work_on_a_missing_directory() {
    let (_tmp, store) = store();
    fs::remove_dir_all(store.dir()).unwrap();
    assert!(store.list_hashes().unwrap().is_empty());
    assert_eq!(
        store.sweep(&HashSet::new()).unwrap(),
        SweepReport::default()
    );
    assert!(store.verify_all().unwrap().is_empty());
}
