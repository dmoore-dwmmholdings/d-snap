/**
 * TypeScript mirrors of the `dsnap-core` shared types (DSNA-24), as they cross
 * the Tauri boundary as JSON.
 *
 * Serde mapping the Rust side must follow:
 * - Structs: `#[serde(rename_all = "camelCase")]`.
 * - Unit-only enums (`VersionKind`, `LineTag`): `#[serde(rename_all = "camelCase")]`,
 *   so they serialize as plain strings.
 * - Enums with data: `#[serde(tag = "kind", rename_all = "camelCase")]` (internally
 *   tagged), with variant fields in camelCase.
 * - `VersionRef`: `#[serde(tag = "kind", content = "id", rename_all = "camelCase")]`
 *   (adjacently tagged), giving `{ kind: "version", id }` or `{ kind: "workingTree" }`.
 * - `ProjectId` / `VersionId`: transparent integers. `BlobHash`: 64-char lowercase hex
 *   string. `RelPath`: transparent string. `PathBuf`: string.
 * - Tuples serialize as arrays: `(RelPath, String)` is `[path, message]`.
 */

/** Project id (`ProjectId(i64)`). */
export type ProjectId = number;
/** Version id (`VersionId(i64)`). */
export type VersionId = number;
/** BLAKE3 hash as 64 lowercase hex characters (`BlobHash`). */
export type BlobHash = string;
/** Project-relative, `/`-separated path (`RelPath`). */
export type RelPath = string;

/** What an entry is. Symlinks store their target and are never followed. */
export type EntryKind = { kind: 'file' } | { kind: 'symlink'; target: string } | { kind: 'dir' };

/** One path in a version's full file list. */
export interface Entry {
  path: RelPath;
  kind: EntryKind;
  /** Content hash; `null` for directories and symlinks. */
  blob: BlobHash | null;
  size: number;
  /**
   * Modification time in nanoseconds since the Unix epoch. Values above 2^53 lose
   * precision in JS; treat as approximate (display only).
   */
  mtimeNs: number;
  readonly: boolean;
}

/** How a version was created. */
export type VersionKind = 'manual' | 'auto' | 'cli' | 'safety';

/** Added/modified/deleted file counts relative to the previous version. */
export interface ChangeCounts {
  added: number;
  modified: number;
  deleted: number;
}

/** One snapshot of a project. */
export interface Version {
  id: VersionId;
  projectId: ProjectId;
  label: string;
  createdAtMs: number;
  kind: VersionKind;
  /** Pinned versions are never pruned by retention. */
  pinned: boolean;
  /** Some files kept changing while the snapshot was taken. */
  unstable: boolean;
  counts: ChangeCounts;
}

/** Auto-snapshot mode for a project (F8). */
export type AutoSnapshot =
  { kind: 'off' } | { kind: 'every'; secs: number } | { kind: 'afterIdle'; secs: number };

/** Per-project settings. */
export interface ProjectSettings {
  /** Extra gitignore-style patterns. */
  extraIgnore: string[];
  /** Follow the project's `.gitignore` (DSNA-3 default: true). */
  respectGitignore: boolean;
  autoSnapshot: AutoSnapshot;
}

/** App-wide settings. */
export interface GlobalSettings {
  /** Files larger than this are skipped (default 50 MiB). */
  sizeCapBytes: number;
  /** Retention keeps pinned versions plus this many latest (default 200). */
  retentionKeep: number;
}

/** A tracked folder. */
export interface Project {
  id: ProjectId;
  name: string;
  /** Absolute path of the project folder. */
  root: string;
  settings: ProjectSettings;
  /** The folder no longer exists at `root`. */
  missing: boolean;
}

/** How a file differs between two sides of a comparison. */
export type ChangeStatus =
  | { kind: 'added' }
  | { kind: 'modified' }
  | { kind: 'deleted' }
  | { kind: 'renamed'; from: RelPath };

/** One changed path between two sides. */
export interface FileChange {
  /** New path (for deletions, the deleted path). */
  path: RelPath;
  status: ChangeStatus;
  old: Entry | null;
  new: Entry | null;
  /** `null` when not computed (binary, too large, or not yet loaded). */
  linesAdded: number | null;
  linesRemoved: number | null;
}

/** Options for a line diff. */
export interface DiffOptions {
  /** Ignore whitespace and line-ending differences. */
  ignoreWhitespace: boolean;
  /** Context lines around each hunk. */
  context: number;
}

/** Line role in a hunk. */
export type LineTag = 'equal' | 'insert' | 'delete';

/** One line in a hunk. Line numbers are 1-based; `null` when the line is absent on that side. */
export interface DiffLine {
  tag: LineTag;
  oldNo: number | null;
  newNo: number | null;
  /** Line text without its line ending. */
  text: string;
}

/** A diff hunk. Starts are 1-based (0 when the side is empty). */
export interface Hunk {
  oldStart: number;
  oldLen: number;
  newStart: number;
  newLen: number;
  lines: DiffLine[];
}

/** Body of a file diff. */
export type DiffBody =
  | { kind: 'text'; hunks: Hunk[] }
  | {
      kind: 'binary';
      oldSize: number | null;
      newSize: number | null;
      oldHash: BlobHash | null;
      newHash: BlobHash | null;
    }
  | { kind: 'image'; old: BlobHash | null; new: BlobHash | null; mime: string }
  | { kind: 'tooLarge'; size: number };

/** Diff of one file. */
export interface FileDiff {
  path: RelPath;
  body: DiffBody;
}

/** One side of a comparison: a stored version or the folder as it is now. */
export type VersionRef = { kind: 'version'; id: VersionId } | { kind: 'workingTree' };

/** Why a file was left out of a snapshot. */
export type SkipReason =
  | { kind: 'tooLarge'; size: number }
  | { kind: 'locked' }
  | { kind: 'nonUtf8Name' }
  | { kind: 'unreadable'; msg: string };

/** A file left out of a snapshot. */
export interface SkippedFile {
  path: RelPath;
  reason: SkipReason;
}

/** Result of a snapshot. `version === null` means nothing changed (F7). */
export interface SnapshotReport {
  version: Version | null;
  skipped: SkippedFile[];
  unstablePaths: RelPath[];
}

/** What a whole-project restore would do. Shown in the confirm dialog. */
export interface RestorePlan {
  target: VersionId;
  write: RelPath[];
  delete: RelPath[];
  createDirs: RelPath[];
  /**
   * Paths the restore would change but leaves alone, because their current
   * content cannot be in the safety snapshot: ignored now, or over the size cap
   * (Rule 1, F20). The confirm dialog must list them.
   */
  uncaptured: RelPath[];
}

/** Result of a restore (project, file or hunk). */
export interface RestoreReport {
  /** The safety snapshot taken before writing (F21). */
  safetyVersion: VersionId;
  written: RelPath[];
  deleted: RelPath[];
  /** Files not written, as `[path, error message]`. */
  failed: [RelPath, string][];
  /** Paths left untouched because the safety snapshot could not hold them (see `RestorePlan`). */
  uncaptured: RelPath[];
}

/** Long-running operation kind that reports progress. */
export type ProgressOp = 'snapshot' | 'restore';

/**
 * Progress phase. `walk`, `hash` and `restore` mirror core `ProgressEvent.stage`;
 * `queued` (waiting for another operation on the same project) and `done` are
 * added by the app layer.
 */
export type ProgressPhase = 'queued' | 'walk' | 'hash' | 'restore' | 'done';

/** Progress event for a snapshot or restore (`dsnap://progress`). */
export interface Progress {
  /** Operation id; pass to `Api.cancelOperation`. */
  opId: string;
  projectId: ProjectId;
  op: ProgressOp;
  phase: ProgressPhase;
  done: number;
  /** `null` while the total is unknown (still walking). */
  total: number | null;
  /** Path being processed, when known. */
  path: RelPath | null;
}

/** Payload of `onProjectChanged`: the folder changed on disk (watcher, F3). */
export interface ProjectChangedEvent {
  projectId: ProjectId;
  /** Files changed since the latest snapshot (sidebar badge). */
  changedCount: number;
}

/** Payload of `onVersionsChanged`: versions were added, edited or removed (GUI or CLI). */
export interface VersionsChangedEvent {
  projectId: ProjectId;
}

/** Stable error codes (DSNA-66 maps `dsnap_core::Error` to these). */
export const API_ERROR_CODES = [
  'not_found',
  'project_missing',
  'safety_snapshot_failed',
  'cancelled',
  'io',
  'invalid_input',
  'corrupt',
  'db',
  'internal',
] as const;

/** One of `API_ERROR_CODES`. */
export type ApiErrorCode = (typeof API_ERROR_CODES)[number];

/** Default diff options used by the UI. */
export const DEFAULT_DIFF_OPTIONS: DiffOptions = { ignoreWhitespace: false, context: 3 };
