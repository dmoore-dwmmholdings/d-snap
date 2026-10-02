//! One `#[tauri::command]` per entry of `COMMANDS` in `app/src/lib/api/commands.ts`, with
//! the same names. Each runs its work on the blocking pool, never on the main thread.
//! Arguments arrive as the camelCase keys the `Api` methods use (Tauri maps them to these
//! snake_case parameters).

use std::sync::Arc;

use dsnap_core::{
    BlobHash, DiffOptions, FileChange, FileDiff, GlobalSettings, Project, ProjectId,
    ProjectSettings, RelPath, RestorePlan, RestoreReport, RevertHunk, SnapshotReport, Version,
    VersionId, VersionRef,
};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use dsnap_bridge::backend::{ApiResult, Backend, Op};
use dsnap_bridge::error::ApiError;

/// Managed state: the shared backend.
pub struct AppState(pub Arc<Backend>);

/// Run `f` with the backend on the blocking pool.
async fn blocking<T: Send + 'static>(
    state: &State<'_, AppState>,
    f: impl FnOnce(&Backend) -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    let b = Arc::clone(&state.0);
    tauri::async_runtime::spawn_blocking(move || f(&b))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> ApiResult<Vec<Project>> {
    blocking(&state, Backend::list_projects).await
}

#[tauri::command]
pub async fn add_project(state: State<'_, AppState>, path: String) -> ApiResult<Project> {
    blocking(&state, move |b| b.add_project(&path)).await
}

#[tauri::command]
pub async fn rename_project(
    state: State<'_, AppState>,
    id: ProjectId,
    name: String,
) -> ApiResult<Project> {
    blocking(&state, move |b| b.rename_project(id, &name)).await
}

#[tauri::command]
pub async fn remove_project(
    state: State<'_, AppState>,
    id: ProjectId,
    delete_snapshots: bool,
) -> ApiResult<()> {
    blocking(&state, move |b| b.remove_project(id, delete_snapshots)).await
}

#[tauri::command]
pub async fn relocate_project(
    state: State<'_, AppState>,
    id: ProjectId,
    path: String,
) -> ApiResult<Project> {
    blocking(&state, move |b| b.relocate_project(id, &path)).await
}

#[tauri::command]
pub async fn reveal_in_explorer(path: String) -> ApiResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        tauri_plugin_opener::reveal_item_in_dir(&path)
            .map_err(|e| ApiError::new("io", e.to_string()))
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> ApiResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .blocking_pick_folder()
            .map(|p| {
                p.into_path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .map_err(|e| ApiError::new("io", e.to_string()))
            })
            .transpose()
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn get_project_settings(
    state: State<'_, AppState>,
    id: ProjectId,
) -> ApiResult<ProjectSettings> {
    blocking(&state, move |b| b.get_project_settings(id)).await
}

#[tauri::command]
pub async fn set_project_settings(
    state: State<'_, AppState>,
    id: ProjectId,
    settings: ProjectSettings,
) -> ApiResult<()> {
    blocking(&state, move |b| b.set_project_settings(id, &settings)).await
}

#[tauri::command]
pub async fn get_global_settings(state: State<'_, AppState>) -> ApiResult<GlobalSettings> {
    blocking(&state, Backend::get_global_settings).await
}

#[tauri::command]
pub async fn set_global_settings(
    state: State<'_, AppState>,
    settings: GlobalSettings,
) -> ApiResult<()> {
    blocking(&state, move |b| b.set_global_settings(&settings)).await
}

#[tauri::command]
pub async fn snapshot(
    state: State<'_, AppState>,
    project_id: ProjectId,
    label: Option<String>,
) -> ApiResult<SnapshotReport> {
    let ctx = state.0.begin(project_id, Op::Snapshot);
    blocking(&state, move |b| b.snapshot(&ctx, label)).await
}

#[tauri::command]
pub async fn status(
    state: State<'_, AppState>,
    project_id: ProjectId,
) -> ApiResult<Vec<FileChange>> {
    blocking(&state, move |b| b.status(project_id)).await
}

#[tauri::command]
pub async fn cancel_operation(state: State<'_, AppState>, op_id: String) -> ApiResult<()> {
    state.0.cancel_operation(&op_id)
}

#[tauri::command]
pub async fn list_versions(
    state: State<'_, AppState>,
    project_id: ProjectId,
) -> ApiResult<Vec<Version>> {
    blocking(&state, move |b| b.list_versions(project_id)).await
}

#[tauri::command]
pub async fn set_label(
    state: State<'_, AppState>,
    version_id: VersionId,
    label: String,
) -> ApiResult<Version> {
    blocking(&state, move |b| b.set_label(version_id, &label)).await
}

#[tauri::command]
pub async fn set_pinned(
    state: State<'_, AppState>,
    version_id: VersionId,
    pinned: bool,
) -> ApiResult<Version> {
    blocking(&state, move |b| b.set_pinned(version_id, pinned)).await
}

#[tauri::command]
pub async fn delete_version(state: State<'_, AppState>, version_id: VersionId) -> ApiResult<()> {
    blocking(&state, move |b| b.delete_version(version_id)).await
}

#[tauri::command]
pub async fn changes(
    state: State<'_, AppState>,
    project_id: ProjectId,
    from: Option<VersionId>,
    to: VersionRef,
) -> ApiResult<Vec<FileChange>> {
    blocking(&state, move |b| b.changes(project_id, from, to)).await
}

#[tauri::command]
pub async fn file_diff(
    state: State<'_, AppState>,
    project_id: ProjectId,
    from: Option<VersionId>,
    to: VersionRef,
    path: RelPath,
    opts: DiffOptions,
) -> ApiResult<FileDiff> {
    blocking(&state, move |b| {
        b.file_diff(project_id, from, to, &path, &opts)
    })
    .await
}

#[tauri::command]
pub async fn read_blob_as_data_url(
    state: State<'_, AppState>,
    hash: BlobHash,
    mime: String,
) -> ApiResult<String> {
    blocking(&state, move |b| b.read_blob_as_data_url(&hash, &mime)).await
}

#[tauri::command]
pub async fn restore_plan(
    state: State<'_, AppState>,
    project_id: ProjectId,
    version_id: VersionId,
) -> ApiResult<RestorePlan> {
    blocking(&state, move |b| b.restore_plan(project_id, version_id)).await
}

#[tauri::command]
pub async fn restore_project(
    state: State<'_, AppState>,
    project_id: ProjectId,
    version_id: VersionId,
) -> ApiResult<RestoreReport> {
    let ctx = state.0.begin(project_id, Op::Restore);
    blocking(&state, move |b| b.restore_project(&ctx, version_id)).await
}

#[tauri::command]
pub async fn restore_file(
    state: State<'_, AppState>,
    project_id: ProjectId,
    version_id: VersionId,
    path: RelPath,
) -> ApiResult<RestoreReport> {
    let ctx = state.0.begin(project_id, Op::Restore);
    blocking(&state, move |b| b.restore_file(&ctx, version_id, &path)).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // mirrors Api.revertHunk
pub async fn revert_hunk(
    state: State<'_, AppState>,
    project_id: ProjectId,
    from: Option<VersionId>,
    to: VersionRef,
    path: RelPath,
    hunk_index: u32,
    opts: DiffOptions,
) -> ApiResult<RestoreReport> {
    let ctx = state.0.begin(project_id, Op::Restore);
    let req = RevertHunk {
        from,
        to,
        path,
        hunk_index,
        opts,
        working_hash: None,
    };
    blocking(&state, move |b| b.revert_hunk(&ctx, &req)).await
}
