//! Content-addressed blob store: `objects/ab/cdef...`, zstd-compressed. Owner: Chain C (DSNA-6).
//!
//! A blob is named by the BLAKE3 hash of its uncompressed bytes: the first two hex digits pick
//! the shard directory, the other 62 name the file. Each object is one zstd frame (level 3).
//!
//! Writes go to a uniquely named temp file (`XXXXXXXX.TMP`), are fsynced, then renamed into
//! place. A reader or [`Store::contains`] therefore never sees a partial blob. If the object
//! already exists the temp file is dropped (dedupe); two writers racing on the same hash both
//! succeed, because the content is identical. A blob whose hash is known before it is
//! compressed (up to 1 MiB) is staged in its shard directory, so parallel writers do not all
//! create and rename files in one directory; a larger, streamed blob is staged directly under
//! `objects/`.
//!
//! Many new blobs (a project's first snapshot) are cheaper through a [`PutBatch`]: it defers
//! the fsyncs and issues them all at once in [`PutBatch::commit`] (DSNA-102).
//!
//! Reads verify the hash of the decompressed bytes and return [`Error::Corrupt`] on mismatch.
//! Dedupe trusts any non-empty object, so damage found by [`Store::verify_all`] is fixed with
//! [`Store::repair`] / [`Store::repair_file`], which rewrite the object in place.
//!
//! Deleting blobs follows the blob lifetime protocol in the [`crate::db`] module docs.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, IoResultExt, Result};
use crate::facade::Dsnap;
use crate::types::{BlobHash, BlobInfo};

/// zstd compression level for new objects.
pub const ZSTD_LEVEL: i32 = 3;

/// Files up to this size are read into memory and compressed only if their hash is new.
/// Larger files are hashed and compressed in one streaming pass.
const SMALL_FILE_LIMIT: u64 = 1 << 20;

/// Read buffer for streaming.
const BUF_SIZE: usize = 256 * 1024;

/// File-name prefix of in-flight writes made by earlier builds. Still recognized as temp
/// files by the scans in `gc`.
const LEGACY_TEMP_PREFIX: &str = ".tmp-";

/// Extension of in-flight writes: `XXXXXXXX.TMP`, 8 uppercase hex digits.
const TEMP_EXT: &str = ".TMP";

/// Whether `name` is an in-flight write (a temp file) rather than an object.
pub(crate) fn is_temp_name(name: &str) -> bool {
    if name.starts_with(LEGACY_TEMP_PREFIX) {
        return true;
    }
    match name.strip_suffix(TEMP_EXT) {
        Some(stem) => {
            stem.len() == 8 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'F'))
        }
        None => false,
    }
}

mod batch;
mod gc;
mod repair;

pub use batch::PutBatch;
pub use gc::{SweepReport, TEMP_GRACE};
pub use repair::RepairOutcome;

/// Blob store rooted at the `objects` directory.
pub struct Store {
    pub(crate) dir: PathBuf,
    /// Shard directories this handle has created or seen, by first hash byte. Only a cache:
    /// a shard deleted behind our back is recreated when a write finds it missing.
    shards: [AtomicBool; 256],
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Open the store at `dir`, creating it if missing.
    pub fn open(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir).at(&dir)?;
        Ok(Self {
            dir,
            shards: std::array::from_fn(|_| AtomicBool::new(false)),
        })
    }

    /// The `objects` directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Store `bytes` (no-op if the hash already exists) and return its record.
    ///
    /// Writes go to a temp file renamed into place, so a reader never sees a partial blob.
    /// An existing 0-length object is damage and is replaced. Other damage (truncation,
    /// bitrot) is not detected here; [`Store::verify_all`] finds it and [`Store::repair`]
    /// fixes it.
    /// The no-op path is safe only because `Db::insert_version` re-checks blob presence under
    /// the write lock (see the `db` module docs).
    pub fn put(&self, bytes: &[u8]) -> Result<BlobInfo> {
        let hash = BlobHash::of(bytes);
        let size = bytes.len() as u64;
        if let Some(stored_size) = self.stored_size(&hash)? {
            return Ok(BlobInfo {
                hash,
                size,
                stored_size,
            });
        }
        let (temp, _) = self.stage_bytes(bytes, &hash)?;
        let stored_size = self.persist(temp, &hash)?;
        Ok(BlobInfo {
            hash,
            size,
            stored_size,
        })
    }

    /// Stream a file into the store (no-op if the hash already exists) and return its record.
    ///
    /// The file is read once. `size` is the number of bytes read, which is what the hash
    /// covers even if the file changes on disk meanwhile. Symlinks are followed; callers that
    /// store links store the link target with [`Store::put`].
    pub fn put_file(&self, path: &Path) -> Result<BlobInfo> {
        let mut src = File::open(path).at(path)?;
        let len = src.metadata().at(path)?.len();
        if len <= SMALL_FILE_LIMIT {
            let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
            src.read_to_end(&mut bytes).at(path)?;
            return self.put(&bytes);
        }

        let (temp, hash, size) = self.stage_reader(&mut src, path)?;
        let stored_size = self.persist(temp, &hash)?;
        Ok(BlobInfo {
            hash,
            size,
            stored_size,
        })
    }

    /// Compress `src` into a new temp file in one pass; returns it with the hash and size of
    /// the bytes read. `path` names `src` in errors.
    fn stage_reader(&self, src: &mut impl Read, path: &Path) -> Result<(TempFile, BlobHash, u64)> {
        let mut temp = self.temp_file()?;
        let temp_path = temp.path.clone();
        let mut hasher = blake3::Hasher::new();
        let mut size = 0u64;
        {
            let out = BufWriter::with_capacity(BUF_SIZE, temp.file()?);
            let mut enc = zstd::Encoder::new(out, ZSTD_LEVEL).at(&temp_path)?;
            let mut buf = vec![0u8; BUF_SIZE];
            loop {
                let n = match src.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Error::io(path, e)),
                };
                let chunk = buf.get(..n).unwrap_or_default();
                hasher.update(chunk);
                enc.write_all(chunk).at(&temp_path)?;
                size += n as u64;
            }
            let mut out = enc.finish().at(&temp_path)?;
            out.flush().at(&temp_path)?;
        }
        Ok((temp, BlobHash::from(hasher.finalize()), size))
    }

    /// Compress `bytes` (whose hash is `hash`) into a new temp file in `hash`'s shard
    /// directory; returns it with the compressed size.
    fn stage_bytes(&self, bytes: &[u8], hash: &BlobHash) -> Result<(TempFile, u64)> {
        let compressed = zstd::bulk::compress(bytes, ZSTD_LEVEL)
            .map_err(|e| Error::Corrupt(format!("zstd compression failed: {e}")))?;
        let mut temp = self.shard_temp_file(hash)?;
        let temp_path = temp.path.clone();
        temp.file()?.write_all(&compressed).at(&temp_path)?;
        Ok((temp, compressed.len() as u64))
    }

    /// Read and decompress a blob, verifying its hash ([`crate::Error::Corrupt`] on mismatch).
    ///
    /// A missing blob is [`crate::Error::NotFound`].
    pub fn get(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        let path = self.path_of(hash);
        let compressed = fs::read(&path).map_err(|e| open_error(hash, &path, e))?;
        let bytes = zstd::stream::decode_all(compressed.as_slice())
            .map_err(|e| Error::Corrupt(format!("blob {hash}: cannot decompress: {e}")))?;
        if BlobHash::of(&bytes) != *hash {
            return Err(corrupt_hash(hash));
        }
        Ok(bytes)
    }

    /// Open a streaming, hash-verifying reader over a blob's decompressed bytes.
    ///
    /// The hash is checked when the reader reaches the end; a mismatch or a bad zstd frame is
    /// an [`io::ErrorKind::InvalidData`] error from `read`. A failure to read the object file
    /// itself keeps its own kind (see [`BlobReader::io_failed`]). Do not trust the bytes read
    /// until the reader has returned `Ok(0)`. A missing blob is [`crate::Error::NotFound`].
    pub fn open_reader(&self, hash: &BlobHash) -> Result<BlobReader> {
        let path = self.path_of(hash);
        let file = File::open(&path).map_err(|e| open_error(hash, &path, e))?;
        BlobReader::new(Box::new(file), path, *hash)
    }

    /// Decompress a blob into `writer`, verifying its hash; returns the bytes written.
    ///
    /// Bad data is [`crate::Error::Corrupt`]. Some bytes may already be written by then, so
    /// write to a temp file and discard it on error. A failure to read the object file is
    /// [`crate::Error::Io`] with the object's path (the blob may be fine; retry later). A
    /// write failure is [`crate::Error::Io`] with the path `<output>`; callers that know the
    /// destination path can use [`Store::open_reader`] with their own copy loop.
    pub fn copy_to<W: Write + ?Sized>(&self, hash: &BlobHash, writer: &mut W) -> Result<u64> {
        let mut reader = self.open_reader(hash)?;
        copy_reader(&mut reader, writer)
    }

    /// Whether a blob exists. One `stat`, no read; cheap enough to call under the DB write
    /// lock. A 0-length object (always damaged) does not count.
    pub fn contains(&self, hash: &BlobHash) -> bool {
        matches!(self.stored_size(hash), Ok(Some(_)))
    }

    /// Delete a blob; returns the bytes freed on disk (0 if it did not exist).
    ///
    /// Only [`crate::db::Db::prune_unreferenced`] may call this, inside its write transaction.
    /// Deleting a blob anywhere else can lose snapshot data (see the `db` module docs).
    pub fn delete(&self, hash: &BlobHash) -> Result<u64> {
        remove_file_len(&self.path_of(hash))
    }

    /// On-disk location of a blob.
    pub fn path_of(&self, hash: &BlobHash) -> PathBuf {
        let hex = hash.to_string();
        let (shard, rest) = hex.split_at(2);
        self.dir.join(shard).join(rest)
    }

    /// Compressed size of a stored blob, or `None` if it is not stored.
    ///
    /// A 0-length object counts as not stored: no zstd frame is empty, so it is damage (e.g. a
    /// partial copy of the data dir), and the next `put` of the same content replaces it.
    pub(crate) fn stored_size(&self, hash: &BlobHash) -> Result<Option<u64>> {
        let path = self.path_of(hash);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_file() && m.len() == 0 => Ok(None),
            Ok(m) if m.is_file() => Ok(Some(m.len())),
            Ok(_) => Err(Error::Corrupt(format!(
                "{} is not a regular file",
                path.display()
            ))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Index of `hash`'s shard directory in [`Store::shards`].
    fn shard_index(hash: &BlobHash) -> usize {
        usize::from(hash.0.first().copied().unwrap_or(0))
    }

    /// The shard directory of `hash`.
    fn shard_of(&self, hash: &BlobHash) -> PathBuf {
        let dest = self.path_of(hash);
        dest.parent().unwrap_or(&self.dir).to_path_buf()
    }

    /// The shard directory of `hash`, created if this handle has not seen it yet.
    fn ensure_shard(&self, hash: &BlobHash) -> Result<PathBuf> {
        let shard = self.shard_of(hash);
        let seen = self.shards.get(Self::shard_index(hash));
        if !seen.is_some_and(|s| s.load(Ordering::Acquire)) {
            fs::create_dir_all(&shard).at(&shard)?;
            if let Some(s) = seen {
                s.store(true, Ordering::Release);
            }
        }
        Ok(shard)
    }

    /// Forget that `hash`'s shard exists (a write found it missing) and create it again.
    fn recreate_shard(&self, hash: &BlobHash) -> Result<PathBuf> {
        if let Some(s) = self.shards.get(Self::shard_index(hash)) {
            s.store(false, Ordering::Release);
        }
        self.ensure_shard(hash)
    }

    /// Create a new temp file in `hash`'s shard directory.
    fn shard_temp_file(&self, hash: &BlobHash) -> Result<TempFile> {
        let shard = self.ensure_shard(hash)?;
        match temp_file_in(&shard) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let shard = self.recreate_shard(hash)?;
                temp_file_in(&shard).at(&shard)
            }
            other => other.at(&shard),
        }
    }

    /// Create a new, uniquely named temp file directly under `objects/`.
    fn temp_file(&self) -> Result<TempFile> {
        temp_file_in(&self.dir).at(&self.dir)
    }

    /// Fsync `temp` and move it to `hash`'s object path; returns the stored size.
    ///
    /// If the object already exists (stored earlier or by a concurrent writer) the temp file
    /// is removed and the existing object's size returned.
    fn persist(&self, temp: TempFile, hash: &BlobHash) -> Result<u64> {
        self.persist_with(temp, hash, |from, to| fs::rename(from, to))
    }

    /// [`Store::persist`] with an injectable rename, so tests can reach the failure branches.
    fn persist_with(
        &self,
        mut temp: TempFile,
        hash: &BlobHash,
        mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
    ) -> Result<u64> {
        let temp_path = temp.path.clone();
        let file = temp.file()?;
        file.sync_all().at(&temp_path)?;
        let len = file.metadata().at(&temp_path)?.len();
        temp.close();
        let stored = self.install_with(&mut temp, len, hash, &mut rename)?;
        if temp.keep {
            sync_dir(&self.shard_of(hash));
        }
        Ok(stored)
    }

    /// Close the already fsynced `temp` (`len` bytes) and rename it to `hash`'s object path,
    /// unless the object exists; returns the stored size. Sets `temp.keep` if it was renamed;
    /// the caller then makes the rename durable with [`sync_dir`] on the shard.
    fn install_with(
        &self,
        temp: &mut TempFile,
        len: u64,
        hash: &BlobHash,
        rename: &mut impl FnMut(&Path, &Path) -> io::Result<()>,
    ) -> Result<u64> {
        temp.close();
        if let Some(existing) = self.stored_size(hash)? {
            return Ok(existing);
        }
        let dest = self.path_of(hash);
        self.ensure_shard(hash)?;

        let mut attempt = 0u64;
        let mut recreated = false;
        loop {
            match rename(&temp.path, &dest) {
                Ok(()) => {
                    temp.keep = true;
                    return Ok(len);
                }
                Err(e) => {
                    // A concurrent writer may have won the race (Windows refuses to replace a
                    // file that is open); its object has the same content.
                    if let Some(existing) = self.stored_size(hash)? {
                        return Ok(existing);
                    }
                    // The shard was removed behind our back (a temp under `objects/` survives
                    // that; one inside the shard did not, and the retry fails again).
                    if e.kind() == io::ErrorKind::NotFound && !recreated {
                        recreated = true;
                        self.recreate_shard(hash)?;
                        continue;
                    }
                    // Transient sharing violations on Windows (e.g. a virus scanner).
                    if e.kind() == io::ErrorKind::PermissionDenied && attempt < 5 {
                        attempt += 1;
                        std::thread::sleep(std::time::Duration::from_millis(10 * attempt));
                        continue;
                    }
                    return Err(Error::io(dest, e));
                }
            }
        }
    }
}

/// Create a new, uniquely named temp file in `dir`.
///
/// The name is a valid 8.3 name (`XXXXXXXX.TMP`), so NTFS volumes that create short names
/// (usually the system volume) need not make one for it; that is a large part of the cost of
/// creating a file there. The 32 random-looking bits come from the process id, a counter and
/// the clock; a clash is retried.
fn temp_file_in(dir: &Path) -> io::Result<TempFile> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = u64::from(std::process::id()).rotate_left(32) ^ (nanos as u64);
    let mut attempt = 0;
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("{:08X}{TEMP_EXT}", mix(seed ^ n) as u32);
        let path = dir.join(name);
        match File::options().write(true).create_new(true).open(&path) {
            Ok(file) => {
                return Ok(TempFile {
                    path,
                    file: Some(file),
                    keep: false,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempt < 16 => {
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// splitmix64 finalizer: spreads `x` over all 64 bits.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Hash a file's bytes without storing it; returns the hash and size.
pub fn hash_file(path: &Path) -> Result<(BlobHash, u64)> {
    let file = File::open(path).at(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(BufReader::with_capacity(BUF_SIZE, file))
        .at(path)?;
    Ok((BlobHash::from(hasher.finalize()), hasher.count()))
}

/// Hash bytes without storing them (same as [`BlobHash::of`]).
pub fn hash_bytes(bytes: &[u8]) -> BlobHash {
    BlobHash::of(bytes)
}

/// Copy loop behind [`Store::copy_to`].
fn copy_reader<W: Write + ?Sized>(reader: &mut BlobReader, writer: &mut W) -> Result<u64> {
    let mut buf = vec![0u8; BUF_SIZE];
    let mut total = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if reader.io_failed() => return Err(Error::io(&reader.path, e)),
            Err(e) => return Err(Error::Corrupt(format!("blob {}: {e}", reader.expected))),
        };
        writer
            .write_all(buf.get(..n).unwrap_or_default())
            .at("<output>")?;
        total += n as u64;
    }
    writer.flush().at("<output>")?;
    Ok(total)
}

/// The object file under a [`BlobReader`]'s decoder. Records the last real I/O error, so
/// it is not reported as bad data after the decoder wraps it.
struct ErrorTap {
    inner: Box<dyn Read + Send>,
    last: Option<io::Error>,
}

impl Read for ErrorTap {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf).inspect_err(|e| {
            if e.kind() != io::ErrorKind::Interrupted {
                self.last = Some(io::Error::new(e.kind(), e.to_string()));
            }
        })
    }
}

/// Streaming reader over a blob's decompressed bytes; see [`Store::open_reader`].
pub struct BlobReader {
    decoder: zstd::Decoder<'static, BufReader<ErrorTap>>,
    hasher: blake3::Hasher,
    expected: BlobHash,
    path: PathBuf,
    verified: bool,
    io_failed: bool,
}

impl BlobReader {
    fn new(source: Box<dyn Read + Send>, path: PathBuf, expected: BlobHash) -> Result<Self> {
        let tap = ErrorTap {
            inner: source,
            last: None,
        };
        let decoder =
            zstd::Decoder::with_buffer(BufReader::with_capacity(BUF_SIZE, tap)).at(&path)?;
        Ok(Self {
            decoder,
            hasher: blake3::Hasher::new(),
            expected,
            path,
            verified: false,
            io_failed: false,
        })
    }

    /// Whether the last error from `read` came from reading the object file (an I/O
    /// failure, with its original kind) rather than from bad data
    /// ([`io::ErrorKind::InvalidData`]).
    pub fn io_failed(&self) -> bool {
        self.io_failed
    }
}

impl std::fmt::Debug for BlobReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobReader")
            .field("expected", &self.expected)
            .field("verified", &self.verified)
            .finish_non_exhaustive()
    }
}

impl Read for BlobReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.io_failed = false;
        self.decoder.get_mut().get_mut().last = None;
        let n = match self.decoder.read(buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(e),
            Err(e) => {
                if let Some(io_err) = self.decoder.get_mut().get_mut().last.take() {
                    self.io_failed = true;
                    return Err(io_err);
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("cannot decompress: {e}"),
                ));
            }
        };
        if n == 0 {
            if !self.verified {
                if BlobHash::from(self.hasher.finalize()) != self.expected {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "content does not match its hash",
                    ));
                }
                self.verified = true;
            }
            return Ok(0);
        }
        self.hasher.update(buf.get(..n).unwrap_or_default());
        Ok(n)
    }
}

impl Dsnap {
    /// Read a blob's bytes (e.g. for an image preview).
    pub fn read_blob(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        self.store.get(hash)
    }
}

fn open_error(hash: &BlobHash, path: &Path, e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::NotFound {
        Error::NotFound(format!("blob {hash}"))
    } else {
        Error::io(path, e)
    }
}

fn corrupt_hash(hash: &BlobHash) -> Error {
    Error::Corrupt(format!("blob {hash}: content does not match its hash"))
}

/// Remove a file; returns its length, or 0 if it did not exist.
fn remove_file_len(path: &Path) -> Result<u64> {
    let len = match fs::symlink_metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::io(path, e)),
    };
    match fs::remove_file(path) {
        Ok(()) => Ok(len),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Make a rename durable (best effort; directories cannot be fsynced on Windows).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// A temp file that is removed on drop unless it was renamed into place.
struct TempFile {
    path: PathBuf,
    file: Option<File>,
    keep: bool,
}

impl TempFile {
    fn file(&mut self) -> Result<&mut File> {
        match self.file.as_mut() {
            Some(f) => Ok(f),
            None => Err(Error::io(
                &self.path,
                io::Error::other("temp file already closed"),
            )),
        }
    }

    /// Close the handle (Windows cannot rename or delete an open file).
    fn close(&mut self) {
        self.file = None;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        self.file = None;
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::open(tmp.path().join("objects")).unwrap();
        (tmp, store)
    }

    /// A temp file holding `bytes`, as `put` leaves it before `persist`.
    fn temp_with(store: &Store, bytes: &[u8]) -> (TempFile, PathBuf) {
        let mut temp = store.temp_file().unwrap();
        temp.file().unwrap().write_all(bytes).unwrap();
        let path = temp.path.clone();
        (temp, path)
    }

    fn denied() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    /// DSNA-92: a concurrent writer creates the object, then our rename fails (as on Windows
    /// when that object is open). The existing object is a success.
    #[test]
    fn rename_failure_with_existing_object_is_success() {
        let (_tmp, store) = store();
        let bytes = b"race";
        let hash = BlobHash::of(bytes);
        let winner = zstd::bulk::compress(bytes, ZSTD_LEVEL).unwrap();
        let (temp, temp_path) = temp_with(&store, b"ours, a different length");
        let calls = Cell::new(0);

        let size = store
            .persist_with(temp, &hash, |_, to| {
                calls.set(calls.get() + 1);
                fs::write(to, &winner)?;
                Err(denied())
            })
            .unwrap();

        assert_eq!(calls.get(), 1, "no retry once the object exists");
        assert_eq!(size, winner.len() as u64, "size of the existing object");
        assert_eq!(store.get(&hash).unwrap(), bytes);
        assert!(!temp_path.exists(), "temp file removed");
    }

    /// A transient sharing violation is retried.
    #[test]
    fn transient_rename_failure_is_retried() {
        let (_tmp, store) = store();
        let bytes = b"retry";
        let hash = BlobHash::of(bytes);
        let compressed = zstd::bulk::compress(bytes, ZSTD_LEVEL).unwrap();
        let (temp, temp_path) = temp_with(&store, &compressed);
        let calls = Cell::new(0);

        let size = store
            .persist_with(temp, &hash, |from, to| {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(denied())
                } else {
                    fs::rename(from, to)
                }
            })
            .unwrap();

        assert_eq!(calls.get(), 3);
        assert_eq!(size, compressed.len() as u64);
        assert_eq!(store.get(&hash).unwrap(), bytes);
        assert!(!temp_path.exists());
    }

    /// A rename that keeps failing with no object in place is an error, and the temp file is
    /// cleaned up.
    #[test]
    fn persistent_rename_failure_is_an_error() {
        let (_tmp, store) = store();
        let hash = BlobHash::of(b"never");
        let (temp, temp_path) = temp_with(&store, b"x");
        let calls = Cell::new(0);

        let err = store
            .persist_with(temp, &hash, |_, _| {
                calls.set(calls.get() + 1);
                Err(denied())
            })
            .unwrap_err();

        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        assert_eq!(calls.get(), 6, "first try plus 5 retries");
        assert!(!store.contains(&hash));
        assert!(!temp_path.exists());
    }

    /// Yields `data`, then fails with `kind`.
    struct FailAfter {
        data: io::Cursor<Vec<u8>>,
        kind: io::ErrorKind,
    }

    impl Read for FailAfter {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.data.read(buf)? {
                0 => Err(io::Error::new(self.kind, "device error")),
                n => Ok(n),
            }
        }
    }

    /// A reader over the first half of `bytes`' zstd frame, then `kind`.
    fn failing_reader(bytes: &[u8], kind: io::ErrorKind) -> BlobReader {
        let frame = zstd::bulk::compress(bytes, ZSTD_LEVEL).unwrap();
        let half = frame.get(..frame.len() / 2).unwrap().to_vec();
        let source = FailAfter {
            data: io::Cursor::new(half),
            kind,
        };
        BlobReader::new(
            Box::new(source),
            PathBuf::from("objects/ab/cd"),
            BlobHash::of(bytes),
        )
        .unwrap()
    }

    fn noisy(len: usize) -> Vec<u8> {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[0]
            })
            .collect()
    }

    /// DSNA-93: an I/O error under the decoder keeps its kind and is not reported as Corrupt.
    #[test]
    fn io_error_while_reading_is_not_corrupt() {
        let bytes = noisy(1 << 20);
        for kind in [
            io::ErrorKind::Other,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::UnexpectedEof,
        ] {
            let mut reader = failing_reader(&bytes, kind);
            let err = reader.read_to_end(&mut Vec::new()).unwrap_err();
            assert_eq!(err.kind(), kind);
            assert!(reader.io_failed());

            let mut reader = failing_reader(&bytes, kind);
            match copy_reader(&mut reader, &mut io::sink()) {
                Err(Error::Io { path, source }) => {
                    assert_eq!(path, PathBuf::from("objects/ab/cd"));
                    assert_eq!(source.kind(), kind);
                }
                other => panic!("expected Io, got {other:?}"),
            }
        }
    }

    /// Bad data from a healthy file is still Corrupt.
    #[test]
    fn truncated_frame_is_corrupt() {
        let bytes = noisy(1 << 20);
        let frame = zstd::bulk::compress(&bytes, ZSTD_LEVEL).unwrap();
        let half = frame.get(..frame.len() / 2).unwrap().to_vec();
        let mut reader = BlobReader::new(
            Box::new(io::Cursor::new(half)),
            PathBuf::from("x"),
            BlobHash::of(&bytes),
        )
        .unwrap();
        let err = copy_reader(&mut reader, &mut io::sink()).unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
        assert!(!reader.io_failed());
    }
}
