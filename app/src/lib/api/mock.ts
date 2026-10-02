// In-memory `Api` for UI development and tests. Seeded with 3 demo projects.
import { ApiError, type Api, type Unsubscribe } from './api';
import type {
  BlobHash,
  DiffOptions,
  FileChange,
  FileDiff,
  GlobalSettings,
  Progress,
  ProgressOp,
  ProgressPhase,
  Project,
  ProjectChangedEvent,
  ProjectId,
  ProjectSettings,
  RelPath,
  RestorePlan,
  RestoreReport,
  SkippedFile,
  SnapshotReport,
  Version,
  VersionId,
  VersionKind,
  VersionRef,
  VersionsChangedEvent,
} from './types';
import { lineDiff, revertHunkText } from './mock/lines';
import { seedState } from './mock/seed';
import {
  DIFF_SIZE_LIMIT,
  MockState,
  compareTrees,
  countChanges,
  sameContent,
  type ProjectRecord,
  type Tree,
} from './mock/state';
import { imageMime, textToBase64, timestampLabel } from './mock/util';

export interface MockOptions {
  /** Delay before each call resolves, in ms. Default 0. */
  latencyMs?: number;
  /** Seed for the demo data. Default 42. */
  seed?: number;
  /** Clock for new versions and file times. Default `Date.now`. */
  now?: () => number;
}

type Listener<T> = (e: T) => void;

const clone = <T>(v: T): T => structuredClone(v);

/**
 * Mock backend. Implements `Api`, plus a few `mock*` helpers that simulate
 * things the real app sees from outside (file edits, CLI snapshots).
 */
export class MockApi implements Api {
  private state: MockState;
  private latencyMs: number;
  private readonly now: () => number;
  private pickCounter = 0;
  private pickResult: string | null | undefined = undefined;
  private opCounter = 0;
  private readonly ops = new Map<string, { cancelled: boolean }>();
  private readonly projectOps = new Map<ProjectId, Promise<unknown>>();
  private readonly projectListeners = new Set<Listener<ProjectChangedEvent>>();
  private readonly versionListeners = new Set<Listener<VersionsChangedEvent>>();
  private readonly progressListeners = new Set<Listener<Progress>>();

  constructor(opts: MockOptions = {}) {
    this.state = seedState(opts.seed ?? 42);
    this.latencyMs = opts.latencyMs ?? 0;
    this.now = opts.now ?? (() => Date.now());
  }

  // ---- mock-only helpers -------------------------------------------------

  /** Changes the simulated latency. */
  mockSetLatency(ms: number): void {
    this.latencyMs = ms;
  }

  /** Next `pickFolder()` result. `undefined` restores the default (a new fake path each call). */
  mockSetPickFolderResult(path: string | null | undefined): void {
    this.pickResult = path;
  }

  /**
   * Simulates an edit in the project folder. `null` deletes the file. `size`
   * fakes a larger file (e.g. over the size cap). Emits `onProjectChanged`.
   */
  mockWriteFile(projectId: ProjectId, path: RelPath, content: string | null, size?: number): void {
    const rec = this.rec(projectId);
    if (content === null) rec.tree.delete(path);
    else {
      const hash = this.state.putText(content, size);
      rec.tree.set(path, this.state.fileEntry(path, hash, this.now() * 1_000_000));
    }
    this.emitProjectChanged(rec);
  }

  /** Text of a file in the simulated folder, or `null` if absent. */
  mockReadFile(projectId: ProjectId, path: RelPath): string | null {
    const e = this.rec(projectId).tree.get(path);
    return e ? this.state.entryText(e) : null;
  }

  /** Simulates `dsnap snap` from the CLI. Emits `onVersionsChanged`. */
  mockCliSnapshot(projectId: ProjectId, label = 'after agent turn'): Version | null {
    const rec = this.rec(projectId);
    return this.capture(rec, 'cli', label, false).version;
  }

  // ---- Api ---------------------------------------------------------------

  listProjects(): Promise<Project[]> {
    return this.call(() =>
      [...this.state.projects.values()]
        .map((r) => r.project)
        .sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })),
    );
  }

  addProject(path: string): Promise<Project> {
    return this.call(() => {
      const root = path.trim().replace(/[\\/]+$/, '');
      if (!root) throw new ApiError('invalid_input', 'Folder path is empty.');
      for (const r of this.state.projects.values()) {
        if (r.project.root.toLowerCase() === root.toLowerCase()) {
          throw new ApiError('invalid_input', `Folder is already tracked as "${r.project.name}".`);
        }
      }
      const id = this.state.nextProjectId++;
      const name = root.split(/[\\/]/).pop() || root;
      const rec: ProjectRecord = {
        project: {
          id,
          name,
          root,
          settings: { extraIgnore: [], respectGitignore: true, autoSnapshot: { kind: 'off' } },
          missing: false,
        },
        tree: new Map(),
        versions: [],
      };
      const t = this.now() * 1_000_000;
      const files: [string, string][] = [
        ['README.md', `# ${name}\n`],
        ['src/main.ts', "console.log('hello');\n"],
      ];
      for (const [p, text] of files) {
        rec.tree.set(p, this.state.fileEntry(p, this.state.putText(text), t));
      }
      this.state.projects.set(id, rec);
      return rec.project;
    });
  }

  renameProject(id: ProjectId, name: string): Promise<Project> {
    return this.call(() => {
      const rec = this.rec(id);
      const trimmed = name.trim();
      if (!trimmed) throw new ApiError('invalid_input', 'Name is empty.');
      rec.project.name = trimmed;
      return rec.project;
    });
  }

  removeProject(id: ProjectId, deleteSnapshots: boolean): Promise<void> {
    return this.call(() => {
      this.rec(id);
      // The mock forgets versions either way; the real app keeps them unless asked.
      void deleteSnapshots;
      this.state.projects.delete(id);
    });
  }

  relocateProject(id: ProjectId, path: string): Promise<Project> {
    return this.call(() => {
      const rec = this.rec(id);
      const root = path.trim();
      if (!root) throw new ApiError('invalid_input', 'Folder path is empty.');
      rec.project.root = root;
      rec.project.missing = false;
      // Pretend the folder holds the latest version.
      const latest = rec.versions[rec.versions.length - 1];
      if (latest && rec.tree.size === 0) rec.tree = new Map(latest.entries);
      this.emitProjectChanged(rec);
      return rec.project;
    });
  }

  revealInExplorer(path: string): Promise<void> {
    return this.call(() => {
      if (!path.trim()) throw new ApiError('invalid_input', 'Path is empty.');
    });
  }

  pickFolder(): Promise<string | null> {
    return this.call(() => {
      if (this.pickResult !== undefined) return this.pickResult;
      this.pickCounter++;
      return `C:\\Users\\demo\\Projects\\new-project-${this.pickCounter}`;
    });
  }

  getProjectSettings(id: ProjectId): Promise<ProjectSettings> {
    return this.call(() => this.rec(id).project.settings);
  }

  setProjectSettings(id: ProjectId, settings: ProjectSettings): Promise<void> {
    return this.call(() => {
      const rec = this.rec(id);
      const a = settings.autoSnapshot;
      if (a.kind !== 'off' && !(Number.isInteger(a.secs) && a.secs > 0)) {
        throw new ApiError(
          'invalid_input',
          'Auto-snapshot interval must be a positive number of seconds.',
        );
      }
      rec.project.settings = clone(settings);
    });
  }

  getGlobalSettings(): Promise<GlobalSettings> {
    return this.call(() => this.state.global);
  }

  setGlobalSettings(settings: GlobalSettings): Promise<void> {
    return this.call(() => {
      if (!(settings.sizeCapBytes > 0)) {
        throw new ApiError('invalid_input', 'Size cap must be positive.');
      }
      if (!(Number.isInteger(settings.retentionKeep) && settings.retentionKeep >= 1)) {
        throw new ApiError('invalid_input', 'Retention must keep at least 1 version.');
      }
      this.state.global = clone(settings);
      for (const rec of this.state.projects.values()) this.applyRetention(rec);
    });
  }

  async snapshot(projectId: ProjectId, label?: string): Promise<SnapshotReport> {
    return this.operation('snapshot', projectId, (rec) => {
      if (rec.project.missing) throw missing(rec);
      return this.capture(rec, 'manual', label?.trim() || null, false);
    });
  }

  status(projectId: ProjectId): Promise<FileChange[]> {
    return this.call(() => {
      const rec = this.rec(projectId);
      if (rec.project.missing) throw missing(rec);
      return this.statusOf(rec, true);
    });
  }

  cancelOperation(opId: string): Promise<void> {
    return this.call(() => {
      const op = this.ops.get(opId);
      if (!op) throw new ApiError('not_found', `No running operation ${opId}.`);
      op.cancelled = true;
    }, 0);
  }

  listVersions(projectId: ProjectId): Promise<Version[]> {
    return this.call(() =>
      this.rec(projectId)
        .versions.map((v) => v.version)
        .reverse(),
    );
  }

  setLabel(versionId: VersionId, label: string): Promise<Version> {
    return this.call(() => {
      const { rec, vr } = this.version(versionId);
      const trimmed = label.trim();
      if (!trimmed) throw new ApiError('invalid_input', 'Label is empty.');
      vr.version.label = trimmed;
      this.emitVersionsChanged(rec);
      return vr.version;
    });
  }

  setPinned(versionId: VersionId, pinned: boolean): Promise<Version> {
    return this.call(() => {
      const { rec, vr } = this.version(versionId);
      vr.version.pinned = pinned;
      this.emitVersionsChanged(rec);
      return vr.version;
    });
  }

  deleteVersion(versionId: VersionId): Promise<void> {
    return this.call(() => {
      const { rec, index } = this.version(versionId);
      rec.versions.splice(index, 1);
      this.state.recount(rec);
      this.emitVersionsChanged(rec);
      this.emitProjectChanged(rec);
    });
  }

  changes(projectId: ProjectId, from: VersionId | null, to: VersionRef): Promise<FileChange[]> {
    return this.call(() => {
      const rec = this.rec(projectId);
      const [older, newer] = this.sides(rec, from, to);
      return compareTrees(this.state, older, newer, true);
    });
  }

  fileDiff(
    projectId: ProjectId,
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    opts: DiffOptions,
  ): Promise<FileDiff> {
    return this.call(() => this.diffOf(this.rec(projectId), from, to, path, opts));
  }

  readBlobAsDataUrl(hash: BlobHash, mime: string): Promise<string> {
    return this.call(() => {
      const b = this.state.blob(hash);
      if (!b) throw new ApiError('not_found', `Blob ${hash} not found.`);
      const base64 = b.binary ? b.data : textToBase64(b.data);
      return `data:${mime};base64,${base64}`;
    });
  }

  restorePlan(projectId: ProjectId, versionId: VersionId): Promise<RestorePlan> {
    return this.call(() => {
      const rec = this.rec(projectId);
      return this.planOf(rec, this.versionIn(rec, versionId).entries, versionId);
    });
  }

  async restoreProject(projectId: ProjectId, versionId: VersionId): Promise<RestoreReport> {
    return this.operation('restore', projectId, (rec) => {
      if (rec.project.missing) throw missing(rec);
      const target = this.versionIn(rec, versionId);
      const plan = this.planOf(rec, target.entries, versionId);
      const safety = this.safetySnapshot(rec, target.version.label);
      const t = this.now() * 1_000_000;
      for (const p of plan.delete) rec.tree.delete(p);
      for (const p of [...plan.write, ...plan.createDirs]) {
        const e = target.entries.get(p);
        if (e) rec.tree.set(p, { ...e, mtimeNs: t });
      }
      this.emitProjectChanged(rec);
      return {
        safetyVersion: safety,
        written: plan.write,
        deleted: plan.delete,
        failed: [],
        uncaptured: plan.uncaptured,
      };
    });
  }

  restoreFile(projectId: ProjectId, versionId: VersionId, path: RelPath): Promise<RestoreReport> {
    return this.operation('restore', projectId, (rec) => {
      if (rec.project.missing) throw missing(rec);
      const target = this.versionIn(rec, versionId);
      const e = target.entries.get(path);
      if (!e)
        throw new ApiError('not_found', `${path} is not in version "${target.version.label}".`);
      const plan = this.planOf(rec, new Map([[path, e]]), versionId);
      const safety = this.safetySnapshot(rec, target.version.label);
      if (plan.uncaptured.includes(path)) {
        return { safetyVersion: safety, written: [], deleted: [], failed: [], uncaptured: [path] };
      }
      rec.tree.set(path, { ...e, mtimeNs: this.now() * 1_000_000 });
      this.emitProjectChanged(rec);
      return { safetyVersion: safety, written: [path], deleted: [], failed: [], uncaptured: [] };
    });
  }

  revertHunk(
    projectId: ProjectId,
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    hunkIndex: number,
    opts: DiffOptions,
  ): Promise<RestoreReport> {
    if (to.kind !== 'workingTree') {
      return Promise.reject(
        new ApiError('invalid_input', 'Only changes in the folder can be reverted.'),
      );
    }
    return this.operation('restore', projectId, (rec) => {
      if (rec.project.missing) throw missing(rec);
      const label = from === null ? 'empty' : this.versionIn(rec, from).version.label;
      if (this.guard(rec).check(path, true)) {
        // Rule 1: the folder copy is not in any snapshot, so never write over it.
        const safety = this.safetySnapshot(rec, label);
        return { safetyVersion: safety, written: [], deleted: [], failed: [], uncaptured: [path] };
      }
      const diff = this.diffOf(rec, from, to, path, opts);
      if (diff.body.kind !== 'text') {
        throw new ApiError('invalid_input', `${path} has no text hunks.`);
      }
      const hunk = diff.body.hunks[hunkIndex];
      if (!hunk) throw new ApiError('invalid_input', `Hunk ${hunkIndex} does not exist.`);
      const { oldEntry, newEntry } = this.pair(rec, from, to, path);
      const text = revertHunkText(
        this.state.entryText(oldEntry),
        this.state.entryText(newEntry),
        hunk,
      );
      const safety = this.safetySnapshot(rec, label);
      const hash = this.state.putText(text);
      rec.tree.set(path, this.state.fileEntry(path, hash, this.now() * 1_000_000));
      this.emitProjectChanged(rec);
      return { safetyVersion: safety, written: [path], deleted: [], failed: [], uncaptured: [] };
    });
  }

  onProjectChanged(cb: (e: ProjectChangedEvent) => void): Unsubscribe {
    this.projectListeners.add(cb);
    return () => this.projectListeners.delete(cb);
  }

  onVersionsChanged(cb: (e: VersionsChangedEvent) => void): Unsubscribe {
    this.versionListeners.add(cb);
    return () => this.versionListeners.delete(cb);
  }

  onProgress(cb: (p: Progress) => void): Unsubscribe {
    this.progressListeners.add(cb);
    return () => this.progressListeners.delete(cb);
  }

  // ---- internals ---------------------------------------------------------

  /** Runs `fn` after the latency; returns a deep copy and maps throws to `ApiError`. */
  private async call<T>(fn: () => T, latency = this.latencyMs): Promise<T> {
    if (latency > 0) await new Promise((r) => setTimeout(r, latency));
    try {
      return clone(fn());
    } catch (err) {
      throw ApiError.from(err);
    }
  }

  /** Like `call`, with progress events and cancellation (snapshot, restore). */
  private async operation<T>(
    op: ProgressOp,
    projectId: ProjectId,
    fn: (rec: ProjectRecord) => T,
  ): Promise<T> {
    const opId = `op-${++this.opCounter}`;
    const handle = { cancelled: false };
    this.ops.set(opId, handle);
    const emit = (phase: ProgressPhase, done: number, total: number | null) => {
      const p: Progress = { opId, projectId, op, phase, done, total, path: null };
      for (const cb of this.progressListeners) cb(clone(p));
    };
    // One operation per project at a time: later calls wait for earlier ones.
    // The first event fires synchronously, so callers learn the opId at once.
    const previous = this.projectOps.get(projectId);
    emit(previous ? 'queued' : 'walk', 0, null);
    const run = (async () => {
      if (previous) {
        await previous.catch(() => undefined);
        emit('walk', 0, null);
      }
      try {
        const rec = this.rec(projectId);
        const total = rec.tree.size;
        if (this.latencyMs > 0) await new Promise((r) => setTimeout(r, this.latencyMs));
        else await Promise.resolve();
        if (handle.cancelled) throw new ApiError('cancelled', `${op} cancelled.`);
        emit(op === 'snapshot' ? 'hash' : 'restore', Math.floor(total / 2), total);
        const result = fn(rec);
        emit('done', total, total);
        return clone(result);
      } catch (err) {
        throw ApiError.from(err);
      } finally {
        this.ops.delete(opId);
      }
    })();
    this.projectOps.set(projectId, run);
    const clear = () => {
      if (this.projectOps.get(projectId) === run) this.projectOps.delete(projectId);
    };
    run.then(clear, clear);
    return run;
  }

  private rec(id: ProjectId): ProjectRecord {
    const rec = this.state.record(id);
    if (!rec) throw new ApiError('not_found', `Project ${id} not found.`);
    return rec;
  }

  private version(id: VersionId) {
    const found = this.state.findVersion(id);
    if (!found) throw new ApiError('not_found', `Version ${id} not found.`);
    return found;
  }

  private versionIn(rec: ProjectRecord, id: VersionId) {
    const vr = rec.versions.find((v) => v.version.id === id);
    if (!vr) {
      throw new ApiError('not_found', `Version ${id} not found in project "${rec.project.name}".`);
    }
    return vr;
  }

  private side(rec: ProjectRecord, id: VersionId | null): Tree | null {
    return id === null ? null : this.versionIn(rec, id).entries;
  }

  private target(rec: ProjectRecord, to: VersionRef): Tree {
    if (to.kind === 'version') return this.versionIn(rec, to.id).entries;
    if (rec.project.missing) throw missing(rec);
    return this.state.capturable(rec).kept;
  }

  private latest(rec: ProjectRecord): Tree | null {
    return rec.versions[rec.versions.length - 1]?.entries ?? null;
  }

  /**
   * Both sides of a comparison. Against the working tree, paths the folder holds
   * outside any snapshot (ignored, over the size cap, or under such a path) are
   * dropped from the old side too, so they never show as "deleted" and cannot be
   * restored or reverted over.
   */
  private sides(rec: ProjectRecord, from: VersionId | null, to: VersionRef): [Tree | null, Tree] {
    const older = this.side(rec, from);
    const newer = this.target(rec, to);
    if (to.kind !== 'workingTree' || !older) return [older, newer];
    const { check } = this.guard(rec);
    return [new Map([...older].filter(([p]) => !check(p, false))), newer];
  }

  private statusOf(rec: ProjectRecord, withLines: boolean): FileChange[] {
    const latest = this.latest(rec);
    const id = rec.versions[rec.versions.length - 1]?.version.id ?? null;
    if (!latest || id === null) {
      return compareTrees(this.state, null, this.state.capturable(rec).kept, withLines);
    }
    const [older, newer] = this.sides(rec, id, { kind: 'workingTree' });
    return compareTrees(this.state, older, newer, withLines);
  }

  private pair(rec: ProjectRecord, from: VersionId | null, to: VersionRef, path: RelPath) {
    const [older, newer] = this.sides(rec, from, to);
    const change = compareTrees(this.state, older, newer, false).find((c) => c.path === path);
    if (change) return { oldEntry: change.old, newEntry: change.new };
    // Unchanged file: same entry on both sides.
    const e = newer.get(path);
    if (!e) throw new ApiError('not_found', `${path} not found.`);
    return { oldEntry: e, newEntry: e };
  }

  private diffOf(
    rec: ProjectRecord,
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    opts: DiffOptions,
  ): FileDiff {
    const { oldEntry, newEntry } = this.pair(rec, from, to, path);
    const mime = imageMime(path);
    if (mime) {
      return {
        path,
        body: { kind: 'image', old: oldEntry?.blob ?? null, new: newEntry?.blob ?? null, mime },
      };
    }
    if (this.state.isBinary(oldEntry) || this.state.isBinary(newEntry)) {
      return {
        path,
        body: {
          kind: 'binary',
          oldSize: oldEntry?.size ?? null,
          newSize: newEntry?.size ?? null,
          oldHash: oldEntry?.blob ?? null,
          newHash: newEntry?.blob ?? null,
        },
      };
    }
    const size = Math.max(oldEntry?.size ?? 0, newEntry?.size ?? 0);
    if (size > DIFF_SIZE_LIMIT) return { path, body: { kind: 'tooLarge', size } };
    const d = lineDiff(this.state.entryText(oldEntry), this.state.entryText(newEntry), opts);
    return { path, body: { kind: 'text', hunks: d.hunks } };
  }

  /**
   * Rule 1 guard. `check(p, asFile)` is true when writing or deleting `p` could
   * replace or remove content that no safety snapshot holds: `p` is ignored, or
   * is in the folder but not capturable (over the size cap), or has such a path
   * as an ancestor, or (when `p` would become a file) as a descendant.
   */
  private guard(rec: ProjectRecord): {
    kept: Tree;
    check: (p: RelPath, asFile: boolean) => boolean;
  } {
    const { kept } = this.state.capturable(rec);
    const held = [...rec.tree.keys()].filter((p) => !kept.has(p));
    const heldSet = new Set(held);
    const check = (p: RelPath, asFile: boolean) =>
      this.state.ignored(rec, p) ||
      heldSet.has(p) ||
      held.some((q) => p.startsWith(`${q}/`) || (asFile && q.startsWith(`${p}/`)));
    return { kept, check };
  }

  /**
   * Restore plan. Paths the restore would change but where that could replace
   * or remove uncaptured content (see `guard`) go to `uncaptured` and are never
   * written, created or deleted (Rule 1, DSNA-87).
   */
  private planOf(rec: ProjectRecord, target: Tree, versionId: VersionId): RestorePlan {
    const write: RelPath[] = [];
    const del: RelPath[] = [];
    const createDirs: RelPath[] = [];
    const uncaptured = new Set<RelPath>();
    const { check } = this.guard(rec);
    for (const [p, e] of target) {
      const cur = rec.tree.get(p);
      if (cur && sameContent(cur, e)) continue;
      const isDir = e.kind.kind === 'dir';
      if (check(p, !isDir)) {
        // An existing dir at p needs nothing; anything else held there must stay.
        if (!isDir || cur?.kind.kind !== 'dir') uncaptured.add(p);
      } else if (isDir) {
        if (!cur) createDirs.push(p);
      } else write.push(p);
    }
    for (const [p, e] of rec.tree) {
      if (target.has(p) || e.kind.kind === 'dir') continue;
      if (check(p, true)) uncaptured.add(p);
      else del.push(p);
    }
    return {
      target: versionId,
      write: write.sort(),
      delete: del.sort(),
      createDirs: createDirs.sort(),
      uncaptured: [...uncaptured].sort(),
    };
  }

  /** Snapshot of the folder; `force` creates a version even when nothing changed. */
  private capture(
    rec: ProjectRecord,
    kind: VersionKind,
    label: string | null,
    force: boolean,
  ): SnapshotReport {
    const { kept, tooLarge } = this.state.capturable(rec);
    const skipped: SkippedFile[] = tooLarge.map((e) => ({
      path: e.path,
      reason: { kind: 'tooLarge', size: e.size },
    }));
    const changes = compareTrees(this.state, this.latest(rec), kept, false);
    if (changes.length === 0 && !force) return { version: null, skipped, unstablePaths: [] };
    const now = this.now();
    const version: Version = {
      id: this.state.nextVersionId++,
      projectId: rec.project.id,
      label: label ?? timestampLabel(now),
      createdAtMs: now,
      kind,
      pinned: false,
      unstable: false,
      counts: countChanges(changes),
    };
    rec.versions.push({ version, entries: new Map(kept) });
    this.applyRetention(rec);
    this.emitVersionsChanged(rec);
    this.emitProjectChanged(rec);
    return { version, skipped, unstablePaths: [] };
  }

  private safetySnapshot(rec: ProjectRecord, targetLabel: string): VersionId {
    const report = this.capture(rec, 'safety', `Before restore to ${targetLabel}`, true);
    if (!report.version) {
      throw new ApiError('safety_snapshot_failed', 'Safety snapshot failed; nothing was restored.');
    }
    return report.version.id;
  }

  /** Keeps pinned versions plus the newest `retentionKeep` others. */
  private applyRetention(rec: ProjectRecord): void {
    const keep = this.state.global.retentionKeep;
    const unpinned = rec.versions.filter((v) => !v.version.pinned);
    const excess = unpinned.length - keep;
    if (excess <= 0) return;
    const drop = new Set(unpinned.slice(0, excess).map((v) => v.version.id));
    rec.versions = rec.versions.filter((v) => !drop.has(v.version.id));
    this.state.recount(rec);
    this.emitVersionsChanged(rec);
  }

  private emitProjectChanged(rec: ProjectRecord): void {
    const changedCount = rec.project.missing ? 0 : this.statusOf(rec, false).length;
    const e: ProjectChangedEvent = { projectId: rec.project.id, changedCount };
    for (const cb of this.projectListeners) cb({ ...e });
  }

  private emitVersionsChanged(rec: ProjectRecord): void {
    for (const cb of this.versionListeners) cb({ projectId: rec.project.id });
  }
}

function missing(rec: ProjectRecord): ApiError {
  return new ApiError('project_missing', `Folder not found: ${rec.project.root}`);
}

/** Creates a mock `Api` with fresh seeded data. */
export function createMockApi(opts: MockOptions = {}): MockApi {
  return new MockApi(opts);
}
