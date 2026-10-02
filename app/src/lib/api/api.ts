import { API_ERROR_CODES } from './types';
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

  /**
   * Wraps any thrown value as an `ApiError`. A `{ code, message }` object keeps its
   * code only if it is a known `ApiErrorCode`; anything else becomes `internal`.
   */
  static from(err: unknown): ApiError {
    if (err instanceof ApiError) return err;
    if (isErrorShape(err)) {
      if (isApiErrorCode(err.code)) return new ApiError(err.code, err.message);
      return new ApiError('internal', `${err.code}: ${err.message}`);
    }
    const message = err instanceof Error ? err.message : String(err);
    return new ApiError('internal', message);
  }
}

/** Whether `code` is a known `ApiErrorCode`. */
export function isApiErrorCode(code: unknown): code is ApiErrorCode {
  return (API_ERROR_CODES as readonly unknown[]).includes(code);
}

function isErrorShape(v: unknown): v is { code: string; message: string } {
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
  /** Where D-Snap keeps its database and snapshots (read-only display in Settings). */
  getDataDir(): Promise<string>;
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
  /**
   * Changes in the folder since the latest version ("Unsaved changes", badge
   * count). Paths outside any snapshot (ignored, over the size cap) are left out
   * on both sides, so they never show as deleted. The same applies to every
   * comparison against the working tree.
   */
  status(projectId: ProjectId): Promise<FileChange[]>;
  /**
   * Cancels a queued or running snapshot or restore by `Progress.opId`. Rejects
   * `not_found` if it already finished.
   *
   * Correlation rule: writes on one project (`snapshot`, `restoreProject`,
   * `restoreFile`, `revertHunk`) run one at a time; later calls queue. Each call emits its first `Progress` event (phase `queued` or `walk`)
   * before the method returns its promise, so the latest `Progress` with this
   * `projectId` and `op` carries the call's `opId`.
   */
  cancelOperation(opId: string): Promise<void>;

  // Versions (F10, F11)
  /** Versions newest first. */
  listVersions(projectId: ProjectId): Promise<Version[]>;
  setLabel(versionId: VersionId, label: string): Promise<Version>;
  setPinned(versionId: VersionId, pinned: boolean): Promise<Version>;
  deleteVersion(versionId: VersionId): Promise<void>;

  // Changes (F13–F18)
  /**
   * Changed files from `from` to `to`, sorted by path. `from: null` means the
   * version before `to` (for the working tree, the latest version); with no such
   * version, every file is added. The same holds for `fileDiff` and `revertHunk`.
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

  // Restore (F19–F22). Each takes a safety snapshot first; if that fails the
  // call rejects `safety_snapshot_failed` and writes nothing. Paths the safety
  // snapshot cannot hold (ignored, over the size cap) are never written or
  // deleted; they come back in `uncaptured`.
  /** What `restoreProject` would write, delete and create, and what it leaves (`uncaptured`). Writes nothing. */
  restorePlan(projectId: ProjectId, versionId: VersionId): Promise<RestorePlan>;
  restoreProject(projectId: ProjectId, versionId: VersionId): Promise<RestoreReport>;
  /**
   * Restores one file (also a deleted one). Rejects `not_found` if the version
   * lacks it. If the path is uncaptured, nothing is written and the report's
   * `uncaptured` holds the path.
   */
  restoreFile(projectId: ProjectId, versionId: VersionId, path: RelPath): Promise<RestoreReport>;
  /**
   * Reverts hunk `hunkIndex` of `fileDiff(projectId, from, to, path, opts)` in the
   * folder; pass the same `opts` the diff view used. `to` must be the working
   * tree, else `invalid_input`. If the path is uncaptured (see
   * `RestorePlan.uncaptured`), nothing is written and the report's `uncaptured`
   * holds the path.
   */
  revertHunk(
    projectId: ProjectId,
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    hunkIndex: number,
    opts: DiffOptions,
  ): Promise<RestoreReport>;

  // Events
  /** The folder changed on disk. */
  onProjectChanged(cb: (e: ProjectChangedEvent) => void): Unsubscribe;
  /** The project list changed (a folder went missing, the CLI edited a project). Re-fetch `listProjects`. */
  onProjectsChanged(cb: () => void): Unsubscribe;
  /** Versions changed (GUI, CLI or retention). Re-fetch `listVersions`. */
  onVersionsChanged(cb: (e: VersionsChangedEvent) => void): Unsubscribe;
  /** Snapshot or restore progress. */
  onProgress(cb: (p: Progress) => void): Unsubscribe;
}
