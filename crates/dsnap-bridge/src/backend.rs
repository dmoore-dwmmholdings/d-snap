//! What the commands do, over the core facade (DSNA-66). Plain methods, so tests call them
//! with a temporary home and capture events; `commands.rs` wraps each in a Tauri command.
//!
//! Writes on one project (snapshot, restore, hunk revert) run one at a time: later calls
//! wait, reporting phase `queued`. Each write emits its first `dsnap://progress` event
//! before its command returns its promise ([`Backend::begin`]), so the UI can tie the
//! latest progress for a project to the call.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dsnap_core::{
    BlobHash, CancelToken, DiffOptions, Dsnap, FileChange, FileDiff, GlobalSettings, Progress,
    ProgressEvent, Project, ProjectId, ProjectSettings, RelPath, RestorePlan, RestoreReport,
    RevertHunk, SnapshotOptions, SnapshotReport, Stage, Version, VersionId, VersionKind,
    VersionRef,
};
use serde::Serialize;

use crate::error::ApiError;

/// Result of a command.
pub type ApiResult<T> = Result<T, ApiError>;

/// Sends an event to the frontend: `(name, payload)`.
pub type Emit = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// Event names (`EVENTS` in `commands.ts`).
pub const EVENT_PROGRESS: &str = "dsnap://progress";
/// Versions of a project changed.
pub const EVENT_VERSIONS_CHANGED: &str = "dsnap://versions-changed";
/// Files in a project folder changed.
pub const EVENT_PROJECT_CHANGED: &str = "dsnap://project-changed";

/// Largest blob `read_blob_as_data_url` returns.
pub const MAX_DATA_URL_BYTES: usize = 20 * 1024 * 1024;

/// Kind of long-running operation (`ProgressOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    /// A snapshot.
    Snapshot,
    /// A restore or hunk revert.
    Restore,
}

/// Phase of an operation (`ProgressPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Waiting for another write on the same project.
    Queued,
    /// Walking the folder.
    Walk,
    /// Hashing and storing.
    Hash,
    /// Writing files.
    Restore,
    /// Finished (successfully or not).
    Done,
}

/// `Progress` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressPayload {
    /// Operation id for `cancel_operation`.
    pub op_id: String,
    /// The project.
    pub project_id: ProjectId,
    /// Snapshot or restore.
    pub op: Op,
    /// Phase.
    pub phase: Phase,
    /// Items done in this phase.
    pub done: u64,
    /// Total items, when known.
    pub total: Option<u64>,
    /// Item being processed.
    pub path: Option<RelPath>,
}

/// `{ projectId }` payload of `dsnap://versions-changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionsChanged {
    /// The project.
    pub project_id: ProjectId,
}

/// `{ projectId, changedCount }` payload of `dsnap://project-changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChanged {
    /// The project.
    pub project_id: ProjectId,
    /// Files changed since the latest version.
    pub changed_count: u32,
}

/// A write operation that has been announced ([`Backend::begin`]).
#[derive(Debug, Clone)]
pub struct OpCtx {
    /// Operation id.
    pub op_id: String,
    /// The project.
    pub project: ProjectId,
    /// Kind.
    pub op: Op,
    /// Cancels it while queued (and a snapshot while running).
    pub cancel: CancelToken,
}

/// The app's backend state.
pub struct Backend {
    dsnap: Result<Arc<Dsnap>, String>,
    emit: Emit,
    ops: Mutex<HashMap<String, CancelToken>>,
    project_locks: Mutex<HashMap<ProjectId, Arc<Mutex<()>>>>,
    next_op: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Backend {
    /// Open the data directory (`home`, or the default). An open error is kept and returned
    /// by every command, so the UI can show it.
    pub fn open(home: Option<std::path::PathBuf>, emit: Emit) -> Self {
        Self {
            dsnap: Dsnap::open(home)
                .map(Arc::new)
                .map_err(|e| format!("D-Snap could not open its data folder: {e}")),
            emit,
            ops: Mutex::default(),
            project_locks: Mutex::default(),
            next_op: AtomicU64::new(1),
        }
    }

    /// The core handle.
    pub fn dsnap(&self) -> ApiResult<&Arc<Dsnap>> {
        self.dsnap
            .as_ref()
            .map_err(|e| ApiError::new("db", e.clone()))
    }

    fn emit<T: Serialize>(&self, name: &str, payload: &T) {
        if let Ok(v) = serde_json::to_value(payload) {
            (self.emit)(name, v);
        }
    }

    fn versions_changed(&self, project_id: ProjectId) {
        self.emit(EVENT_VERSIONS_CHANGED, &VersionsChanged { project_id });
    }

    /// Report a project's change count (`dsnap://project-changed`).
    pub fn project_changed(&self, project_id: ProjectId, changed_count: u32) {
        self.emit(
            EVENT_PROJECT_CHANGED,
            &ProjectChanged {
                project_id,
                changed_count,
            },
        );
    }

    fn progress(
        &self,
        ctx: &OpCtx,
        phase: Phase,
        done: u64,
        total: Option<u64>,
        path: Option<RelPath>,
    ) {
        self.emit(
            EVENT_PROGRESS,
            &ProgressPayload {
                op_id: ctx.op_id.clone(),
                project_id: ctx.project,
                op: ctx.op,
                phase,
                done,
                total,
                path,
            },
        );
    }

    fn project_lock(&self, project: ProjectId) -> Arc<Mutex<()>> {
        Arc::clone(lock(&self.project_locks).entry(project).or_default())
    }

    /// Announce a write on `project`: register its cancel token and emit its first progress
    /// event (`queued` if another write on the project is running, else `walk`). Call before
    /// the command's promise is returned.
    pub fn begin(&self, project: ProjectId, op: Op) -> OpCtx {
        let n = self.next_op.fetch_add(1, Ordering::Relaxed);
        let ctx = OpCtx {
            op_id: format!("op-{n}"),
            project,
            op,
            cancel: CancelToken::new(),
        };
        lock(&self.ops).insert(ctx.op_id.clone(), ctx.cancel.clone());
        let busy = self.project_lock(project).try_lock().is_err();
        let phase = if busy { Phase::Queued } else { Phase::Walk };
        self.progress(&ctx, phase, 0, None, None);
        ctx
    }

    /// Run a write announced with [`Backend::begin`]: wait for the project, check for
    /// cancellation, run `f`, then emit `done` and forget the operation.
    fn run_write<T>(&self, ctx: &OpCtx, f: impl FnOnce(&Dsnap) -> ApiResult<T>) -> ApiResult<T> {
        let res = (|| {
            let dsnap = self.dsnap()?;
            let project_lock = self.project_lock(ctx.project);
            let _guard = lock(&project_lock);
            ctx.cancel.check()?;
            f(dsnap)
        })();
        self.progress(ctx, Phase::Done, 0, None, None);
        lock(&self.ops).remove(&ctx.op_id);
        res
    }

    /// Cancel a queued or running operation.
    pub fn cancel_operation(&self, op_id: &str) -> ApiResult<()> {
        match lock(&self.ops).get(op_id) {
            Some(t) => {
                t.cancel();
                Ok(())
            }
            None => Err(ApiError::new(
                "not_found",
                format!("no running operation {op_id}"),
            )),
        }
    }

    // ---- projects ----

    /// Projects sorted by name.
    pub fn list_projects(&self) -> ApiResult<Vec<Project>> {
        let mut list = self.dsnap()?.list_projects()?;
        list.sort_by_key(|p| (p.name.to_lowercase(), p.id));
        Ok(list)
    }

    /// Track a folder.
    pub fn add_project(&self, path: &str) -> ApiResult<Project> {
        Ok(self.dsnap()?.add_project(Path::new(path), None)?)
    }

    /// Rename a project.
    pub fn rename_project(&self, id: ProjectId, name: &str) -> ApiResult<Project> {
        let d = self.dsnap()?;
        d.rename_project(id, name)?;
        Ok(d.project(id)?)
    }

    /// Stop tracking a project.
    pub fn remove_project(&self, id: ProjectId, delete_snapshots: bool) -> ApiResult<()> {
        self.dsnap()?.remove_project(id, delete_snapshots)?;
        lock(&self.project_locks).remove(&id);
        Ok(())
    }

    /// Point a project at its moved folder.
    pub fn relocate_project(&self, id: ProjectId, path: &str) -> ApiResult<Project> {
        Ok(self.dsnap()?.relocate_project(id, Path::new(path))?)
    }

    // ---- settings ----

    /// A project's settings.
    pub fn get_project_settings(&self, id: ProjectId) -> ApiResult<ProjectSettings> {
        Ok(self.dsnap()?.project_settings(id)?)
    }

    /// Replace a project's settings.
    pub fn set_project_settings(&self, id: ProjectId, settings: &ProjectSettings) -> ApiResult<()> {
        Ok(self.dsnap()?.set_project_settings(id, settings)?)
    }

    /// App-wide settings.
    pub fn get_global_settings(&self) -> ApiResult<GlobalSettings> {
        Ok(self.dsnap()?.global_settings()?)
    }

    /// Replace the app-wide settings.
    pub fn set_global_settings(&self, settings: &GlobalSettings) -> ApiResult<()> {
        Ok(self.dsnap()?.set_global_settings(settings)?)
    }

    // ---- snapshots ----

    /// Take a manual snapshot (announced with [`Backend::begin`]).
    pub fn snapshot(&self, ctx: &OpCtx, label: Option<String>) -> ApiResult<SnapshotReport> {
        let report = self.run_write(ctx, |d| {
            let progress = {
                let ctx = ctx.clone();
                let emit = Arc::clone(&self.emit);
                Progress::new(move |e: &ProgressEvent| {
                    let phase = match e.stage {
                        Stage::Walk => Phase::Walk,
                        Stage::Hash => Phase::Hash,
                        Stage::Restore => Phase::Restore,
                    };
                    let p = ProgressPayload {
                        op_id: ctx.op_id.clone(),
                        project_id: ctx.project,
                        op: ctx.op,
                        phase,
                        done: e.done,
                        total: e.total,
                        path: e.path.clone(),
                    };
                    if let Ok(v) = serde_json::to_value(&p) {
                        emit(EVENT_PROGRESS, v);
                    }
                })
            };
            Ok(d.snapshot(
                ctx.project,
                SnapshotOptions {
                    label,
                    kind: VersionKind::Manual,
                    progress: Some(progress),
                    cancel: Some(ctx.cancel.clone()),
                },
            )?)
        })?;
        if report.version.is_some() {
            self.versions_changed(ctx.project);
        }
        Ok(report)
    }

    /// Unsaved changes.
    pub fn status(&self, project: ProjectId) -> ApiResult<Vec<FileChange>> {
        Ok(self.dsnap()?.status(project)?)
    }

    // ---- versions ----

    /// Versions newest first.
    pub fn list_versions(&self, project: ProjectId) -> ApiResult<Vec<Version>> {
        Ok(self.dsnap()?.list_versions(project)?)
    }

    /// Set a version's label.
    pub fn set_label(&self, id: VersionId, label: &str) -> ApiResult<Version> {
        let d = self.dsnap()?;
        d.set_label(id, label)?;
        let v = d.version(id)?;
        self.versions_changed(v.project_id);
        Ok(v)
    }

    /// Pin or unpin a version.
    pub fn set_pinned(&self, id: VersionId, pinned: bool) -> ApiResult<Version> {
        let d = self.dsnap()?;
        d.set_pinned(id, pinned)?;
        let v = d.version(id)?;
        self.versions_changed(v.project_id);
        Ok(v)
    }

    /// Delete a version.
    pub fn delete_version(&self, id: VersionId) -> ApiResult<()> {
        let d = self.dsnap()?;
        let v = d.version(id)?;
        d.delete_version(id)?;
        self.versions_changed(v.project_id);
        Ok(())
    }

    // ---- changes ----

    /// Changes from `from` (`None`: the version before `to`) to `to`.
    pub fn changes(
        &self,
        project: ProjectId,
        from: Option<VersionId>,
        to: VersionRef,
    ) -> ApiResult<Vec<FileChange>> {
        Ok(self.dsnap()?.changes(project, from, to)?)
    }

    /// Diff of one file.
    pub fn file_diff(
        &self,
        project: ProjectId,
        from: Option<VersionId>,
        to: VersionRef,
        path: &RelPath,
        opts: &DiffOptions,
    ) -> ApiResult<FileDiff> {
        Ok(self.dsnap()?.file_diff(project, from, to, path, opts)?)
    }

    /// An image blob as a `data:` URL (images up to [`MAX_DATA_URL_BYTES`]).
    pub fn read_blob_as_data_url(&self, hash: &BlobHash, mime: &str) -> ApiResult<String> {
        if !mime.starts_with("image/") || mime.contains([';', ',', ' ']) {
            return Err(ApiError::new(
                "invalid_input",
                format!("not an image type: {mime}"),
            ));
        }
        let bytes = self.dsnap()?.read_blob(hash)?;
        if bytes.len() > MAX_DATA_URL_BYTES {
            return Err(ApiError::new(
                "invalid_input",
                format!("image is larger than {} MB", MAX_DATA_URL_BYTES >> 20),
            ));
        }
        Ok(format!("data:{mime};base64,{}", base64(&bytes)))
    }

    // ---- restore ----

    /// What a whole-project restore would do.
    pub fn restore_plan(&self, project: ProjectId, version: VersionId) -> ApiResult<RestorePlan> {
        Ok(self.dsnap()?.restore_plan(project, version)?)
    }

    /// Restore the whole project (announced with [`Backend::begin`]).
    pub fn restore_project(&self, ctx: &OpCtx, version: VersionId) -> ApiResult<RestoreReport> {
        self.restore_op(ctx, |d| d.restore_project(ctx.project, version))
    }

    /// Restore one file (announced with [`Backend::begin`]).
    pub fn restore_file(
        &self,
        ctx: &OpCtx,
        version: VersionId,
        path: &RelPath,
    ) -> ApiResult<RestoreReport> {
        self.restore_op(ctx, |d| d.restore_file(ctx.project, version, path))
    }

    /// Revert one hunk (announced with [`Backend::begin`]).
    pub fn revert_hunk(&self, ctx: &OpCtx, req: &RevertHunk) -> ApiResult<RestoreReport> {
        self.restore_op(ctx, |d| d.revert_hunk(ctx.project, req))
    }

    fn restore_op(
        &self,
        ctx: &OpCtx,
        f: impl FnOnce(&Dsnap) -> dsnap_core::Result<RestoreReport>,
    ) -> ApiResult<RestoreReport> {
        let res = self.run_write(ctx, |d| {
            self.progress(ctx, Phase::Restore, 0, None, None);
            Ok(f(d)?)
        });
        // A safety version was written even if the restore then failed.
        self.versions_changed(ctx.project);
        res
    }
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        let sym = |shift: u32| char::from(A[((n >> shift) & 63) as usize]);
        out.push(sym(18));
        out.push(sym(12));
        out.push(if c.len() > 1 { sym(6) } else { '=' });
        out.push(if c.len() > 2 { sym(0) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests;
