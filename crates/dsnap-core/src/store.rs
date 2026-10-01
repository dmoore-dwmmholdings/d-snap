//! Content-addressed blob store: `objects/ab/cdef...`, zstd-compressed. Owner: Chain C (DSNA-6).
#![allow(unused_variables)] // stub signatures; remove when implemented

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{BlobHash, BlobInfo};

/// Blob store rooted at the `objects` directory.
#[derive(Debug)]
pub struct Store {
    #[allow(dead_code)] // used once Chain C implements the store
    pub(crate) dir: PathBuf,
}

impl Store {
    /// Open the store at `dir`, creating it if missing.
    pub fn open(dir: PathBuf) -> Result<Self> {
        todo!("DSNA-6")
    }

    /// Store `bytes` (no-op if the hash already exists) and return its record.
    ///
    /// Writes go to a temp file renamed into place, so a reader never sees a partial blob.
    /// The no-op path is safe only because `Db::insert_version` re-checks blob presence under
    /// the write lock (see the `db` module docs).
    pub fn put(&self, bytes: &[u8]) -> Result<BlobInfo> {
        todo!("DSNA-6")
    }

    /// Stream a file into the store (no-op if the hash already exists) and return its record.
    pub fn put_file(&self, path: &Path) -> Result<BlobInfo> {
        todo!("DSNA-6")
    }

    /// Read and decompress a blob, verifying its hash ([`crate::Error::Corrupt`] on mismatch).
    pub fn get(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        todo!("DSNA-6")
    }

    /// Whether a blob exists.
    pub fn contains(&self, hash: &BlobHash) -> bool {
        todo!("DSNA-6")
    }

    /// Delete a blob; returns the bytes freed on disk (0 if it did not exist).
    ///
    /// Only [`crate::db::Db::prune_unreferenced`] may call this, inside its write transaction.
    /// Deleting a blob anywhere else can lose snapshot data (see the `db` module docs).
    pub fn delete(&self, hash: &BlobHash) -> Result<u64> {
        todo!("DSNA-6")
    }

    /// On-disk location of a blob.
    pub fn path_of(&self, hash: &BlobHash) -> PathBuf {
        todo!("DSNA-6")
    }
}

/// Hash a file's bytes without storing it; returns the hash and size.
pub fn hash_file(path: &Path) -> Result<(BlobHash, u64)> {
    todo!("DSNA-6")
}

impl Dsnap {
    /// Read a blob's bytes (e.g. for an image preview).
    pub fn read_blob(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        todo!("DSNA-6")
    }
}
