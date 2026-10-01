import type {
  ApiErrorCode,
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

/** Error every `Api` method rejects with. */
export class ApiError extends Error {
  readonly code: ApiErrorCode;

  constructor(code: ApiErrorCode, message: string) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
  }

  /** Wraps any thrown value as an `ApiError` (`internal` unless it already is one). */
  static from(err: unknown): ApiError {
    if (err instanceof ApiError) return err;
    if (isApiErrorShape(err)) return new ApiError(err.code, err.message);
    const message = err instanceof Error ? err.message : String(err);
    return new ApiError('internal', message);
  }
}

function isApiErrorShape(v: unknown): v is { code: ApiErrorCode; message: string } {
  return (
    typeof v === 'object' &&
    v !== null &&
    typeof (v as { code?: unknown }).code === 'string' &&
    typeof (v as { message?: unknown }).message === 'string'
  );
}

/** Stops an event subscription. */
export type Unsubscribe = () => void;

/**
 * The only way the UI talks to the backend. Methods map 1:1 to Tauri commands
 * (see `commands.ts`). Every method rejects with `ApiError`.
 */
export interface Api {
  // Projects (F1, F2)
  /** All tracked projects, in sidebar order (by name). */
  listProjects(): Promise<Project[]>;
  /** Starts tracking a folder. Rejects `invalid_input` if already tracked. */
  addProject(path: string): Promise<Project>;
  renameProject(id: ProjectId, name: string): Promise<Project>;
  /** Stops tracking; with `deleteSnapshots` also deletes its versions. */
  removeProject(id: ProjectId, deleteSnapshots: boolean): Promise<void>;
  /** Points a missing project at its new folder ("Locate folder"). */
  relocateProject(id: ProjectId, path: string): Promise<Project>;
  /** Opens the system file manager with the folder selected. */
  revealInExplorer(path: string): Promise<void>;
  /** Native folder picker. `null` when the user cancels. */
  pickFolder(): Promise<string | null>;

  // Settings
  getProjectSettings(id: ProjectId): Promise<ProjectSettings>;
  setProjectSettings(id: ProjectId, settings: ProjectSettings): Promise<void>;
  getGlobalSettings(): Promise<GlobalSettings>;
  setGlobalSettings(settings: GlobalSettings): Promise<void>;

  // Snapshots (F4–F7)
  /** Captures the folder. Empty `label` means default (timestamp). `version: null` = nothing changed. */
  snapshot(projectId: ProjectId, label?: string): Promise<SnapshotReport>;
  /** Changes in the folder since the latest version ("Unsaved changes", badge count). */
  status(projectId: ProjectId): Promise<FileChange[]>;
  /** Cancels a running snapshot or restore by `Progress.opId`. Rejects `not_found` if not running. */
  cancelOperation(opId: string): Promise<void>;

  // Versions (F10, F11)
  /** Versions newest first. */
  listVersions(projectId: ProjectId): Promise<Version[]>;
  setLabel(versionId: VersionId, label: string): Promise<Version>;
  setPinned(versionId: VersionId, pinned: boolean): Promise<Version>;
  deleteVersion(versionId: VersionId): Promise<void>;

  // Changes (F13–F18)
  /**
   * Changed files from `from` to `to`, sorted by path. `from: null` compares with
   * nothing (every file added); to compare a version with the one before it, pass
   * the previous version's id.
   */
  changes(projectId: ProjectId, from: VersionId | null, to: VersionRef): Promise<FileChange[]>;
  /** Diff of one file. For a rename, `path` is the new path. */
  fileDiff(
    projectId: ProjectId,
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    opts: DiffOptions,
  ): Promise<FileDiff>;
  /** Blob content as a `data:` URL, for image previews. */
  readBlobAsDataUrl(hash: BlobHash, mime: string): Promise<string>;

  // Restore (F19–F22). Each takes a safety snapshot first.
  /** What `restoreProject` would write, delete and create. Writes nothing. */
  restorePlan(projectId: ProjectId, versionId: VersionId): Promise<RestorePlan>;
  restoreProject(projectId: ProjectId, versionId: VersionId): Promise<RestoreReport>;
  /** Restores one file (also a deleted one). Rejects `not_found` if the version lacks it. */
  restoreFile(projectId: ProjectId, versionId: VersionId, path: RelPath): Promise<RestoreReport>;
  /**
   * Reverts hunk `hunkIndex` of `fileDiff(projectId, from, to, path, DEFAULT_DIFF_OPTIONS)`
   * in the folder. `to` must be the working tree, else `invalid_input`.
   */
  revertHunk(
    projectId: ProjectId,
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    hunkIndex: number,
  ): Promise<RestoreReport>;

  // Events
  /** The folder changed on disk. */
  onProjectChanged(cb: (e: ProjectChangedEvent) => void): Unsubscribe;
  /** Versions changed (GUI, CLI or retention). Re-fetch `listVersions`. */
  onVersionsChanged(cb: (e: VersionsChangedEvent) => void): Unsubscribe;
  /** Snapshot or restore progress. */
  onProgress(cb: (p: Progress) => void): Unsubscribe;
}
