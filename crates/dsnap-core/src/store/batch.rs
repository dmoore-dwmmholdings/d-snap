//! Storing many new blobs with few rounds of fsyncs (DSNA-102).
//!
//! [`Store::put`] fsyncs and renames each blob before it returns. On Windows that costs
//! 5–20 ms per blob, almost all of it waiting on the flush, so a project's first snapshot of
//! 10,000 small files took a minute or more. A [`PutBatch`] writes and closes each temp file
//! but defers the fsync. Staged files are then flushed in chunks: every file of a chunk is
//! fsynced at once from many threads, which lets the device overlap and coalesce the
//! flushes, and only then renamed into place. A full chunk (1024 files) is flushed by
//! the `put` call that filled it while other threads keep staging, so the slow renames
//! (NTFS also creates an 8.3 short name for each 62-character object name) overlap with
//! staging. [`PutBatch::commit`] flushes the rest.
//!
//! **Durability (Rule 1) and the blob lifetime protocol are unchanged.** A blob appears in
//! the store ([`Store::contains`], readers) only after it was fsynced and renamed, exactly as
//! with `put`. A version may reference a blob from a batch only after `commit` returned `Ok`:
//! only then is every blob of the batch durable and in the store (and, on unix, its shard
//! directory fsynced). `Db::insert_version` then re-checks presence under the write lock as
//! usual.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use super::{SMALL_FILE_LIMIT, Store, TempFile, sync_dir};
use crate::error::{Error, IoResultExt, Result};
use crate::types::{BlobHash, BlobInfo};

/// Staged files that trigger an early flush of the batch.
const CHUNK: usize = 1024;

/// Early flushes that may run at once. Flushes overlap each other's fsyncs and renames.
const MAX_EARLY_FLUSHES: usize = 3;

/// Most threads a flush uses to fsync staged files. A flush mostly waits on the device, and
/// more flushes in flight let it coalesce them.
const SYNC_THREADS: usize = 128;

/// Most threads a flush uses to rename staged files into place.
const RENAME_THREADS: usize = 32;

/// A group of new blobs made durable by [`PutBatch::commit`]. Create one with
/// [`Store::batch`].
///
/// [`PutBatch::put`] and [`PutBatch::put_file`] take `&self` and may be called from many
/// threads at once. Each returns the blob's record right away; the blob is in the store once
/// `commit` returns `Ok` (possibly earlier, see the module docs, but never before it is
/// durable). Do not let a version reference a blob of the batch before `commit` returned
/// `Ok`.
///
/// Dropping a batch without committing removes the temp files still staged; blobs flushed
/// early stay in the store, unreferenced, like objects stored before a failed snapshot.
pub struct PutBatch<'a> {
    store: &'a Store,
    staged: Mutex<HashMap<BlobHash, Staged>>,
    /// Staged files that trigger an early flush ([`CHUNK`]; smaller in tests).
    chunk: usize,
    /// Early flushes running now; at most [`MAX_EARLY_FLUSHES`].
    flushing: AtomicUsize,
    /// First error of an early flush, returned by `commit`.
    error: Mutex<Option<Error>>,
}

/// One blob written to a closed temp file, not yet fsynced or renamed.
struct Staged {
    temp: TempFile,
    stored_size: u64,
}

impl std::fmt::Debug for PutBatch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PutBatch")
            .field("staged", &self.len())
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Start a batch of writes; see [`PutBatch`].
    pub fn batch(&self) -> PutBatch<'_> {
        PutBatch {
            store: self,
            staged: Mutex::new(HashMap::new()),
            chunk: CHUNK,
            flushing: AtomicUsize::new(0),
            error: Mutex::new(None),
        }
    }
}

impl PutBatch<'_> {
    /// Stage `bytes` (nothing is written if the blob is already stored or staged) and return
    /// its record. Like [`Store::put`], but the blob is durable only once
    /// [`PutBatch::commit`] returns `Ok`.
    ///
    /// For a newly staged blob, `stored_size` is the size of the object that will be written.
    /// May flush a full chunk of the batch before returning; an error there is returned by
    /// `commit`, not here.
    pub fn put(&self, bytes: &[u8]) -> Result<BlobInfo> {
        let hash = BlobHash::of(bytes);
        let size = bytes.len() as u64;
        if let Some(stored_size) = self.known(&hash)? {
            return Ok(BlobInfo {
                hash,
                size,
                stored_size,
            });
        }
        let (temp, len) = self.store.stage_bytes(bytes, &hash)?;
        let stored_size = self.add(hash, temp, len)?;
        Ok(BlobInfo {
            hash,
            size,
            stored_size,
        })
    }

    /// Stage a file, read once, as [`Store::put_file`] stores it; see [`PutBatch::put`].
    pub fn put_file(&self, path: &Path) -> Result<BlobInfo> {
        let mut src = File::open(path).at(path)?;
        let len = src.metadata().at(path)?.len();
        if len <= SMALL_FILE_LIMIT {
            let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
            src.read_to_end(&mut bytes).at(path)?;
            return self.put(&bytes);
        }
        let (mut temp, hash, size) = self.store.stage_reader(&mut src, path)?;
        drop(src);
        let stored_size = match self.known(&hash)? {
            Some(stored_size) => stored_size, // `temp` is dropped and removed
            None => {
                let len = temp.file()?.metadata().at(&temp.path)?.len();
                self.add(hash, temp, len)?
            }
        };
        Ok(BlobInfo {
            hash,
            size,
            stored_size,
        })
    }

    /// Number of blobs staged and not yet flushed.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether nothing is waiting to be flushed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Make every blob of the batch durable and put it in the store.
    ///
    /// Flushes what is still staged: fsyncs those temp files at once, renames each into place
    /// (unless its object appeared meanwhile), then on unix fsyncs each shard directory that
    /// received a file. When this returns `Ok`, every blob returned by this batch is durable
    /// and in the store.
    ///
    /// On error, blobs already renamed stay in the store (unreferenced until a version uses
    /// them) and the remaining temp files are removed. The error is the first failure seen,
    /// including one from an early flush.
    pub fn commit(self) -> Result<()> {
        let staged = self
            .staged
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let earlier = self
            .error
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(e) = earlier {
            return Err(e); // `staged` drops and removes its temp files
        }
        flush(self.store, staged.into_iter().collect())
    }

    /// Size of `hash`'s object if it is already staged here or stored.
    fn known(&self, hash: &BlobHash) -> Result<Option<u64>> {
        if let Some(s) = self.lock().get(hash) {
            return Ok(Some(s.stored_size));
        }
        self.store.stored_size(hash)
    }

    /// Close `temp` (the staged object of `hash`, `stored_size` bytes) and add it; returns
    /// the stored size. If another thread staged the same hash meanwhile, `temp` is removed
    /// and that one's size returned. Flushes the staged files if they fill a chunk and no
    /// other flush is running.
    fn add(&self, hash: BlobHash, mut temp: TempFile, stored_size: u64) -> Result<u64> {
        temp.close();
        let (size, full) = {
            let mut staged = self.lock();
            let size = match staged.entry(hash) {
                Entry::Occupied(e) => e.get().stored_size,
                Entry::Vacant(e) => e.insert(Staged { temp, stored_size }).stored_size,
            };
            (size, staged.len() >= self.chunk)
        };
        if full {
            self.flush_early();
        }
        Ok(size)
    }

    /// Flush a full chunk of staged files from a `put` call, unless enough flushes are
    /// running already or an earlier flush failed. Errors are kept for `commit`.
    fn flush_early(&self) {
        if self.flushing.fetch_add(1, Ordering::AcqRel) >= MAX_EARLY_FLUSHES
            || self.error_lock().is_some()
        {
            self.flushing.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        let chunk: Vec<(BlobHash, Staged)> = {
            let mut staged = self.lock();
            // Another thread may have taken the chunk first.
            if staged.len() >= self.chunk {
                staged.drain().collect()
            } else {
                Vec::new()
            }
        };
        if let Err(e) = flush(self.store, chunk) {
            self.error_lock().get_or_insert(e);
        }
        self.flushing.fetch_sub(1, Ordering::AcqRel);
    }

    fn error_lock(&self) -> MutexGuard<'_, Option<Error>> {
        self.error.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<BlobHash, Staged>> {
        self.staged.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Make `staged` durable and put it in the store: fsync all at once, rename, then fsync the
/// shard directories (unix). Temp files not renamed are removed. Returns the first error.
fn flush(store: &Store, mut staged: Vec<(BlobHash, Staged)>) -> Result<()> {
    if staged.is_empty() {
        return Ok(());
    }
    // Neighbours are in different shards, so the threads, which take items in order, rarely
    // rename in the same directory at once.
    staged.sort_by_key(|(hash, _)| (hash.0.get(1).copied(), Store::shard_index(hash)));

    fan_out(staged.len(), SYNC_THREADS, |i| match staged.get(i) {
        Some((_, s)) => sync_temp(&s.temp),
        None => Ok(()),
    })?;

    let slots: Vec<Mutex<(BlobHash, Staged)>> = staged.into_iter().map(Mutex::new).collect();
    let result = fan_out(slots.len(), RENAME_THREADS, |i| {
        let Some(slot) = slots.get(i) else {
            return Ok(());
        };
        let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
        let (hash, staged) = &mut *slot;
        let len = staged.stored_size;
        store
            .install_with(&mut staged.temp, len, hash, &mut |from, to| {
                std::fs::rename(from, to)
            })
            .map(drop)
    });

    // Make the renames durable: one fsync per shard directory that received a file. Temp
    // files not renamed are removed when their slot drops.
    let mut synced = [false; 256];
    for slot in slots {
        let (hash, staged) = slot.into_inner().unwrap_or_else(|p| p.into_inner());
        if let Some(done) = synced.get_mut(Store::shard_index(&hash)) {
            if staged.temp.keep && !*done {
                *done = true;
                sync_dir(&store.shard_of(&hash));
            }
        }
    }
    result
}

/// Fsync a staged (closed) file through a new handle. Both `FlushFileBuffers` (Windows) and
/// `fsync` (unix) flush the file's cached data and metadata, whichever handle wrote it.
/// Closing after the write keeps a large batch far from the open-file limit; keeping the
/// handles open was not faster on windows-latest.
fn sync_temp(temp: &TempFile) -> Result<()> {
    File::options()
        .write(true)
        .open(&temp.path)
        .and_then(|f| f.sync_all())
        .at(&temp.path)
}

/// Run `work(i)` for every `i` in `0..n` on up to `max_threads` threads (the calling thread
/// included); returns the first error. After an error, items not yet started are skipped. If
/// the OS refuses a thread, the others (at least the caller) do its share.
fn fan_out(n: usize, max_threads: usize, work: impl Fn(usize) -> Result<()> + Sync) -> Result<()> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let first_error: Mutex<Option<Error>> = Mutex::new(None);
    let worker = || {
        while !failed.load(Ordering::Relaxed) {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= n {
                break;
            }
            if let Err(e) = work(i) {
                failed.store(true, Ordering::Relaxed);
                let mut slot = first_error.lock().unwrap_or_else(|p| p.into_inner());
                slot.get_or_insert(e);
            }
        }
    };
    std::thread::scope(|scope| {
        for t in 1..max_threads.min(n) {
            let spawned = std::thread::Builder::new()
                .name(format!("dsnap-store-commit-{t}"))
                .stack_size(64 * 1024)
                .spawn_scoped(scope, worker);
            if spawned.is_err() {
                break;
            }
        }
        worker();
    });
    match first_error.into_inner().unwrap_or_else(|p| p.into_inner()) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn io_err(kind: io::ErrorKind) -> Error {
        Error::io("x", io::Error::from(kind))
    }

    fn store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::open(tmp.path().join("objects")).unwrap();
        (tmp, store)
    }

    fn blob(i: usize) -> Vec<u8> {
        format!(
            "blob {i}
"
        )
        .repeat(1 + i % 5)
        .into_bytes()
    }

    /// Temp files left anywhere under the store.
    fn temps(store: &Store) -> usize {
        let mut n = 0;
        let mut dirs = vec![store.dir().to_path_buf()];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(d).unwrap() {
                let e = e.unwrap();
                if e.file_type().unwrap().is_dir() {
                    dirs.push(e.path());
                } else if crate::store::is_temp_name(&e.file_name().to_string_lossy()) {
                    n += 1;
                }
            }
        }
        n
    }

    /// Chunk size for tests, to keep their disk load small.
    const TEST_CHUNK: usize = 32;

    fn small_batch(store: &Store) -> PutBatch<'_> {
        let mut batch = store.batch();
        batch.chunk = TEST_CHUNK;
        batch
    }

    /// A full chunk is flushed by the `put` that fills it; the rest by `commit`.
    #[test]
    fn full_chunks_are_flushed_early() {
        let (_tmp, store) = store();
        let n = 2 * TEST_CHUNK + 10;
        let batch = small_batch(&store);
        let infos: Vec<BlobInfo> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|t| {
                    let batch = &batch;
                    s.spawn(move || {
                        (t..n)
                            .step_by(8)
                            .map(|i| batch.put(&blob(i)).unwrap())
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });
        let early = infos.iter().filter(|i| store.contains(&i.hash)).count();
        assert!(early >= TEST_CHUNK, "only {early} stored before commit");
        assert_eq!(batch.len(), n - early);
        batch.commit().unwrap();
        for (i, info) in infos.iter().enumerate() {
            assert_eq!(info.hash, BlobHash::of(&blob_for(&infos, i)));
            assert!(store.contains(&info.hash));
        }
        assert_eq!(temps(&store), 0);
    }

    /// The bytes behind `infos[i]` (threads interleave, so look them up by hash).
    fn blob_for(infos: &[BlobInfo], i: usize) -> Vec<u8> {
        let hash = infos[i].hash;
        (0..3 * TEST_CHUNK)
            .map(blob)
            .find(|b| BlobHash::of(b) == hash)
            .unwrap()
    }

    /// A failed early flush does not fail the `put`; `commit` reports it and nothing staged
    /// is left behind.
    #[test]
    fn early_flush_error_is_returned_by_commit() {
        let (_tmp, store) = store();
        let batch = small_batch(&store);
        let first = batch.put(&blob(0)).unwrap();
        // Lose the staged temp file, as a stale-temp cleanup would.
        let path = batch.lock().get(&first.hash).unwrap().temp.path.clone();
        std::fs::remove_file(path).unwrap();
        for i in 1..TEST_CHUNK {
            batch.put(&blob(i)).unwrap();
        }
        assert!(batch.is_empty(), "the chunk was taken for flushing");
        let late = batch.put(&blob(TEST_CHUNK)).unwrap();
        let err = batch.commit().unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        assert!(!store.contains(&first.hash));
        assert!(!store.contains(&late.hash), "not flushed after the error");
        assert_eq!(temps(&store), 0);
    }

    #[test]
    fn fan_out_runs_every_item_once() {
        let hits: Vec<AtomicUsize> = (0..1000).map(|_| AtomicUsize::new(0)).collect();
        fan_out(hits.len(), 16, |i| {
            if let Some(h) = hits.get(i) {
                h.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        })
        .unwrap();
        assert!(hits.iter().all(|h| h.load(Ordering::Relaxed) == 1));
        fan_out(0, 16, |_| panic!("no items")).unwrap();
    }

    #[test]
    fn fan_out_returns_the_first_error_and_stops() {
        let done = AtomicUsize::new(0);
        let err = fan_out(10_000, 4, |i| {
            done.fetch_add(1, Ordering::Relaxed);
            if i == 5 {
                Err(io_err(io::ErrorKind::Other))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(matches!(err, Error::Io { .. }));
        assert!(done.load(Ordering::Relaxed) < 10_000, "stopped early");
    }
}
