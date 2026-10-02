//! Taking snapshots. Owner: Chain K (DSNA-14).
//!
//! Algorithm (DSNA-1 "Snapshot algorithm"):
//!
//! 1. Walk the folder with the project's ignore rules and the global size cap.
//! 2. Reuse the latest version's hash for files whose size and mtime match it; hash and store
//!    every other file, in parallel.
//! 3. Compare the result with the latest version. Nothing changed: no version is written
//!    (F7). A project's first snapshot is always written, even for an empty folder, so it
//!    has a baseline.
//! 4. Insert the version, its entries and the new blob rows in one transaction
//!    ([`crate::db::Db::insert_version`]).
//!
//! Blobs are written before the transaction. A snapshot that fails or is cancelled before
//! the commit leaves no version, only orphan blobs, which `Db::sweep_orphans` removes.
//!
//! If a concurrent prune deleted a blob this snapshot deduplicated against, the insert fails
//! with [`Error::BlobMissing`]; the affected files are stored again (or recaptured if they
//! changed meanwhile) and the insert is retried once (DSNA-80 protocol, DSNA-83).

pub(crate) mod capture;

use time::OffsetDateTime;
use time::macros::format_description;

use crate::db::NewVersion;
use crate::diff::entries::{EntryDiffOptions, count_changes, diff_entries};
use crate::error::{Error, Result};
use crate::facade::Dsnap;
use crate::types::{ProjectId, SnapshotOptions, SnapshotReport};

use capture::{Hooks, Mode};

impl Dsnap {
    /// Capture the project folder. Returns `version: None` when nothing changed (F7).
    ///
    /// The label is trimmed and cut to [`MAX_LABEL_CHARS`] characters; an empty label uses
    /// the local time (`YYYY-MM-DD HH:MM:SS`).
    ///
    /// After a version commits, retention runs ([`Dsnap::after_snapshot`]; skipped for
    /// safety versions). A retention error does not fail the snapshot.
    ///
    /// Errors: [`crate::Error::ProjectMissing`] if the folder is gone,
    /// [`crate::Error::Cancelled`] if `opts.cancel` fires before the commit (nothing is
    /// committed then), [`Error::BlobMissing`] if a blob is still missing after one retry,
    /// [`Error::Busy`] if another D-Snap operation held the database past the busy timeout,
    /// and walk, store or database errors.
    pub fn snapshot(&self, project: ProjectId, opts: SnapshotOptions) -> Result<SnapshotReport> {
        let hooks = Hooks {
            progress: opts.progress.clone(),
            cancel: opts.cancel.clone(),
        };
        let proj = self.load_project(project)?;
        // Taken before the walk: `created_at_ms` is the capture start, which the next
        // capture's racy-clean check relies on (DSNA-103).
        let now = now_local();
        let mut cap = self.capture(&proj, Mode::Store, &hooks)?;

        let label = opts
            .label
            .as_deref()
            .map(clean_label)
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| default_label(now));
        let mut retried = false;
        loop {
            let changes = diff_entries(
                &cap.latest_entries,
                &cap.entries,
                EntryDiffOptions::default(),
            );
            let mut report = SnapshotReport {
                version: None,
                skipped: cap.skipped.clone(),
                unstable_paths: cap.unstable_paths.clone(),
            };
            if cap.latest.is_some() && changes.is_empty() {
                return Ok(report);
            }
            let nv = NewVersion {
                project_id: project,
                label: label.clone(),
                created_at_ms: unix_ms(now),
                kind: opts.kind,
                unstable: !report.unstable_paths.is_empty(),
                counts: count_changes(&changes),
                entries: std::mem::take(&mut cap.entries),
                new_blobs: std::mem::take(&mut cap.new_blobs),
            };
            hooks.check_cancel()?;
            match self.db.insert_version(&nv, &self.store) {
                Ok(v) => {
                    // Retention (DSNA-55) runs after the commit; the version is saved, so a
                    // retention error is only a warning and the next run retries (DSNA-109).
                    let _ = self.after_snapshot(&v);
                    report.version = Some(v);
                    return Ok(report);
                }
                // A prune removed a blob this snapshot deduplicated against (DSNA-80): store
                // the affected files again and retry once. Nothing was committed.
                Err(Error::BlobMissing(hash)) if !retried => {
                    retried = true;
                    cap.entries = nv.entries;
                    cap.new_blobs = nv.new_blobs;
                    self.restore_missing_blobs(&mut cap, hash)?;
                }
                Err(e) => return Err(busy_to_retryable(e)),
            }
        }
    }
}

/// Map SQLite's busy/locked errors to [`Error::Busy`] (DSNA-100): another D-Snap operation
/// held the write lock past the busy timeout, so the caller may simply retry.
fn busy_to_retryable(e: Error) -> Error {
    match &e {
        Error::Db(db)
            if matches!(
                db.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ) =>
        {
            Error::Busy
        }
        _ => e,
    }
}

/// Longest version label in characters; longer labels are cut (same limit as
/// `projects::MAX_NAME_CHARS` in Chain L).
pub const MAX_LABEL_CHARS: usize = 200;

/// Trim a label and cut it to [`MAX_LABEL_CHARS`] characters. A snapshot never fails over its
/// label (a hook's `-m` text may be anything), so an over-long label is truncated, not
/// rejected.
fn clean_label(label: &str) -> String {
    let trimmed = label.trim();
    match trimmed.char_indices().nth(MAX_LABEL_CHARS) {
        Some((cut, _)) => trimmed[..cut].trim_end().to_owned(),
        None => trimmed.to_owned(),
    }
}

/// Local time, or UTC when the local offset cannot be determined.
fn now_local() -> OffsetDateTime {
    OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc())
}

/// Default label: local time as `YYYY-MM-DD HH:MM:SS`.
fn default_label(t: OffsetDateTime) -> String {
    let fmt = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    t.format(&fmt)
        .unwrap_or_else(|_| t.unix_timestamp().to_string())
}

fn unix_ms(t: OffsetDateTime) -> i64 {
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn labels_are_trimmed_and_capped() {
        assert_eq!(
            clean_label(
                "  before agent turn 
"
            ),
            "before agent turn"
        );
        assert_eq!(clean_label("   "), "");
        let long = "é".repeat(MAX_LABEL_CHARS + 50);
        assert_eq!(clean_label(&long).chars().count(), MAX_LABEL_CHARS);
        let exact = "x".repeat(MAX_LABEL_CHARS);
        assert_eq!(clean_label(&exact), exact);
    }

    #[test]
    fn default_label_is_a_local_timestamp() {
        let t = datetime!(2026-03-04 05:06:07 +02:00);
        assert_eq!(default_label(t), "2026-03-04 05:06:07");
        assert_eq!(unix_ms(t), t.unix_timestamp() * 1000);
    }
}
