//! Rewriting damaged objects in place (DSNA-94).
//!
//! Dedupe in [`Store::put`] trusts any non-empty object, and the snapshot fast path does not
//! call `put` for unchanged files at all, so a damaged object stays damaged until something
//! rewrites it. [`Store::repair`] and [`Store::repair_file`] do that for a hash that
//! [`Store::verify_all`] reported.
//!
//! **Blob lifetime protocol:** a repair never removes an object. It renames a verified copy
//! of the right content over the damaged file in one atomic step, so no reader or `Db`
//! re-check ever sees the object missing, and it needs no DB lock. (Moving a bad object
//! aside instead would be a delete and would need the lock.)

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use super::{SMALL_FILE_LIMIT, Store, TempFile, sync_dir};
use crate::error::{Error, IoResultExt, Result};
use crate::types::BlobHash;

/// Outcome of [`Store::repair`] or [`Store::repair_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairOutcome {
    /// The object was already intact; nothing was written and the source was not read.
    Healthy,
    /// The object was missing or damaged and now holds the source's bytes.
    Repaired {
        /// Compressed size of the new object.
        stored_size: u64,
    },
    /// The object needs repair, but the source's content has a different hash (the file
    /// changed since it was stored). Nothing was written.
    SourceMismatch {
        /// Hash of the bytes the source held.
        actual: BlobHash,
    },
}

impl Store {
    /// Rewrite the object for `hash` from `bytes` if it is missing or damaged.
    ///
    /// Use it for a hash [`Store::verify_all`] reported as [`Error::Corrupt`]. The existing
    /// object is read and verified first; an intact one is left alone. `bytes` are stored only
    /// if they hash to `hash`. An object that cannot be read is an [`Error::Io`] and is left
    /// alone (it may be fine). Safe at any time; see the module docs.
    pub fn repair(&self, hash: &BlobHash, bytes: &[u8]) -> Result<RepairOutcome> {
        if self.is_intact(hash)? {
            return Ok(RepairOutcome::Healthy);
        }
        self.repair_checked(hash, bytes)
    }

    /// The write half of [`Store::repair`], after the object was found not intact.
    fn repair_checked(&self, hash: &BlobHash, bytes: &[u8]) -> Result<RepairOutcome> {
        let actual = BlobHash::of(bytes);
        if actual != *hash {
            return Ok(RepairOutcome::SourceMismatch { actual });
        }
        let (temp, _) = self.stage_bytes(bytes, hash)?;
        let stored_size = self.replace(temp, hash)?;
        Ok(RepairOutcome::Repaired { stored_size })
    }

    /// [`Store::repair`] from a file, e.g. the working-tree file of an entry that references
    /// `hash`. The file is read once, streamed, and stored only if what was read hashes to
    /// `hash`. Symlinks are followed, as in [`Store::put_file`].
    pub fn repair_file(&self, hash: &BlobHash, path: &Path) -> Result<RepairOutcome> {
        if self.is_intact(hash)? {
            return Ok(RepairOutcome::Healthy);
        }
        let mut src = File::open(path).at(path)?;
        // Same encoding as `put_file`, so the object (and its stored size) matches.
        if src.metadata().at(path)?.len() <= SMALL_FILE_LIMIT {
            let mut bytes = Vec::new();
            src.read_to_end(&mut bytes).at(path)?;
            return self.repair_checked(hash, &bytes);
        }
        let (temp, actual, _size) = self.stage_reader(&mut src, path)?;
        drop(src);
        if actual != *hash {
            return Ok(RepairOutcome::SourceMismatch { actual });
        }
        let stored_size = self.replace(temp, hash)?;
        Ok(RepairOutcome::Repaired { stored_size })
    }

    /// Whether the object for `hash` exists and verifies. Missing, empty or bad data is
    /// `false`; a failure to read the object file is an error.
    fn is_intact(&self, hash: &BlobHash) -> Result<bool> {
        if self.stored_size(hash)?.is_none() {
            return Ok(false);
        }
        match self.copy_to(hash, &mut io::sink()) {
            Ok(_) => Ok(true),
            Err(Error::NotFound(_) | Error::Corrupt(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Fsync `temp` and rename it over `hash`'s object path, replacing whatever is there.
    fn replace(&self, temp: TempFile, hash: &BlobHash) -> Result<u64> {
        self.replace_with(temp, hash, |from, to| fs::rename(from, to))
    }

    /// [`Store::replace`] with an injectable rename, so tests can reach the retry branch.
    fn replace_with(
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

        let dest = self.path_of(hash);
        let shard = self.ensure_shard(hash)?;

        let mut attempt = 0u64;
        loop {
            match rename(&temp_path, &dest) {
                Ok(()) => {
                    temp.keep = true;
                    sync_dir(&shard);
                    return Ok(len);
                }
                // Windows: the object may be open without delete sharing (a reader, a virus
                // scanner). Retry briefly.
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempt < 5 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10 * attempt));
                }
                Err(e) => return Err(Error::io(dest, e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ZSTD_LEVEL;
    use std::cell::Cell;
    use std::io::Write;
    use tempfile::TempDir;

    /// A damaged object stays in place when the rename keeps failing; transient failures are
    /// retried. The temp file is removed either way.
    #[test]
    fn replace_retries_then_gives_up_without_touching_the_object() {
        let tmp = TempDir::new().unwrap();
        let store = Store::open(tmp.path().join("objects")).unwrap();
        let bytes = b"repair me";
        let hash = BlobHash::of(bytes);
        store.put(bytes).unwrap();
        let path = store.path_of(&hash);
        fs::write(&path, b"damaged").unwrap();

        let stage = || {
            let mut temp = store.temp_file().unwrap();
            let frame = zstd::bulk::compress(bytes, ZSTD_LEVEL).unwrap();
            temp.file().unwrap().write_all(&frame).unwrap();
            let p = temp.path.clone();
            (temp, p)
        };

        let (temp, temp_path) = stage();
        let calls = Cell::new(0);
        let err = store
            .replace_with(temp, &hash, |_, _| {
                calls.set(calls.get() + 1);
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            })
            .unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        assert_eq!(calls.get(), 6, "first try plus 5 retries");
        assert_eq!(fs::read(&path).unwrap(), b"damaged");
        assert!(!temp_path.exists());

        let (temp, temp_path) = stage();
        let calls = Cell::new(0);
        store
            .replace_with(temp, &hash, |from, to| {
                calls.set(calls.get() + 1);
                if calls.get() < 3 {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                } else {
                    fs::rename(from, to)
                }
            })
            .unwrap();
        assert_eq!(calls.get(), 3);
        assert_eq!(store.get(&hash).unwrap(), bytes);
        assert!(!temp_path.exists());
    }
}
