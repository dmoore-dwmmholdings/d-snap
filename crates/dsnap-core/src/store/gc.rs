//! Store-level garbage collection and integrity checks (DSNA-29).
//!
//! These primitives see only the files on disk. Which blobs are still needed is the
//! database's knowledge, so deleting through them must follow the blob lifetime protocol in
//! the [`crate::db`] module docs.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Store, TEMP_PREFIX, remove_file_len};
use crate::error::{Error, IoResultExt, Result};
use crate::types::BlobHash;

/// Temp files younger than this may belong to a write in progress (possibly in another
/// process) and are never removed.
pub const TEMP_GRACE: Duration = Duration::from_secs(60 * 60);

/// Outcome of [`Store::sweep`] or [`Store::clean_temp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// Blob objects deleted.
    pub deleted: u64,
    /// Stale temp files removed.
    pub temp_removed: u64,
    /// Bytes freed on disk (objects and temp files).
    pub bytes_freed: u64,
}

/// One file found under `objects/`.
enum Item {
    Object(BlobHash, PathBuf),
    Temp(PathBuf),
}

impl Store {
    /// Every blob hash stored, sorted. Files that are not objects are skipped.
    pub fn list_hashes(&self) -> Result<Vec<BlobHash>> {
        let mut out: Vec<BlobHash> = self
            .scan()?
            .into_iter()
            .filter_map(|i| match i {
                Item::Object(h, _) => Some(h),
                Item::Temp(_) => None,
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Delete every object whose hash is not in `referenced`, and temp files older than
    /// [`TEMP_GRACE`].
    ///
    /// **Blob lifetime protocol:** this deletes blob files, so call it only from the database
    /// layer while it holds the write lock (`BEGIN IMMEDIATE`), with `referenced` read inside
    /// that same transaction. A `referenced` set read earlier can miss a version committed in
    /// the meantime and lose its data (see the [`crate::db`] module docs).
    pub fn sweep(&self, referenced: &HashSet<BlobHash>) -> Result<SweepReport> {
        let now = SystemTime::now();
        let mut report = SweepReport::default();
        for item in self.scan()? {
            match item {
                Item::Object(hash, _) if referenced.contains(&hash) => {}
                Item::Object(_, path) => {
                    let freed = remove_file_len(&path)?;
                    // 0 means it vanished meanwhile (no zstd frame is empty).
                    if freed > 0 {
                        report.deleted += 1;
                        report.bytes_freed += freed;
                    }
                }
                Item::Temp(path) => remove_stale_temp(&path, now, &mut report),
            }
        }
        Ok(report)
    }

    /// Remove temp files older than [`TEMP_GRACE`] (left by a crashed write). Safe at any
    /// time.
    pub fn clean_temp(&self) -> Result<SweepReport> {
        let now = SystemTime::now();
        let mut report = SweepReport::default();
        for item in self.scan()? {
            if let Item::Temp(path) = item {
                remove_stale_temp(&path, now, &mut report);
            }
        }
        Ok(report)
    }

    /// Read and verify every object; returns the ones that fail, with the error.
    ///
    /// An object deleted while the scan runs is not reported.
    pub fn verify_all(&self) -> Result<Vec<(BlobHash, Error)>> {
        let mut bad = Vec::new();
        for hash in self.list_hashes()? {
            match self.copy_to(&hash, &mut io::sink()) {
                Ok(_) | Err(Error::NotFound(_)) => {}
                Err(e) => bad.push((hash, e)),
            }
        }
        Ok(bad)
    }

    /// List objects (`ab/<62 hex>`) and temp files (`.tmp-*`, at any level).
    fn scan(&self) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        for entry in read_dir(&self.dir)? {
            let (name, path, is_dir) = entry;
            if is_dir {
                if name.len() == 2 && is_lower_hex(&name) {
                    for (file, fpath, fdir) in read_dir(&path)? {
                        if fdir {
                            continue;
                        }
                        if file.starts_with(TEMP_PREFIX) {
                            items.push(Item::Temp(fpath));
                        } else if file.len() == 62 && is_lower_hex(&file) {
                            if let Ok(hash) = format!("{name}{file}").parse() {
                                items.push(Item::Object(hash, fpath));
                            }
                        }
                    }
                }
            } else if name.starts_with(TEMP_PREFIX) {
                items.push(Item::Temp(path));
            }
        }
        Ok(items)
    }
}

/// Entries of `dir` as (UTF-8 name, path, is_dir); non-UTF-8 names are skipped. A directory
/// that vanished meanwhile reads as empty.
fn read_dir(dir: &Path) -> Result<Vec<(String, PathBuf, bool)>> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(dir, e)),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.at(dir)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let is_dir = match entry.file_type() {
            Ok(t) => t.is_dir(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io(entry.path(), e)),
        };
        out.push((name, entry.path(), is_dir));
    }
    Ok(out)
}

/// Remove a temp file if it is older than [`TEMP_GRACE`]. Errors are ignored: the file may
/// be open by its writer, and the next sweep retries.
fn remove_stale_temp(path: &Path, now: SystemTime, report: &mut SweepReport) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    let age = meta
        .modified()
        .ok()
        .and_then(|m| now.duration_since(m).ok())
        .unwrap_or(Duration::ZERO);
    if age < TEMP_GRACE {
        return;
    }
    if fs::remove_file(path).is_ok() {
        report.temp_removed += 1;
        report.bytes_freed += meta.len();
    }
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
