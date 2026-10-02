// App state (DSNA-41): the selected project, version and file, what is loaded for them, and
// the actions the views call. Plain Svelte 5 runes; views get the store from context.
import { getContext, setContext } from 'svelte';
import {
  ApiError,
  type Api,
  type FileChange,
  type Progress,
  type Project,
  type ProjectId,
  type RelPath,
  type RestorePlan,
  type RestoreReport,
  type SnapshotReport,
  type Unsubscribe,
  type Version,
  type VersionId,
  type VersionRef,
} from '../api';
import { Toasts } from '../toast/toasts.svelte';

/** What the versions list has selected. */
export type Selection = { kind: 'unsaved' } | { kind: 'version'; id: VersionId };

/** Which main screen is shown. */
export type View = 'main' | 'settings';

/** The two sides the changes view compares. */
export interface Sides {
  from: VersionId | null;
  to: VersionRef;
}

const KEY = Symbol('dsnap-store');

/** Make `store` available to child components. */
export function provideStore(store: AppStore): void {
  setContext(KEY, store);
}

/** Context map holding `store`, for mounting a component on its own (tests). */
export function storeContext(store: AppStore): Map<unknown, unknown> {
  return new Map([[KEY, store]]);
}

/** The store provided by an ancestor. */
export function useStore(): AppStore {
  const s = getContext<AppStore | undefined>(KEY);
  if (!s) throw new Error('AppStore not provided');
  return s;
}

function message(err: unknown): string {
  return ApiError.from(err).message;
}

export class AppStore {
  readonly api: Api;
  readonly toasts: Toasts;

  view = $state<View>('main');
  projects = $state<Project[]>([]);
  /** Changed-file count per project (sidebar badge). */
  counts = $state<Record<ProjectId, number>>({});
  projectId = $state<ProjectId | null>(null);
  versions = $state<Version[]>([]);
  selection = $state<Selection | null>(null);
  /** Pair compared in the changes view: `null` = the selection's default (F16 overrides). */
  compare = $state<Sides | null>(null);
  changes = $state<FileChange[]>([]);
  path = $state<RelPath | null>(null);
  /** Latest progress per project (snapshot/restore). */
  progress = $state<Record<ProjectId, Progress>>({});
  /** A snapshot is running for the selected project. */
  snapshotting = $state(false);
  /** Last snapshot report of the selected project, for the skipped-files notice. */
  lastSnapshot = $state<SnapshotReport | null>(null);
  loading = $state({ projects: true, versions: false, changes: false });
  /** A restore that could not write every file (shown in RestoreResultDialog). */
  restoreResult = $state<RestoreReport | null>(null);
  /**
   * Set by a screen with unsaved edits (Settings). Called before navigating away with the
   * navigation to run later; returning `false` stops it for now.
   */
  leaveGuard: ((proceed: () => void) => boolean) | null = null;
  errors = $state<{ versions: string | null; changes: string | null }>({
    versions: null,
    changes: null,
  });

  private seq = { versions: 0, changes: 0 };
  private stops: Unsubscribe[] = [];

  constructor(api: Api, toasts: Toasts = new Toasts()) {
    this.api = api;
    this.toasts = toasts;
  }

  // ---- derived ----

  get project(): Project | null {
    return this.projects.find((p) => p.id === this.projectId) ?? null;
  }

  get unsavedCount(): number {
    return this.projectId === null ? 0 : (this.counts[this.projectId] ?? 0);
  }

  /** Sides of the comparison shown in the changes view. */
  get sides(): Sides | null {
    if (this.compare) return this.compare;
    const sel = this.selection;
    if (!sel) return null;
    return sel.kind === 'unsaved'
      ? { from: null, to: { kind: 'workingTree' } }
      : { from: null, to: { kind: 'version', id: sel.id } };
  }

  /** The selected file's change. */
  get change(): FileChange | null {
    return this.changes.find((c) => c.path === this.path) ?? null;
  }

  // ---- lifecycle ----

  /** Subscribe to backend events and load the projects. */
  async start(): Promise<void> {
    this.stops.push(
      this.api.onProjectChanged((e) => {
        this.counts[e.projectId] = e.changedCount;
        if (e.projectId === this.projectId && this.sides?.to.kind === 'workingTree') {
          void this.loadChanges();
        }
      }),
      this.api.onVersionsChanged((e) => {
        if (e.projectId === this.projectId) void this.loadVersions();
        void this.refreshCount(e.projectId);
      }),
      this.api.onProgress((p) => {
        this.progress[p.projectId] = p;
      }),
    );
    await this.loadProjects();
  }

  stop(): void {
    this.stops.forEach((s) => s());
    this.stops = [];
  }

  // ---- projects ----

  async loadProjects(): Promise<void> {
    this.loading.projects = true;
    try {
      this.projects = await this.api.listProjects();
      await Promise.all(this.projects.map((p) => this.refreshCount(p.id)));
      if (this.projectId !== null && !this.project) this.projectId = null;
    } catch (err) {
      this.toasts.error('Could not load projects', err);
    } finally {
      this.loading.projects = false;
    }
  }

  async refreshCount(id: ProjectId): Promise<void> {
    const p = this.projects.find((x) => x.id === id);
    if (!p || p.missing) {
      this.counts[id] = 0;
      return;
    }
    try {
      this.counts[id] = (await this.api.status(id)).length;
    } catch {
      // A transient failure keeps the old badge; the next event retries.
    }
  }

  /** Switch screens, asking the current one first ([`leaveGuard`]). */
  setView(view: View): void {
    if (view === this.view) return;
    if (this.view === 'settings' && this.leaveGuard && !this.leaveGuard(() => this.forceView(view)))
      return;
    this.forceView(view);
  }

  private forceView(view: View): void {
    this.leaveGuard = null;
    this.view = view;
  }

  async selectProject(id: ProjectId): Promise<void> {
    if (this.projectId === id && this.view === 'main') return;
    if (
      this.view === 'settings' &&
      this.leaveGuard &&
      !this.leaveGuard(() => {
        this.forceView('main');
        void this.selectProject(id);
      })
    )
      return;
    this.leaveGuard = null;
    this.view = 'main';
    this.projectId = id;
    this.selection = { kind: 'unsaved' };
    this.compare = null;
    this.path = null;
    this.lastSnapshot = null;
    this.versions = [];
    this.changes = [];
    await Promise.all([this.loadVersions(), this.loadChanges()]);
  }

  /** Pick a folder and track it. */
  async addProject(): Promise<void> {
    try {
      const path = await this.api.pickFolder();
      if (path === null) return;
      const p = await this.api.addProject(path);
      await this.loadProjects();
      await this.selectProject(p.id);
      this.toasts.push('success', `Tracking ${p.name}.`);
    } catch (err) {
      this.toasts.error('Could not add the folder', err);
    }
  }

  async renameProject(id: ProjectId, name: string): Promise<boolean> {
    try {
      const p = await this.api.renameProject(id, name);
      this.projects = this.projects
        .map((x) => (x.id === id ? p : x))
        .sort((a, b) => a.name.localeCompare(b.name));
      return true;
    } catch (err) {
      this.toasts.error('Could not rename', err);
      return false;
    }
  }

  async removeProject(id: ProjectId, deleteSnapshots: boolean): Promise<void> {
    try {
      await this.api.removeProject(id, deleteSnapshots);
      if (this.projectId === id) {
        this.projectId = null;
        this.selection = null;
        this.versions = [];
        this.changes = [];
      }
      await this.loadProjects();
    } catch (err) {
      this.toasts.error('Could not remove the project', err);
    }
  }

  async revealProject(id: ProjectId): Promise<void> {
    const p = this.projects.find((x) => x.id === id);
    if (!p) return;
    try {
      await this.api.revealInExplorer(p.root);
    } catch (err) {
      this.toasts.error('Could not open the folder', err);
    }
  }

  /** "Locate folder" for a missing project. */
  async relocateProject(id: ProjectId): Promise<void> {
    try {
      const path = await this.api.pickFolder();
      if (path === null) return;
      await this.api.relocateProject(id, path);
      await this.loadProjects();
      this.projectId = null;
      await this.selectProject(id);
    } catch (err) {
      this.toasts.error('Could not use that folder', err);
    }
  }

  // ---- versions and changes ----

  async loadVersions(): Promise<void> {
    const id = this.projectId;
    if (id === null) return;
    const seq = ++this.seq.versions;
    this.loading.versions = true;
    this.errors.versions = null;
    try {
      const v = await this.api.listVersions(id);
      if (seq !== this.seq.versions) return;
      this.versions = v;
      const sel = this.selection;
      if (sel?.kind === 'version' && !v.some((x) => x.id === sel.id)) {
        await this.select({ kind: 'unsaved' });
      }
    } catch (err) {
      if (seq === this.seq.versions) this.errors.versions = message(err);
    } finally {
      if (seq === this.seq.versions) this.loading.versions = false;
    }
  }

  async select(sel: Selection): Promise<void> {
    this.selection = sel;
    this.compare = null;
    this.path = null;
    await this.loadChanges();
  }

  /** Compare any two sides (F16). */
  async setCompare(sides: Sides | null): Promise<void> {
    this.compare = sides;
    this.path = null;
    await this.loadChanges();
  }

  async loadChanges(): Promise<void> {
    const id = this.projectId;
    const sides = this.sides;
    if (id === null || !sides) return;
    const seq = ++this.seq.changes;
    this.loading.changes = true;
    this.errors.changes = null;
    try {
      const unsaved = !this.compare && sides.to.kind === 'workingTree';
      const list = unsaved
        ? await this.api.status(id)
        : await this.api.changes(id, sides.from, sides.to);
      if (seq !== this.seq.changes) return;
      this.changes = list;
      if (unsaved) this.counts[id] = list.length;
      if (this.path !== null && !list.some((c) => c.path === this.path)) this.path = null;
    } catch (err) {
      if (seq === this.seq.changes) {
        this.changes = [];
        this.errors.changes = message(err);
      }
    } finally {
      if (seq === this.seq.changes) this.loading.changes = false;
    }
  }

  selectFile(path: RelPath | null): void {
    this.path = path;
  }

  // ---- snapshot and restore ----

  /** Snapshot the selected project. Returns the report, or `null` on failure. */
  async snapshot(label: string): Promise<SnapshotReport | null> {
    const id = this.projectId;
    if (id === null || this.snapshotting) return null;
    this.snapshotting = true;
    try {
      const report = await this.api.snapshot(id, label.trim() || undefined);
      if (id !== this.projectId) return report;
      this.lastSnapshot = report;
      if (report.version) {
        this.toasts.push('success', `Saved "${report.version.label}".`);
      } else {
        this.toasts.push('info', 'Nothing changed since the last snapshot.');
      }
      await Promise.all([this.loadVersions(), this.refreshCount(id)]);
      if (this.selection?.kind === 'unsaved') await this.loadChanges();
      return report;
    } catch (err) {
      const e = ApiError.from(err);
      if (e.code !== 'cancelled') this.toasts.error('Snapshot failed', e);
      return null;
    } finally {
      this.snapshotting = false;
    }
  }

  async restorePlan(versionId: VersionId): Promise<RestorePlan | null> {
    if (this.projectId === null) return null;
    try {
      return await this.api.restorePlan(this.projectId, versionId);
    } catch (err) {
      this.toasts.error('Could not plan the restore', err);
      return null;
    }
  }

  /** Restore the project; then select the new safety version (the undo point). */
  async restoreProject(versionId: VersionId): Promise<RestoreReport | null> {
    const id = this.projectId;
    if (id === null) return null;
    try {
      const r = await this.api.restoreProject(id, versionId);
      await this.afterRestore(id, r, 'Restored the project');
      return r;
    } catch (err) {
      this.toasts.error('Restore failed', err);
      await this.loadVersions();
      return null;
    }
  }

  async restoreFile(versionId: VersionId, path: RelPath): Promise<RestoreReport | null> {
    const id = this.projectId;
    if (id === null) return null;
    try {
      const r = await this.api.restoreFile(id, versionId, path);
      await this.afterRestore(id, r, `Restored ${path}`);
      return r;
    } catch (err) {
      this.toasts.error('Restore failed', err);
      return null;
    }
  }

  private async afterRestore(id: ProjectId, r: RestoreReport, done: string): Promise<void> {
    const undo = { label: 'Undo', run: () => void this.restoreProject(r.safetyVersion) };
    if (r.failed.length > 0) {
      this.restoreResult = r;
    } else if (r.uncaptured.length > 0) {
      this.toasts.push(
        'warning',
        `${done}. Left alone (not in any snapshot): ${r.uncaptured.join(', ')}.`,
        undo,
      );
    } else {
      this.toasts.push('success', `${done}.`, undo);
    }
    if (id !== this.projectId) return;
    await this.loadVersions();
    await this.refreshCount(id);
    if (this.selection?.kind === 'unsaved') await this.loadChanges();
  }
}
