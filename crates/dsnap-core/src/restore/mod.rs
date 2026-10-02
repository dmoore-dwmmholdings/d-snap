//! Restoring files and projects. Owner: Chain M (DSNA-16).
//!
//! Every restore runs the same steps:
//!
//! 1. **Safety snapshot** (Rule 1, F21): a [`VersionKind::Safety`] version of the folder,
//!    written even when nothing changed, so the undo point is a version of its own and the
//!    newest one, which retention keeps (DSNA-85). If it fails, nothing is written
//!    ([`Error::SafetySnapshotFailed`]).
//! 2. **Plan** against what that snapshot captured (`plan` module). Paths whose current content
//!    the snapshot could not hold are left alone and reported as `uncaptured` (DSNA-87).
//! 3. **Stage** every file to write: its blob is copied to an fsynced temp file in the
//!    project root before anything visible changes. If the target version or a blob has
//!    gone meanwhile (another process's retention), the restore fails here with the folder
//!    untouched (DSNA-85).
//! 4. **Apply**: deletes (deepest first), empty-directory removal (non-recursive only),
//!    directory creation, then each staged file is renamed into place. Before a path is
//!    deleted or replaced, its stat is checked against the safety capture; a path that
//!    changed since is left alone and reported as `uncaptured`. The first locked file stops
//!    the restore: every path not yet done goes to `failed`, and the safety version undoes
//!    what was done.
//! 5. **Verify**: the folder is captured again and each written or deleted path compared
//!    with the target; a mismatch moves to `failed`.
//! 6. **Retention** for the project, protecting the target and the safety version.
//!
//! Directories are never removed recursively: [`EntryKind::Dir`] entries may still hold
//! ignored or skipped paths.

mod plan;
mod write;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

use crate::diff::entries::EntryDiffOptions;
use crate::error::{Error, IoResultExt, Result};
use crate::facade::Dsnap;
use crate::ignore_rules::IgnoreRules;
use crate::snapshot::capture::{Hooks, Mode};
use crate::types::{
    Entry, EntryKind, Hunk, Project, ProjectId, RelPath, RestorePlan, RestoreReport,
    SnapshotOptions, SnapshotReport, Version, VersionId, VersionKind,
};
use crate::walk::mtime_ns;

use plan::{Current, Plan, plan};
use write::{Staged, create_dir, locked_or_io, stage, write_symlink};

impl Dsnap {
    /// What restoring the whole project to `target` would write, delete and create, and
    /// which paths it would leave alone (`uncaptured`). Writes nothing.
    ///
    /// The folder is captured as for [`Dsnap::status`]; a file that cannot be read now is
    /// uncaptured. The restore itself decides again from its safety snapshot.
    ///
    /// Errors: [`Error::NotFound`] if `target` is not a version of `project`,
    /// [`Error::ProjectMissing`], and walk or database errors (including an unreadable
    /// `.gitignore`).
    pub fn restore_plan(&self, project: ProjectId, target: VersionId) -> Result<RestorePlan> {
        let proj = self.load_project(project)?;
        self.version_of(project, target)?;
        let cap = self.capture(&proj, Mode::HashOnly, &Hooks::default())?;
        let current = Current::new(cap.entries, &cap.skipped, &[], case_insensitive());
        let entries = self.db.entries(target)?;
        let p = plan(&cap.root, &entries, &current, &cap.rules, None)?;
        Ok(RestorePlan {
            target,
            write: p.write.into_iter().map(|e| e.path).collect(),
            delete: p.delete.into_iter().map(|e| e.path).collect(),
            create_dirs: p.create_dirs,
            uncaptured: p.uncaptured,
        })
    }

    /// Restore the whole project to `target` (F20). See the module docs for the steps.
    ///
    /// Never touches ignored or uncaptured paths. Files added since `target` are deleted.
    /// On a locked file the restore stops; `failed` lists every path not done, and
    /// restoring `safety_version` undoes what was done.
    ///
    /// Errors (nothing written): [`Error::NotFound`], [`Error::ProjectMissing`],
    /// [`Error::SafetySnapshotFailed`], and errors while planning or staging (a missing
    /// blob, an unreadable `.gitignore`).
    pub fn restore_project(&self, project: ProjectId, target: VersionId) -> Result<RestoreReport> {
        self.restore(project, target, None)
    }

    /// Restore one file (including a deleted one) to its content in `version` (F19).
    ///
    /// Takes a safety snapshot first, like [`Dsnap::restore_project`]. An uncaptured path
    /// (ignored now, over the size cap, locked...) is not written; it is returned in
    /// `uncaptured`. Captured paths in the way (a file where the version has a directory
    /// above `path`, or files inside a directory at `path`) are removed.
    ///
    /// Errors: [`Error::NotFound`] if `path` is not in `version` (or `version` is not a
    /// version of `project`), and the errors of [`Dsnap::restore_project`].
    pub fn restore_file(
        &self,
        project: ProjectId,
        version: VersionId,
        path: &RelPath,
    ) -> Result<RestoreReport> {
        self.restore(project, version, Some(path))
    }

    /// Undo one hunk of the diff from `base` to the working file at `path`.
    pub fn revert_hunk(
        &self,
        project: ProjectId,
        base: VersionId,
        path: &RelPath,
        hunk: &Hunk,
    ) -> Result<RestoreReport> {
        let _ = (project, base, path, hunk);
        todo!("DSNA-59")
    }

    /// `id`, if it is a version of `project`; [`Error::NotFound`] otherwise.
    fn version_of(&self, project: ProjectId, id: VersionId) -> Result<Version> {
        match self.db.get_version(id) {
            Ok(v) if v.project_id == project => Ok(v),
            Ok(_) | Err(Error::NotFound(_)) => Err(Error::NotFound(format!(
                "version {} of project {}",
                id.0, project.0
            ))),
            Err(e) => Err(e),
        }
    }

    fn restore(
        &self,
        project: ProjectId,
        target: VersionId,
        scope: Option<&RelPath>,
    ) -> Result<RestoreReport> {
        let proj = self.load_project(project)?;
        let version = self.version_of(project, target)?;
        if let Some(path) = scope {
            if self.db.entry(target, path)?.is_none() {
                return Err(Error::NotFound(format!(
                    "{path} in version \"{}\"",
                    version.label
                )));
            }
        }

        // 1. Safety snapshot.
        let safety = self
            .snapshot_forced(
                project,
                SnapshotOptions {
                    label: Some(format!("Before restore to {}", version.label)),
                    kind: VersionKind::Safety,
                    progress: None,
                    cancel: None,
                },
            )
            .map_err(|e| Error::SafetySnapshotFailed {
                source: Box::new(e),
            })?;
        let safety_version =
            safety
                .version
                .as_ref()
                .map(|v| v.id)
                .ok_or_else(|| Error::SafetySnapshotFailed {
                    source: Box::new(Error::Corrupt("forced snapshot wrote no version".into())),
                })?;

        let report = self.restore_from(&proj, target, scope, safety_version, &safety);
        // 6. Retention, protecting the target and the undo point. The restore is done, so
        // an error here is only a warning (DSNA-55).
        let _ = self.apply_retention_protecting(project, &[target, safety_version]);
        report
    }

    /// Steps 2 to 5, after the safety snapshot.
    fn restore_from(
        &self,
        proj: &Project,
        target: VersionId,
        scope: Option<&RelPath>,
        safety_version: VersionId,
        safety: &SnapshotReport,
    ) -> Result<RestoreReport> {
        let root = &proj.root;
        // 2. Plan against what the safety snapshot captured. Every path is decided before
        // anything is written: the restore may rewrite a `.gitignore`.
        let rules = IgnoreRules::new(root, &proj.settings)?;
        let current = Current::new(
            self.db.entries(safety_version)?,
            &safety.skipped,
            &safety.unstable_paths,
            case_insensitive(),
        );
        // Another process may have deleted the target since the first check.
        self.version_of(proj.id, target)?;
        let entries = self.db.entries(target)?;
        let plan = plan(root, &entries, &current, &rules, scope)?;

        // 3. Stage every file before the first visible change.
        let mut staged = Vec::with_capacity(plan.write.len());
        for e in &plan.write {
            staged.push(match e.kind {
                EntryKind::File => Some(stage(root, e, &self.store)?),
                _ => None,
            });
        }

        // 4. Apply.
        let mut report = apply(root, &plan, &current, staged);
        report.safety_version = safety_version;

        // 5. Verify.
        if !report.written.is_empty() || !report.deleted.is_empty() {
            self.verify(proj, &plan, &mut report)?;
        }
        Ok(report)
    }

    /// Capture the folder again; move each written or deleted path that does not match the
    /// plan from `written`/`deleted` to `failed`.
    fn verify(&self, proj: &Project, plan: &Plan, report: &mut RestoreReport) -> Result<()> {
        let cap = self.capture(proj, Mode::HashOnly, &Hooks::default())?;
        let now: HashMap<&RelPath, &Entry> = cap.entries.iter().map(|e| (&e.path, e)).collect();
        let want: HashMap<&RelPath, &Entry> = plan.write.iter().map(|e| (&e.path, e)).collect();
        let mut failed = Vec::new();
        report.written.retain(|p| {
            let ok = match (now.get(p), want.get(p)) {
                (Some(n), Some(w)) => n.kind == w.kind && n.blob == w.blob,
                // A created directory: it may hold entries now, so only check it exists.
                (_, None) => p.to_path(&proj.root).is_dir(),
                (None, Some(_)) => false,
            };
            if !ok {
                failed.push((
                    p.clone(),
                    "does not match the version after the restore".into(),
                ));
            }
            ok
        });
        report.deleted.retain(|p| {
            let gone = !now.contains_key(p) || want.contains_key(p);
            if !gone {
                failed.push((p.clone(), "still present after the restore".into()));
            }
            gone
        });
        report.failed.extend(failed);
        report.failed.sort();
        Ok(())
    }
}

/// Whether paths that differ only in case name the same file (as `diff_entries` assumes).
fn case_insensitive() -> bool {
    EntryDiffOptions::default().case_insensitive
}

/// Step 4: apply `plan`. Per-path errors go to `failed`; nothing fails the whole call.
fn apply(
    root: &Path,
    plan: &Plan,
    current: &Current,
    staged: Vec<Option<Staged>>,
) -> RestoreReport {
    let mut r = RestoreReport {
        safety_version: VersionId(0),
        written: Vec::new(),
        deleted: Vec::new(),
        failed: Vec::new(),
        uncaptured: plan.uncaptured.clone(),
    };
    let mut stopped: Option<RelPath> = None;
    let stop_msg = |at: &RelPath| format!("not restored: stopped at locked file {at}");
    // Record an outcome; a locked path stops the rest.
    let outcome = |r: &mut RestoreReport, p: &RelPath, res: Result<()>, done: bool| match res {
        Ok(()) => {
            if done {
                r.deleted.push(p.clone());
            } else {
                r.written.push(p.clone());
            }
            None
        }
        Err(err @ Error::Locked { .. }) => {
            r.failed.push((p.clone(), err.to_string()));
            Some(p.clone())
        }
        Err(err) => {
            r.failed.push((p.clone(), err.to_string()));
            None
        }
    };

    // Deletes, deepest first.
    let mut deletes: Vec<&Entry> = plan.delete.iter().collect();
    deletes.sort_by(|a, b| b.path.as_str().cmp(a.path.as_str()));
    for e in deletes {
        if let Some(at) = &stopped {
            r.failed.push((e.path.clone(), stop_msg(at)));
            continue;
        }
        let abs = e.path.to_path(root);
        match unchanged(&abs, Some(e)) {
            Ok(true) => {}
            Ok(false) => {
                r.uncaptured.push(e.path.clone());
                continue;
            }
            Err(err) => {
                r.failed.push((e.path.clone(), err.to_string()));
                continue;
            }
        }
        stopped = outcome(&mut r, &e.path, remove_entry(&abs, e), true);
    }

    // Directories left empty: non-recursive only; a failure means it still holds something.
    if stopped.is_none() {
        for d in &plan.remove_dirs {
            let was_entry = current.get(d).is_some_and(|c| c.kind == EntryKind::Dir);
            if fs::remove_dir(d.to_path(root)).is_ok() && was_entry {
                r.deleted.push(d.clone());
            }
        }
    }

    for d in &plan.create_dirs {
        if let Some(at) = &stopped {
            r.failed.push((d.clone(), stop_msg(at)));
            continue;
        }
        stopped = outcome(&mut r, d, create_dir(&d.to_path(root)), false);
    }

    for (e, staged) in plan.write.iter().zip(staged) {
        if let Some(at) = &stopped {
            r.failed.push((e.path.clone(), stop_msg(at)));
            continue;
        }
        let abs = e.path.to_path(root);
        // Replace only what the safety snapshot holds.
        match unchanged(&abs, current.get(&e.path)) {
            Ok(true) => {}
            Ok(false) => {
                r.uncaptured.push(e.path.clone());
                continue;
            }
            Err(err) => {
                r.failed.push((e.path.clone(), err.to_string()));
                continue;
            }
        }
        let res = abs
            .parent()
            .map_or(Ok(()), |dir| fs::create_dir_all(dir).at(dir))
            .and_then(|()| match (&e.kind, staged) {
                (EntryKind::File, Some(s)) => s.place(&abs, e),
                (EntryKind::Symlink { target }, _) => write_symlink(&abs, target),
                _ => Err(Error::Corrupt(format!(
                    "cannot write {} as planned",
                    e.path
                ))),
            });
        stopped = outcome(&mut r, &e.path, res, false);
    }

    r.written.sort();
    r.deleted.sort();
    r.failed.sort();
    r.uncaptured.sort();
    r.uncaptured.dedup();
    r
}

/// Whether the path at `abs` is still what the safety capture recorded (`expected`), or
/// absent. Anything else changed after the safety snapshot, so its content may be in no
/// version.
fn unchanged(abs: &Path, expected: Option<&Entry>) -> Result<bool> {
    let meta = match fs::symlink_metadata(abs) {
        Ok(m) => m,
        // Gone since: nothing to lose.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(Error::io(abs, e)),
    };
    let Some(exp) = expected else {
        // A directory the plan emptied but could not remove still holds something; placing
        // a file there refuses. Anything else is new since the safety snapshot.
        return Ok(meta.is_dir());
    };
    Ok(match &exp.kind {
        EntryKind::File => {
            meta.is_file() && meta.len() == exp.size && mtime_ns(&meta) == exp.mtime_ns
        }
        EntryKind::Symlink { target } => {
            meta.file_type().is_symlink()
                && fs::read_link(abs).is_ok_and(|l| l.as_os_str() == OsStr::new(target))
        }
        EntryKind::Dir => meta.is_dir(),
    })
}

/// Remove a file or link without following it.
fn remove_entry(abs: &Path, e: &Entry) -> Result<()> {
    match fs::remove_file(abs) {
        Ok(()) => Ok(()),
        // A directory symlink or junction on Windows is removed like a directory.
        Err(err) if e.kind != EntryKind::File && cfg!(windows) => {
            fs::remove_dir(abs).map_err(|_| Error::io(abs, err))
        }
        Err(err) => Err(locked_or_io(abs, err)),
    }
}
