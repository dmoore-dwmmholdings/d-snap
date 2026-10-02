import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from './api';
import { COMMANDS, EVENTS } from './commands';
import { DEFAULT_DIFF_OPTIONS } from './types';

const invoke = vi.fn();
const unlisten = vi.fn();
const listen = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: (...a: unknown[]) => listen(...a) }));

const { TauriApi } = await import('./real');

const WT = { kind: 'workingTree' } as const;

beforeEach(() => {
  invoke.mockReset().mockResolvedValue('ok');
  unlisten.mockReset();
  listen.mockReset().mockResolvedValue(unlisten);
});

describe('TauriApi', () => {
  const api = new TauriApi();

  // Each method: the command and the argument object the Rust side expects.
  const cases: [string, () => Promise<unknown>, string, Record<string, unknown> | undefined][] = [
    ['listProjects', () => api.listProjects(), COMMANDS.listProjects, undefined],
    ['addProject', () => api.addProject('C:/p'), COMMANDS.addProject, { path: 'C:/p' }],
    [
      'renameProject',
      () => api.renameProject(1, 'n'),
      COMMANDS.renameProject,
      { id: 1, name: 'n' },
    ],
    [
      'removeProject',
      () => api.removeProject(1, true),
      COMMANDS.removeProject,
      { id: 1, deleteSnapshots: true },
    ],
    [
      'relocateProject',
      () => api.relocateProject(1, 'D:/p'),
      COMMANDS.relocateProject,
      { id: 1, path: 'D:/p' },
    ],
    [
      'revealInExplorer',
      () => api.revealInExplorer('C:/p'),
      COMMANDS.revealInExplorer,
      { path: 'C:/p' },
    ],
    ['pickFolder', () => api.pickFolder(), COMMANDS.pickFolder, undefined],
    ['getDataDir', () => api.getDataDir(), COMMANDS.getDataDir, undefined],
    ['getProjectSettings', () => api.getProjectSettings(2), COMMANDS.getProjectSettings, { id: 2 }],
    ['getGlobalSettings', () => api.getGlobalSettings(), COMMANDS.getGlobalSettings, undefined],
    ['snapshot', () => api.snapshot(3), COMMANDS.snapshot, { projectId: 3, label: null }],
    [
      'snapshot labelled',
      () => api.snapshot(3, 'x'),
      COMMANDS.snapshot,
      { projectId: 3, label: 'x' },
    ],
    ['status', () => api.status(3), COMMANDS.status, { projectId: 3 }],
    [
      'cancelOperation',
      () => api.cancelOperation('op-1'),
      COMMANDS.cancelOperation,
      { opId: 'op-1' },
    ],
    ['listVersions', () => api.listVersions(3), COMMANDS.listVersions, { projectId: 3 }],
    ['setLabel', () => api.setLabel(4, 'l'), COMMANDS.setLabel, { versionId: 4, label: 'l' }],
    ['setPinned', () => api.setPinned(4, true), COMMANDS.setPinned, { versionId: 4, pinned: true }],
    ['deleteVersion', () => api.deleteVersion(4), COMMANDS.deleteVersion, { versionId: 4 }],
    [
      'changes',
      () => api.changes(3, null, WT),
      COMMANDS.changes,
      { projectId: 3, from: null, to: WT },
    ],
    [
      'fileDiff',
      () => api.fileDiff(3, 4, WT, 'a.txt', DEFAULT_DIFF_OPTIONS),
      COMMANDS.fileDiff,
      { projectId: 3, from: 4, to: WT, path: 'a.txt', opts: DEFAULT_DIFF_OPTIONS },
    ],
    [
      'readBlobAsDataUrl',
      () => api.readBlobAsDataUrl('ab', 'image/png'),
      COMMANDS.readBlobAsDataUrl,
      { hash: 'ab', mime: 'image/png' },
    ],
    [
      'restorePlan',
      () => api.restorePlan(3, 4),
      COMMANDS.restorePlan,
      { projectId: 3, versionId: 4 },
    ],
    [
      'restoreProject',
      () => api.restoreProject(3, 4),
      COMMANDS.restoreProject,
      { projectId: 3, versionId: 4 },
    ],
    [
      'restoreFile',
      () => api.restoreFile(3, 4, 'a.txt'),
      COMMANDS.restoreFile,
      { projectId: 3, versionId: 4, path: 'a.txt' },
    ],
    [
      'revertHunk',
      () => api.revertHunk(3, null, WT, 'a.txt', 2, DEFAULT_DIFF_OPTIONS),
      COMMANDS.revertHunk,
      { projectId: 3, from: null, to: WT, path: 'a.txt', hunkIndex: 2, opts: DEFAULT_DIFF_OPTIONS },
    ],
  ];

  it.each(cases)('%s invokes its command', async (_name, run, cmd, args) => {
    await expect(run()).resolves.toBe('ok');
    expect(invoke).toHaveBeenCalledWith(cmd, args);
  });

  it('turns backend errors into ApiError with the same code', async () => {
    invoke.mockRejectedValueOnce({ code: 'project_missing', message: 'gone' });
    const err = await api.status(1).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ code: 'project_missing', message: 'gone' });
    invoke.mockRejectedValueOnce('boom');
    await expect(api.status(1)).rejects.toMatchObject({ code: 'internal' });
  });

  it('subscribes to events and unsubscribes, even before listen settles', async () => {
    const seen: unknown[] = [];
    const stop = api.onProgress((p) => seen.push(p));
    expect(listen).toHaveBeenCalledWith(EVENTS.onProgress, expect.any(Function));
    const handler = listen.mock.calls[0]![1] as (e: { payload: unknown }) => void;
    handler({ payload: { opId: 'op-1' } });
    expect(seen).toEqual([{ opId: 'op-1' }]);
    stop(); // before the listen promise resolved
    await Promise.resolve();
    await Promise.resolve();
    expect(unlisten).toHaveBeenCalledTimes(1);

    api.onProjectsChanged(() => {});
    expect(listen).toHaveBeenCalledWith(EVENTS.onProjectsChanged, expect.any(Function));
    const stop2 = api.onVersionsChanged(() => {});
    await Promise.resolve();
    await Promise.resolve();
    stop2();
    expect(unlisten).toHaveBeenCalledTimes(2);
  });
});
