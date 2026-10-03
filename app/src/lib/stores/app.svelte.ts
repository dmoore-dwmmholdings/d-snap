// App state for "Turn review" (DSNA-124): the selected folder, its history (versions and
// agent turns), what the main area shows, and the actions the views call. Views load the
// details they show (turn files, diffs) themselves; the store holds what several views share.
import { getContext, setContext } from 'svelte';
import {
  ApiError,
  type Api,
  type DiffOptions,
  type FileChange,
  type Progress,
  type Project,
  type ProjectId,
  type RelPath,
  type RestorePlan,
  type RestoreReport,
  type SnapshotReport,
  type Turn,
  type TurnFile,
  type TurnId,
  type Unsubscribe,
  type UndoTurnReport,
  type Version,
  type VersionId,
  type VersionRef,
} from '../api';
import {
  buildHistory,
  selectionKey,
  selectionOf,
  waitingTurns,
  type HistoryItem,
  type Selection,
} from '../history';
import { Toasts } from '../toast/toasts.svelte';

export type { Selection } from '../history';

/** Which screen is shown. */
export type View = 'main' | 'settings';

const KEY = Symbol('dsnap-store');

/** Make `store` available to child components. */
export function provideStore(store: AppStore): void {
  setContext(KEY, store);
}

/** Context map holding `store`, for mounting a component on its own (tests). */
export function storeContext(store: AppStore): Map<unknown, unknown> {
  // Svelte reads context once at mount; this map is never mutated.
  // eslint-disable-next-line svelte/prefer-svelte-reactivity
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

const byName = (a: Project, b: Project) =>
  a.name.localeCompare(b.name, undefined, { sensitivity: 'base' });

export class AppStore {
  readonly api: Api;
  readonly toasts: Toasts;

  view = $state<View>('main');
  projects = $state<Project[]>([]);
  /** Files changed since the last snapshot, per folder. */
  counts = $state<Record<ProjectId, number>>({});
  /** Turns waiting for review, per folder (the lime dot on its chip). */
  waiting = $state<Record<ProjectId, number>>({});
  projectId = $state<ProjectId | null>(null);
  versions = $state<Version[]>([]);
  turns = $state<Turn[]>([]);
  /** The folder's unsaved changes (working tree vs latest version). */
  unsaved = $state<FileChange[]>([]);
  selection = $state<Selection | null>(null);
  /** First side picked for "Compare with…" (F16), waiting for the second. */
  comparePick = $state<VersionId | null>(null);
  /** Latest progress per folder (snapshot, restore). */
  progress = $state<Record<ProjectId, Progress>>({});
  /** A snapshot or restore started from this window is running. */
  busy = $state<null | 'snapshot' | 'restore'>(null);
  /** Last snapshot's skipped and unstable files, for the warning panel. */
  lastSnapshot = $state<SnapshotReport | null>(null);
  loading = $state({ projects: true, history: false });
  errors = $state<{ history: string | null }>({ history: null });
  /** A restore that could not write every file (RestoreResultDialog). */
  restoreResult = $state<RestoreReport | null>(null);
  /** The "Take a snapshot" dialog is open. */
  snapshotDialog = $state(false);
  /** A whole-folder restore waiting for confirmation, with its plan once loaded. */
  restoreAsk = $state<{ version: Version; plan: RestorePlan | null } | null>(null);
  /** Bumped after every write, so open views reload what they show. */
  revision = $state(0);
  /**
   * Set by a screen with unsaved edits (Settings). Called before navigating away with the
   * navigation to run later; returning `false` stops it for now.
   */
  leaveGuard: ((proceed: () => void) => boolean) | null = null;

  private seq = 0;
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

  /** History rows, newest first. */
  get history(): HistoryItem[] {
    return buildHistory(this.versions, this.turns);
  }

  get selectedKey(): string | null {
    return this.selection ? selectionKey(this.selection) : null;
  }

  version(id: VersionId | null): Version | null {
    return id === null ? null : (this.versions.find((v) => v.id === id) ?? null);
  }

  turn(id: TurnId): Turn | null {
    return this.turns.find((t) => t.id === id) ?? null;
  }

  /** The version just before `id` (what a version's changes compare with). */
  previousVersion(id: VersionId): Version | null {
    const i = this.versions.findIndex((v) => v.id === id);
    return i >= 0 ? (this.versions[i + 1] ?? null) : null;
  }

  // ---- lifecycle ----

  async start(): Promise<void> {
    this.stops.push(
      this.api.onProjectChanged((e) => {
        this.counts[e.projectId] = e.changedCount;
        if (e.projectId === this.projectId) void this.loadUnsaved();
      }),
      this.api.onVersionsChanged((e) => {
        if (e.projectId === this.projectId) void this.loadHistory();
        else void this.refreshWaiting(e.projectId);
        void this.refreshCount(e.projectId);
      }),
      this.api.onProgress((p) => {
        this.progress[p.projectId] = p;
      }),
      this.api.onProjectsChanged(() => void this.loadProjects()),
    );
    await this.loadProjects();
    const first = this.projects.find((p) => (this.waiting[p.id] ?? 0) > 0) ?? this.projects[0];
    if (first && this.projectId === null) await this.selectProject(first.id);
  }

  /** Re-read everything the watchers report (on window focus, in case one missed). */
  async refreshAll(): Promise<void> {
    await this.loadProjects();
    if (this.projectId !== null && this.view === 'main') {
      await Promise.all([this.loadHistory(), this.loadUnsaved()]);
    }
  }

  stop(): void {
    this.stops.forEach((s) => s());
    this.stops = [];
  }

  // ---- folders ----

  async loadProjects(): Promise<void> {
    this.loading.projects = true;
    try {
      this.projects = (await this.api.listProjects()).sort(byName);
      await Promise.all(
        this.projects.flatMap((p) => [this.refreshCount(p.id), this.refreshWaiting(p.id)]),
      );
      if (this.projectId !== null && !this.project) {
        this.projectId = null;
        this.selection = null;
      }
    } catch (err) {
      this.toasts.error('Could not load your folders', err);
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
      // A transient failure keeps the old count; the next event retries.
    }
  }

  async refreshWaiting(id: ProjectId): Promise<void> {
    try {
      this.waiting[id] = waitingTurns(await this.api.listTurns(id)).length;
    } catch {
      // Keep the old dot.
    }
  }

  /** Switch screens, asking the current one first (`leaveGuard`). */
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
    this.selection = null;
    this.comparePick = null;
    this.lastSnapshot = null;
    this.versions = [];
    this.turns = [];
    this.unsaved = [];
    await Promise.all([this.loadHistory(), this.loadUnsaved()]);
    if (this.projectId === id && this.selection === null) this.selection = this.defaultSelection();
  }

  /** What a folder opens on: the newest turn waiting for review, else the unsaved changes,
   * else the newest History row. */
  defaultSelection(): Selection | null {
    const waiting = waitingTurns(this.turns)[0];
    if (waiting) return { kind: 'turn', id: waiting.id };
    if (this.unsaved.length > 0) return { kind: 'unsaved' };
    const first = this.history[0];
    return first ? selectionOf(first) : null;
  }

  async addProject(): Promise<void> {
    try {
      const path = await this.api.pickFolder();
      if (path === null) return;
      const p = await this.api.addProject(path);
      await this.loadProjects();
      await this.selectProject(p.id);
      this.toasts.push('success', `Watching ${p.name}.`);
    } catch (err) {
      this.toasts.error('Could not add the folder', err);
    }
  }

  async renameProject(id: ProjectId, name: string): Promise<boolean> {
    try {
      const p = await this.api.renameProject(id, name);
      this.projects = this.projects.map((x) => (x.id === id ? p : x)).sort(byName);
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
        this.turns = [];
        this.unsaved = [];
      }
      await this.loadProjects();
      const next = this.projects[0];
      if (this.projectId === null && next) await this.selectProject(next.id);
    } catch (err) {
      this.toasts.error('Could not remove the folder', err);
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

  /** "Locate folder…" for a missing folder. */
  async relocateProject(id: ProjectId): Promise<boolean> {
    try {
      const path = await this.api.pickFolder();
      if (path === null) return false;
      const p = await this.api.relocateProject(id, path);
      await this.loadProjects();
      if (this.projectId === id) await Promise.all([this.loadHistory(), this.loadUnsaved()]);
      this.toasts.push('success', `Found ${p.name}. Watching it again.`);
      return true;
    } catch (err) {
      this.toasts.error('Could not use that folder', err);
      return false;
    }
  }

  // ---- history ----

  async loadHistory(): Promise<void> {
    const id = this.projectId;
    if (id === null) return;
    const seq = ++this.seq;
    this.loading.history = true;
    this.errors.history = null;
    try {
      const [versions, turns] = await Promise.all([
        this.api.listVersions(id),
        this.api.listTurns(id),
      ]);
      if (seq !== this.seq || id !== this.projectId) return;
      this.versions = versions;
      this.turns = turns;
      this.waiting[id] = waitingTurns(turns).length;
      this.revision++;
      if (this.selection && !this.selectionExists(this.selection)) {
        this.selection = this.defaultSelection();
      }
    } catch (err) {
      if (seq === this.seq) this.errors.history = message(err);
    } finally {
      if (seq === this.seq) this.loading.history = false;
    }
  }

  private selectionExists(s: Selection): boolean {
    switch (s.kind) {
      case 'turn':
        return this.turns.some((t) => t.id === s.id);
      case 'version':
        return this.versions.some((v) => v.id === s.id);
      case 'edits':
        return this.versions.some((v) => v.id === s.to);
      case 'autos':
        return s.ids.some((id) => this.versions.some((v) => v.id === id));
      case 'compare':
        return this.versions.some((v) => v.id === s.from);
      default:
        return true;
    }
  }

  async loadUnsaved(): Promise<void> {
    const id = this.projectId;
    const p = this.project;
    if (id === null || !p) return;
    if (p.missing) {
      this.unsaved = [];
      return;
    }
    try {
      const list = await this.api.status(id);
      if (id !== this.projectId) return;
      this.unsaved = list;
      this.counts[id] = list.length;
    } catch {
      // The view shows what it can; the next event retries.
    }
  }

  select(sel: Selection): void {
    if (this.view !== 'main') this.setView('main');
    this.selection = sel;
  }

  // ---- compare (F16) ----

  /** Start "Compare with…" from `id`; the next History pick completes it. */
  startCompare(id: VersionId): void {
    this.comparePick = id;
    this.toasts.push('info', 'Pick another entry in History to compare with.');
  }

  cancelCompare(): void {
    this.comparePick = null;
  }

  /** Complete a compare with `other` (`null` = the folder now). Older side first. */
  finishCompare(other: VersionId | null): void {
    const a = this.comparePick;
    this.comparePick = null;
    if (a === null || a === other) return;
    if (other === null) {
      this.select({ kind: 'compare', from: a, to: null });
      return;
    }
    const [from, to] = a < other ? [a, other] : [other, a];
    this.select({ kind: 'compare', from, to });
  }

  /** The version a History row compares as (its newest snapshot). */
  versionOfItem(item: HistoryItem): VersionId | null {
    switch (item.kind) {
      case 'turn':
        return item.turn.after;
      case 'edits':
        return item.to.id;
      case 'autos':
        return item.versions[0]?.id ?? null;
      case 'version':
        return item.version.id;
    }
  }

  // ---- snapshot and restore ----

  async snapshot(label: string): Promise<SnapshotReport | null> {
    const id = this.projectId;
    if (id === null || this.busy) return null;
    this.busy = 'snapshot';
    try {
      const report = await this.api.snapshot(id, label.trim() || undefined);
      this.lastSnapshot =
        report.skipped.length > 0 || report.unstablePaths.length > 0 ? report : null;
      if (report.version) {
        const v = report.version;
        this.toasts.push('success', `Saved “${v.label}”.`, {
          label: 'Delete it',
          run: () => void this.deleteVersion(v.id),
        });
      } else {
        this.toasts.push('info', 'Nothing changed since the last snapshot, so none was saved.');
      }
      await Promise.all([this.loadHistory(), this.loadUnsaved()]);
      if (report.version && this.projectId === id) {
        this.select({ kind: 'version', id: report.version.id });
      }
      return report;
    } catch (err) {
      const e = ApiError.from(err);
      if (e.code !== 'cancelled') this.toasts.error('Snapshot failed', e);
      return null;
    } finally {
      this.busy = null;
    }
  }

  /** Ask before restoring the folder to `versionId` (loads the plan for the dialog). */
  async askRestore(versionId: VersionId): Promise<void> {
    const version = this.version(versionId);
    if (!version) return;
    this.restoreAsk = { version, plan: null };
    const plan = await this.restorePlan(versionId);
    if (this.restoreAsk?.version.id !== versionId) return;
    if (plan) this.restoreAsk = { version, plan };
    else this.restoreAsk = null;
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

  async restoreProject(versionId: VersionId, label?: string): Promise<RestoreReport | null> {
    return this.write(
      'restore',
      async (id) => {
        const r = await this.api.restoreProject(id, versionId);
        const name = label ?? this.version(versionId)?.label ?? 'that snapshot';
        this.afterRestore(r, `${this.project?.name ?? 'Folder'} restored to “${name}”`, () =>
          this.restoreProject(r.safetyVersion, 'before the restore'),
        );
        return r;
      },
      'Restore failed',
    );
  }

  async restoreFile(versionId: VersionId, path: RelPath): Promise<RestoreReport | null> {
    return this.write(
      'restore',
      async (id) => {
        const r = await this.api.restoreFile(id, versionId, path);
        this.afterRestore(r, `${path} brought back`, () => this.restoreFile(r.safetyVersion, path));
        return r;
      },
      'Could not bring the file back',
    );
  }

  /** Revert one hunk of an unsaved change (F22). */
  async revertHunk(
    from: VersionId | null,
    to: VersionRef,
    path: RelPath,
    hunkIndex: number,
    opts: DiffOptions,
  ): Promise<boolean> {
    const r = await this.write(
      'restore',
      async (id) => {
        const r = await this.api.revertHunk(id, from, to, path, hunkIndex, opts);
        this.afterRestore(r, `That part of ${path} is undone`, () =>
          this.restoreFile(r.safetyVersion, path),
        );
        return r;
      },
      'Could not undo that part',
    );
    return (r?.written.length ?? 0) > 0;
  }

  // ---- turns ----

  /** Record a decision on one file (or clear it with `null`). */
  async decide(turnId: TurnId, path: RelPath, decision: 'kept' | null): Promise<boolean> {
    try {
      const t = await this.api.setTurnDecisions(turnId, [{ path, decision }]);
      this.patchTurn(t);
      return true;
    } catch (err) {
      this.toasts.error('Could not save that decision', err);
      return false;
    }
  }

  async keepTurn(turnId: TurnId): Promise<Turn | null> {
    try {
      const t = await this.api.keepTurn(turnId);
      this.patchTurn(t);
      this.toasts.push('success', `Turn ${t.seq} kept. Nothing left to review.`);
      return t;
    } catch (err) {
      this.toasts.error('Could not keep the turn', err);
      return null;
    }
  }

  /**
   * Undo files of a turn (`paths: null` = all). Files changed since the turn are skipped
   * unless `includeChanged`. The toast offers "Put it back" (redo of what was undone).
   */
  async undoTurn(
    turnId: TurnId,
    paths: RelPath[] | null,
    includeChanged: boolean,
    wholeTurn = false,
  ): Promise<UndoTurnReport | null> {
    return this.write(
      'restore',
      async (id) => {
        const r = await this.api.undoTurn(id, turnId, { paths, includeChanged });
        this.patchTurn(r.turn);
        const redoPaths = paths ? paths.filter((p) => !r.skippedChanged.includes(p)) : null;
        const one = !wholeTurn && paths?.length === 1 ? paths[0] : undefined;
        let text = one
          ? `${one} is back as it was before Turn ${r.turn.seq}. A safety copy was saved.`
          : `Turn ${r.turn.seq} is undone. A safety copy was saved.`;
        if (r.skippedChanged.length > 0) {
          text += ` Left alone, because you changed them since: ${r.skippedChanged.join(', ')}.`;
        }
        if (r.restore.failed.length > 0) this.restoreResult = r.restore;
        else
          this.toasts.push(r.skippedChanged.length ? 'warning' : 'success', text, {
            label: 'Put it back',
            run: () => void this.redoTurn(turnId, redoPaths),
          });
        return r;
      },
      'Could not undo',
    );
  }

  /** Put undone files back as the turn left them (`null` = every undone file). */
  async redoTurn(turnId: TurnId, paths: RelPath[] | null): Promise<UndoTurnReport | null> {
    return this.write(
      'restore',
      async (id) => {
        const r = await this.api.redoTurn(id, turnId, paths);
        this.patchTurn(r.turn);
        this.toasts.push('success', `Put back. Turn ${r.turn.seq} is as the agent left it.`);
        return r;
      },
      'Could not put it back',
    );
  }

  /** Undo one hunk of a turn's change to `path`. */
  async undoTurnHunk(
    turnId: TurnId,
    path: RelPath,
    hunkIndex: number,
    opts: DiffOptions,
  ): Promise<boolean> {
    const r = await this.write(
      'restore',
      async (id) => {
        const r = await this.api.undoTurnHunk(id, turnId, path, hunkIndex, opts);
        this.afterRestore(r, `That part of ${path} is undone`, () =>
          this.restoreFile(r.safetyVersion, path),
        );
        return r;
      },
      'Could not undo that part',
    );
    return (r?.written.length ?? 0) > 0;
  }

  async turnFiles(turnId: TurnId): Promise<TurnFile[]> {
    return this.api.turnFiles(turnId);
  }

  private patchTurn(t: Turn): void {
    this.turns = this.turns.map((x) => (x.id === t.id ? t : x));
    this.waiting[t.projectId] = waitingTurns(this.turns).length;
    this.revision++;
  }

  // ---- version management (F11) ----

  async setLabel(id: VersionId, label: string): Promise<void> {
    const old = this.version(id);
    const next = label.trim();
    if (!old || !next || next === old.label) return;
    this.patchVersion({ ...old, label: next });
    try {
      this.patchVersion(await this.api.setLabel(id, next));
    } catch (err) {
      this.patchVersion(old);
      this.toasts.error('Could not rename the snapshot', err);
    }
  }

  async setPinned(id: VersionId, pinned: boolean): Promise<void> {
    const old = this.version(id);
    if (!old) return;
    this.patchVersion({ ...old, pinned });
    try {
      this.patchVersion(await this.api.setPinned(id, pinned));
    } catch (err) {
      this.patchVersion(old);
      this.toasts.error(pinned ? 'Could not pin' : 'Could not unpin', err);
    }
  }

  async deleteVersion(id: VersionId): Promise<void> {
    const before = this.versions;
    this.versions = before.filter((v) => v.id !== id);
    if (this.selection?.kind === 'version' && this.selection.id === id) {
      this.selection = this.defaultSelection();
    }
    try {
      await this.api.deleteVersion(id);
      await this.loadHistory();
    } catch (err) {
      this.versions = before;
      this.toasts.error('Could not delete the snapshot', err);
    }
  }

  private patchVersion(v: Version): void {
    this.versions = this.versions.map((x) => (x.id === v.id ? v : x));
  }

  // ---- helpers ----

  /** Run a write on the current folder with the busy flag, then reload what it shows. */
  private async write<T>(
    kind: 'snapshot' | 'restore',
    run: (id: ProjectId) => Promise<T>,
    failure: string,
  ): Promise<T | null> {
    const id = this.projectId;
    if (id === null) return null;
    this.busy = kind;
    try {
      return await run(id);
    } catch (err) {
      const e = ApiError.from(err);
      if (e.code !== 'cancelled') this.toasts.error(failure, e);
      return null;
    } finally {
      this.busy = null;
      if (this.projectId === id) await Promise.all([this.loadHistory(), this.loadUnsaved()]);
      this.revision++;
    }
  }

  private afterRestore(r: RestoreReport, done: string, undo: () => Promise<unknown>): void {
    const action = { label: 'Undo', run: () => void undo() };
    if (r.failed.length > 0) {
      this.restoreResult = r;
    } else if (r.uncaptured.length > 0) {
      this.toasts.push(
        'warning',
        `${done}. Left alone (not in any snapshot): ${r.uncaptured.join(', ')}.`,
        action,
      );
    } else {
      this.toasts.push('success', `${done}. A safety copy was saved.`, action);
    }
  }
}
