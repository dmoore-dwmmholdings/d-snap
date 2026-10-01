//! Blob store (DSNA-28, DSNA-81).
#![allow(clippy::unwrap_used)] // helpers outside #[test] fns are test code too

use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Barrier};

use dsnap_core::store::{Store, hash_bytes, hash_file};
use dsnap_core::{BlobHash, Error};
use filetime::FileTime;
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let tmp = TempDir::new().unwrap();
    let store = Store::open(tmp.path().join("objects")).unwrap();
    (tmp, store)
}

/// Deterministic pseudo-random bytes with some repetition, so zstd has work to do.
fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let b = x.to_le_bytes();
        // Every other block is text-like, which compresses.
        if (out.len() / 4096) % 2 == 0 {
            out.extend_from_slice(&b);
        } else {
            out.extend_from_slice(b"fn main() {}\n");
        }
    }
    out.truncate(len);
    out
}

/// Files under the store dir: (objects, temp files).
fn census(store: &Store) -> (Vec<String>, Vec<String>) {
    let mut objects = Vec::new();
    let mut temps = Vec::new();
    for e in fs::read_dir(store.dir()).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        if e.file_type().unwrap().is_dir() {
            for f in fs::read_dir(e.path()).unwrap() {
                let f = f.unwrap().file_name().into_string().unwrap();
                objects.push(format!("{name}{f}"));
            }
        } else {
            temps.push(name);
        }
    }
    objects.sort();
    (objects, temps)
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn open_creates_the_directory() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("a").join("objects");
    let store = Store::open(dir.clone()).unwrap();
    assert!(dir.is_dir());
    assert_eq!(store.dir(), dir);
}

#[test]
fn bytes_round_trip() {
    let (_tmp, store) = store();
    let bytes = b"hello, blob store\n".repeat(100);
    let info = store.put(&bytes).unwrap();
    assert_eq!(info.hash, BlobHash::of(&bytes));
    assert_eq!(info.hash, hash_bytes(&bytes));
    assert_eq!(info.size, bytes.len() as u64);
    assert!(store.contains(&info.hash));

    let path = store.path_of(&info.hash);
    let hex = info.hash.to_string();
    assert_eq!(path, store.dir().join(&hex[..2]).join(&hex[2..]));
    assert_eq!(fs::metadata(&path).unwrap().len(), info.stored_size);
    assert!(info.stored_size < info.size, "compressed");
    // Stored as a plain zstd frame.
    assert_eq!(
        zstd::decode_all(&fs::read(&path).unwrap()[..]).unwrap(),
        bytes
    );

    assert_eq!(store.get(&info.hash).unwrap(), bytes);
    let mut streamed = Vec::new();
    store
        .open_reader(&info.hash)
        .unwrap()
        .read_to_end(&mut streamed)
        .unwrap();
    assert_eq!(streamed, bytes);
    let mut copied = Vec::new();
    assert_eq!(
        store.copy_to(&info.hash, &mut copied).unwrap(),
        bytes.len() as u64
    );
    assert_eq!(copied, bytes);
    assert_eq!(census(&store), (vec![hex], vec![]));
}

#[test]
fn small_and_streamed_files_round_trip() {
    let (tmp, store) = store();
    for (i, len) in [1usize, 1000, 1 << 20, (1 << 20) + 1, 3 << 20]
        .into_iter()
        .enumerate()
    {
        let bytes = data(len, i as u64 + 7);
        let p = write(tmp.path(), &format!("f{i}"), &bytes);
        let info = store.put_file(&p).unwrap();
        assert_eq!(info.hash, BlobHash::of(&bytes), "len {len}");
        assert_eq!(info.size, len as u64);
        assert_eq!(hash_file(&p).unwrap(), (info.hash, len as u64));
        assert_eq!(
            fs::metadata(store.path_of(&info.hash)).unwrap().len(),
            info.stored_size
        );
        assert_eq!(store.get(&info.hash).unwrap(), bytes);
        // put() of the same bytes finds the streamed object.
        assert_eq!(store.put(&bytes).unwrap(), info);
    }
    assert!(census(&store).1.is_empty(), "no temp files left");
}

#[test]
fn second_put_writes_nothing() {
    let (tmp, store) = store();
    let bytes = data(10_000, 1);
    let big = data(2 << 20, 2);
    let p = write(tmp.path(), "big", &big);
    let a = store.put(&bytes).unwrap();
    let b = store.put_file(&p).unwrap();

    let old = FileTime::from_unix_time(1_000_000_000, 0);
    for h in [a.hash, b.hash] {
        filetime::set_file_mtime(store.path_of(&h), old).unwrap();
    }
    let before = census(&store);

    assert_eq!(store.put(&bytes).unwrap(), a);
    assert_eq!(store.put_file(&p).unwrap(), b);
    let small = write(tmp.path(), "small", &bytes);
    assert_eq!(store.put_file(&small).unwrap(), a);

    for h in [a.hash, b.hash] {
        let m = fs::metadata(store.path_of(&h)).unwrap();
        assert_eq!(
            FileTime::from_last_modification_time(&m),
            old,
            "not rewritten"
        );
    }
    assert_eq!(census(&store), before);
}

#[test]
fn empty_blob() {
    let (tmp, store) = store();
    let info = store.put(b"").unwrap();
    assert_eq!(info.hash, BlobHash::of(b""));
    assert_eq!(info.size, 0);
    assert!(info.stored_size > 0, "a zstd frame is never empty");
    assert_eq!(store.get(&info.hash).unwrap(), Vec::<u8>::new());

    let p = write(tmp.path(), "empty", b"");
    assert_eq!(store.put_file(&p).unwrap(), info);
    assert_eq!(hash_file(&p).unwrap(), (info.hash, 0));
    let mut out = Vec::new();
    assert_eq!(store.copy_to(&info.hash, &mut out).unwrap(), 0);
}

#[test]
fn sixty_megabyte_file() {
    let (tmp, store) = store();
    let bytes = data(60 << 20, 42);
    let p = write(tmp.path(), "big.bin", &bytes);
    let info = store.put_file(&p).unwrap();
    assert_eq!(info.size, 60 << 20);
    assert_eq!(info.hash, BlobHash::of(&bytes));
    assert_eq!(hash_file(&p).unwrap(), (info.hash, 60 << 20));

    let mut out = Vec::new();
    assert_eq!(store.copy_to(&info.hash, &mut out).unwrap(), 60 << 20);
    assert!(out == bytes);
    assert!(store.get(&info.hash).unwrap() == bytes);
}

#[test]
fn corrupt_objects_are_detected() {
    let (_tmp, store) = store();
    let bytes = data(300_000, 3);
    let info = store.put(&bytes).unwrap();
    let path = store.path_of(&info.hash);

    // Valid zstd, wrong content.
    let mut other = bytes.clone();
    other[1234] ^= 1;
    fs::write(&path, zstd::encode_all(&other[..], 3).unwrap()).unwrap();
    assert_corrupt(&store, &info.hash);

    // Not a zstd frame at all.
    fs::write(&path, b"garbage").unwrap();
    assert_corrupt(&store, &info.hash);

    // Truncated frame.
    let full = zstd::encode_all(&bytes[..], 3).unwrap();
    fs::write(&path, &full[..full.len() / 2]).unwrap();
    assert_corrupt(&store, &info.hash);
}

fn assert_corrupt(store: &Store, hash: &BlobHash) {
    assert!(matches!(store.get(hash), Err(Error::Corrupt(_))));
    let mut sink = Vec::new();
    assert!(matches!(
        store.copy_to(hash, &mut sink),
        Err(Error::Corrupt(_))
    ));
    let err = store
        .open_reader(hash)
        .unwrap()
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn missing_blob_is_not_found() {
    let (_tmp, store) = store();
    let h = BlobHash::of(b"never stored");
    assert!(!store.contains(&h));
    assert!(matches!(store.get(&h), Err(Error::NotFound(_))));
    assert!(matches!(store.open_reader(&h), Err(Error::NotFound(_))));
    assert!(matches!(
        store.copy_to(&h, &mut Vec::new()),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn missing_source_file_is_an_io_error() {
    let (tmp, store) = store();
    let p = tmp.path().join("nope");
    assert!(matches!(store.put_file(&p), Err(Error::Io { .. })));
    assert!(matches!(hash_file(&p), Err(Error::Io { .. })));
}

#[test]
fn concurrent_puts_of_the_same_blob_are_safe() {
    let (tmp, store) = store();
    let store = Arc::new(store);
    let small = Arc::new(data(500_000, 9));
    let big = data(3 << 20, 10);
    let big_path = write(tmp.path(), "big", &big);

    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|i| {
            let (store, small, barrier, big_path) = (
                store.clone(),
                small.clone(),
                barrier.clone(),
                big_path.clone(),
            );
            std::thread::spawn(move || {
                barrier.wait();
                if i % 2 == 0 {
                    (
                        store.put(&small).unwrap(),
                        store.put_file(&big_path).unwrap(),
                    )
                } else {
                    (
                        store.put_file(&big_path).unwrap(),
                        store.put(&small).unwrap(),
                    )
                }
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let s = BlobHash::of(&small);
    let b = BlobHash::of(&big);
    for (x, y) in results {
        let (si, bi) = if x.hash == s { (x, y) } else { (y, x) };
        assert_eq!((si.hash, bi.hash), (s, b));
        assert_eq!(
            si.stored_size,
            fs::metadata(store.path_of(&s)).unwrap().len()
        );
        assert_eq!(
            bi.stored_size,
            fs::metadata(store.path_of(&b)).unwrap().len()
        );
    }
    let (objects, temps) = census(&store);
    assert_eq!(objects.len(), 2);
    assert!(temps.is_empty(), "{temps:?}");
    assert_eq!(store.get(&s).unwrap(), *small);
    assert_eq!(store.get(&b).unwrap(), big);
}

#[test]
fn delete_returns_bytes_freed_and_is_idempotent() {
    let (_tmp, store) = store();
    let info = store.put(&data(50_000, 5)).unwrap();
    assert_eq!(store.delete(&info.hash).unwrap(), info.stored_size);
    assert!(!store.contains(&info.hash));
    assert_eq!(store.delete(&info.hash).unwrap(), 0);
    assert_eq!(store.delete(&BlobHash::of(b"never")).unwrap(), 0);
}

#[test]
fn put_after_delete_recreates_the_blob() {
    let (tmp, store) = store();
    let bytes = data(50_000, 6);
    let big = data(2 << 20, 8);
    let p = write(tmp.path(), "big", &big);
    let a = store.put(&bytes).unwrap();
    let b = store.put_file(&p).unwrap();
    store.delete(&a.hash).unwrap();
    store.delete(&b.hash).unwrap();
    assert_eq!(store.put(&bytes).unwrap(), a);
    assert_eq!(store.put_file(&p).unwrap(), b);
    assert_eq!(store.get(&a.hash).unwrap(), bytes);
    assert_eq!(store.get(&b.hash).unwrap(), big);
}
