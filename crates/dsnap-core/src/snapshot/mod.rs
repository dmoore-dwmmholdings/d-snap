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

pub(crate) mod capture;

use time::OffsetDateTime;
use time::macros::format_description;

use crate::db::NewVersion;
use crate::diff::entries::{EntryDiffOptions, count_changes, diff_entries};
use crate::error::Result;
use crate::facade::Dsnap;
use crate::types::{ProjectId, SnapshotOptions, SnapshotReport};

use capture::{Hooks, Mode};

impl Dsnap {
    /// Capture the project folder. Returns `version: None` when nothing changed (F7).
    ///
    /// Errors: [`crate::Error::ProjectMissing`] if the folder is gone,
    /// [`crate::Error::Cancelled`] if `opts.cancel` fires before the commit (nothing is
    /// committed then), and walk, store or database errors.
    pub fn snapshot(&self, project: ProjectId, opts: SnapshotOptions) -> Result<SnapshotReport> {
        let hooks = Hooks {
            progress: opts.progress.clone(),
            cancel: opts.cancel.clone(),
        };
        let proj = self.load_project(project)?;
        let cap = self.capture(&proj, Mode::Store, &hooks)?;

        let changes = diff_entries(
            &cap.latest_entries,
            &cap.entries,
            EntryDiffOptions::default(),
        );
        let mut report = SnapshotReport {
            version: None,
            skipped: cap.skipped,
            unstable_paths: cap.unstable_paths,
        };
        if cap.latest.is_some() && changes.is_empty() {
            return Ok(report);
        }

        let now = now_local();
        let label = match opts.label {
            Some(l) if !l.trim().is_empty() => l,
            _ => default_label(now),
        };
        let nv = NewVersion {
            project_id: project,
            label,
            created_at_ms: unix_ms(now),
            kind: opts.kind,
            unstable: !report.unstable_paths.is_empty(),
            counts: count_changes(&changes),
            entries: cap.entries,
            new_blobs: cap.new_blobs,
        };
        hooks.check_cancel()?;
        report.version = Some(self.db.insert_version(&nv, &self.store)?);
        Ok(report)
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
    fn default_label_is_a_local_timestamp() {
        let t = datetime!(2026-03-04 05:06:07 +02:00);
        assert_eq!(default_label(t), "2026-03-04 05:06:07");
        assert_eq!(unix_ms(t), t.unix_timestamp() * 1000);
    }
}
