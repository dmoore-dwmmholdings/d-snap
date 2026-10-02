// The `Api` over the Tauri backend (DSNA-67): one `invoke` per command in `COMMANDS`,
// one `listen` per event in `EVENTS`. Argument keys are the `Api` parameter names; Tauri
// maps them to the snake_case parameters of the Rust commands.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ApiError, type Api, type Unsubscribe } from './api';
import { COMMANDS, EVENTS } from './commands';
import type {
  BlobHash,
  DiffOptions,
  FileChange,
  FileDiff,
  GlobalSettings,
  Progress,
  Project,
  ProjectChangedEvent,
  ProjectId,
  ProjectSettings,
  RelPath,
  RestorePlan,
  RestoreReport,
  SnapshotReport,
  Version,
  VersionId,
  VersionRef,
  VersionsChangedEvent,
} from './types';

/** Invoke `cmd`, turning any rejection into an `ApiError`. */
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    throw ApiError.from(err);
  }
}

/** Listen to `event`; the returned function stops listening, even before `listen` settles. */
function subscribe<T>(event: string, cb: (payload: T) => void): Unsubscribe {
  let stop: (() => void) | null = null;
  let stopped = false;
  listen<T>(event, (e) => cb(e.payload)).then(
    (unlisten) => {
      if (stopped) unlisten();
      else stop = unlisten;
    },
    (err: unknown) => console.error(`cannot listen to ${event}:`, err),
  );
  return () => {
    stopped = true;
    stop?.();
  };
}

/** `Api` backed by the Tauri commands. */
export class TauriApi implements Api {
  listProjects(): Promise<Project[]> {
    return call(COMMANDS.listProjects);
  }

  addProject(path: string): Promise<Project> {
    return call(COMMANDS.addProject, { path });
  }

  renameProject(id: ProjectId, name: string): Promise<Project> {
    return call(COMMANDS.renameProject, { id, name });
  }

  removeProject(id: ProjectId, deleteSnapshots: boolean): Promise<void> {
    return call(COMMANDS.removeProject, { id, deleteSnapshots });
  }

  relocateProject(id: ProjectId, path: string): Promise<Project> {
    return call(COMMANDS.relocateProject, { id, path });
  }

  revealInExplorer(path: string): Promise<void> {
    return call(COMMANDS.revealInExplorer, { path });
  }

  getDataDir(): Promise<string> {
    return call(COMMANDS.getDataDir);
  }

  pickFolder(): Promise<string | null> {
    return call(COMMANDS.pickFolder);
  }

  getProjectSettings(id: ProjectId): Promise<ProjectSettings> {
    return call(COMMANDS.getProjectSettings, { id });
  }

  setProjectSettings(id: ProjectId, settings: ProjectSettings): Promise<void> {
    return call(COMMANDS.setProjectSettings, { id, settings });
  }

  getGlobalSettings(): Promise<GlobalSettings> {
    return call(COMMANDS.getGlobalSettings);
  }

  setGlobalSettings(settings: GlobalSettings): Promise<void> {
    return call(COMMANDS.setGlobalSettings, { settings });
  }

  snapshot(projectId: ProjectId, label?: string): Promise<SnapshotReport> {
    return call(COMMANDS.snapshot, { projectId, label: label ?? null });
  }

  status(projectId: ProjectId): Promise<FileChange[]> {
    return call(COMMANDS.status, { projectId });
  }

  cancelOperation(opId: string): Promise<void> {
    return call(COMMANDS.cancelOperation, { opId });
  }

  listVersions(projectId: ProjectId): Promise<Version[]> {
    return call(COMMANDS.listVersions, { projectId });
  }

  setLabel(versionId: VersionId, label: string): Promise<Version> {
    return call(COMMANDS.setLabel, { versionId, label });
  }

  setPinned(versionId: VersionId, pinned: boolean): Promise<Version> {
    return call(COMMANDS.setPinned, { versionId, pinned });
  }

  deleteVersion(versionId: VersionId): Promise<void> {
    return call(COMMANDS.deleteVersion, { versionId });
  }

  changes(projectId: ProjectId, from: VersionId | null, to: VersionRef): Promise<FileChange[]> {
    return call(COMMANDS.changes, { projectId, from, to });
  }

  fileDiff(
    projectId: ProjectId,
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    opts: DiffOptions,
  ): Promise<FileDiff> {
    return call(COMMANDS.fileDiff, { projectId, from, to, path, opts });
  }

  readBlobAsDataUrl(hash: BlobHash, mime: string): Promise<string> {
    return call(COMMANDS.readBlobAsDataUrl, { hash, mime });
  }

  restorePlan(projectId: ProjectId, versionId: VersionId): Promise<RestorePlan> {
    return call(COMMANDS.restorePlan, { projectId, versionId });
  }

  restoreProject(projectId: ProjectId, versionId: VersionId): Promise<RestoreReport> {
    return call(COMMANDS.restoreProject, { projectId, versionId });
  }

  restoreFile(projectId: ProjectId, versionId: VersionId, path: RelPath): Promise<RestoreReport> {
    return call(COMMANDS.restoreFile, { projectId, versionId, path });
  }

  revertHunk(
    projectId: ProjectId,
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    hunkIndex: number,
    opts: DiffOptions,
  ): Promise<RestoreReport> {
    return call(COMMANDS.revertHunk, { projectId, from, to, path, hunkIndex, opts });
  }

  onProjectChanged(cb: (e: ProjectChangedEvent) => void): Unsubscribe {
    return subscribe(EVENTS.onProjectChanged, cb);
  }

  onProjectsChanged(cb: () => void): Unsubscribe {
    return subscribe(EVENTS.onProjectsChanged, () => cb());
  }

  onVersionsChanged(cb: (e: VersionsChangedEvent) => void): Unsubscribe {
    return subscribe(EVENTS.onVersionsChanged, cb);
  }

  onProgress(cb: (p: Progress) => void): Unsubscribe {
    return subscribe(EVENTS.onProgress, cb);
  }
}
