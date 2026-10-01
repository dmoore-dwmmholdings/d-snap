import { ApiError, type Api } from './api';
import { COMMANDS, EVENTS } from './commands';
import { createMockApi, MockApi } from './mock';
import { DEFAULT_DIFF_OPTIONS, type Project, type Version } from './types';

const WT = { kind: 'workingTree' } as const;
const v = (id: number) => ({ kind: 'version', id }) as const;

async function setup() {
  const api = createMockApi({ now: () => Date.UTC(2026, 9, 1, 12, 0, 0) });
  const projects = await api.listProjects();
  const byName = (n: string): Project => {
    const p = projects.find((x) => x.name === n);
    if (!p) throw new Error(`no project ${n}`);
    return p;
  };
  return { api, shop: byName('web-shop'), tool: byName('rust-tool'), notes: byName('notes') };
}

async function rejectsWith(p: Promise<unknown>, code: string) {
  const err = await p.then(
    () => null,
    (e: unknown) => e,
  );
  expect(err).toBeInstanceOf(ApiError);
  expect((err as ApiError).code).toBe(code);
}

describe('mock seed data', () => {
  it('has 3 projects sorted by name, one missing', async () => {
    const { api } = await setup();
    const projects = await api.listProjects();
    expect(projects.map((p) => p.name)).toEqual(['notes', 'rust-tool', 'web-shop']);
    expect(projects.filter((p) => p.missing).map((p) => p.name)).toEqual(['notes']);
  });

  it('has ~15 versions per project, newest first, with every kind', async () => {
    const { api } = await setup();
    const all: Version[] = [];
    for (const p of await api.listProjects()) {
      const versions = await api.listVersions(p.id);
      expect(versions.length).toBeGreaterThanOrEqual(14);
      const times = versions.map((x) => x.createdAtMs);
      expect([...times].sort((a, b) => b - a)).toEqual(times);
      all.push(...versions);
    }
    expect(new Set(all.map((x) => x.kind))).toEqual(new Set(['manual', 'auto', 'cli', 'safety']));
    expect(all.some((x) => x.pinned)).toBe(true);
    expect(all.some((x) => x.unstable)).toBe(true);
  });

  it('is deterministic for a seed', async () => {
    const a = await createMockApi({ seed: 7 }).listVersions(1);
    const b = await createMockApi({ seed: 7 }).listVersions(1);
    expect(a).toEqual(b);
  });

  it('includes text, binary, image, renamed, deleted and too-large changes', async () => {
    const { api, shop } = await setup();
    const versions = (await api.listVersions(shop.id)).reverse();
    const kinds = new Set<string>();
    const bodies = new Set<string>();
    for (let i = 1; i < versions.length; i++) {
      const from = versions[i - 1]!.id;
      const to = v(versions[i]!.id);
      for (const c of await api.changes(shop.id, from, to)) {
        kinds.add(c.status.kind);
        const d = await api.fileDiff(shop.id, from, to, c.path, DEFAULT_DIFF_OPTIONS);
        bodies.add(d.body.kind);
      }
    }
    expect(kinds).toEqual(new Set(['added', 'modified', 'deleted', 'renamed']));
    expect(bodies).toEqual(new Set(['text', 'binary', 'image', 'tooLarge']));
  });

  it('has a 5k-line diff', async () => {
    const { api, shop } = await setup();
    const versions = (await api.listVersions(shop.id)).reverse();
    let lines = 0;
    for (let i = 1; i < versions.length; i++) {
      const from = versions[i - 1]!.id;
      const to = v(versions[i]!.id);
      const changes = await api.changes(shop.id, from, to);
      const c = changes.find((x) => x.path === 'src/generated/schema.ts');
      if (c?.status.kind !== 'added') continue;
      expect(c.linesAdded).toBe(5000);
      const d = await api.fileDiff(shop.id, from, to, c.path, DEFAULT_DIFF_OPTIONS);
      if (d.body.kind === 'text') lines = d.body.hunks[0]!.lines.length;
    }
    expect(lines).toBe(5000);
  });
});

describe('status and snapshot', () => {
  it('reports unsaved changes with line counts and skips oversized files', async () => {
    const { api, shop } = await setup();
    const status = await api.status(shop.id);
    expect(status.map((c) => [c.path, c.status.kind])).toEqual([
      ['src/cart.ts', 'modified'],
      ['src/config.ts', 'added'],
      ['src/payment.ts', 'deleted'],
    ]);
    expect(status[1]!.linesAdded).toBe(1);
    expect(status.every((c) => c.path !== 'videos/demo.mp4')).toBe(true);
  });

  it('snapshot adds a version, reports skipped files, then reports nothing changed', async () => {
    const { api, shop } = await setup();
    const before = await api.listVersions(shop.id);
    const versionEvents: number[] = [];
    api.onVersionsChanged((e) => versionEvents.push(e.projectId));

    const report = await api.snapshot(shop.id, '  my label ');
    expect(report.version).toMatchObject({
      label: 'my label',
      kind: 'manual',
      projectId: shop.id,
      counts: { added: 1, modified: 1, deleted: 1 },
    });
    expect(report.skipped).toEqual([
      { path: 'videos/demo.mp4', reason: { kind: 'tooLarge', size: 80 * 1024 * 1024 } },
    ]);
    const after = await api.listVersions(shop.id);
    expect(after.length).toBe(before.length + 1);
    expect(after[0]!.id).toBe(report.version!.id);
    expect(await api.status(shop.id)).toEqual([]);
    expect(versionEvents).toContain(shop.id);

    const again = await api.snapshot(shop.id);
    expect(again.version).toBeNull();
  });

  it('uses a timestamp as the default label', async () => {
    const { api, shop } = await setup();
    const report = await api.snapshot(shop.id);
    expect(report.version!.label).toMatch(/^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d$/);
  });

  it('emits progress events and supports cancel', async () => {
    const { api, shop } = await setup();
    const phases: string[] = [];
    let opId = '';
    api.onProgress((p) => {
      phases.push(p.phase);
      opId = p.opId;
    });
    await api.snapshot(shop.id);
    expect(phases).toEqual(['walk', 'hash', 'done']);

    const pending = api.snapshot(shop.id);
    expect(opId).toBe('op-2'); // the walk event fires before the first await
    await api.cancelOperation(opId);
    await rejectsWith(pending, 'cancelled');
    await rejectsWith(api.cancelOperation('op-999'), 'not_found');
  });

  it('rejects snapshot and status on a missing project', async () => {
    const { api, notes } = await setup();
    await rejectsWith(api.snapshot(notes.id), 'project_missing');
    await rejectsWith(api.status(notes.id), 'project_missing');
    expect((await api.listVersions(notes.id)).length).toBeGreaterThan(0);
  });

  it('mock edits emit onProjectChanged with the change count', async () => {
    const { api, tool } = await setup();
    const events: number[] = [];
    const off = api.onProjectChanged((e) => events.push(e.changedCount));
    api.mockWriteFile(tool.id, 'src/new.rs', 'fn x() {}\n');
    api.mockWriteFile(tool.id, 'src/main.rs', null);
    off();
    api.mockWriteFile(tool.id, 'src/other.rs', '\n');
    expect(events).toEqual([1, 2]);
  });

  it('mock CLI snapshot shows up as a cli version', async () => {
    const { api, tool } = await setup();
    api.mockWriteFile(tool.id, 'src/new.rs', 'fn x() {}\n');
    const version = api.mockCliSnapshot(tool.id);
    expect(version?.kind).toBe('cli');
    expect((await api.listVersions(tool.id))[0]!.id).toBe(version!.id);
  });
});

describe('changes and diffs', () => {
  it('compares a version with nothing (all added)', async () => {
    const { api, tool } = await setup();
    const first = (await api.listVersions(tool.id)).at(-1)!;
    const changes = await api.changes(tool.id, null, v(first.id));
    expect(changes.every((c) => c.status.kind === 'added')).toBe(true);
    expect(changes.length).toBe(first.counts.added);
  });

  it('compares any two versions', async () => {
    const { api, tool } = await setup();
    const versions = await api.listVersions(tool.id);
    const changes = await api.changes(tool.id, versions.at(-1)!.id, v(versions[0]!.id));
    expect(changes.length).toBeGreaterThan(0);
  });

  it('honors context and ignoreWhitespace', async () => {
    const { api, tool } = await setup();
    const text = api.mockReadFile(tool.id, 'src/main.rs')!;
    api.mockWriteFile(tool.id, 'src/main.rs', text.replace(/\n/g, '  \r\n'));
    const latest = (await api.listVersions(tool.id))[0]!.id;
    const strict = await api.fileDiff(tool.id, latest, WT, 'src/main.rs', DEFAULT_DIFF_OPTIONS);
    expect(strict.body.kind === 'text' && strict.body.hunks.length).toBe(1);
    const loose = await api.fileDiff(tool.id, latest, WT, 'src/main.rs', {
      ignoreWhitespace: true,
      context: 0,
    });
    expect(loose.body).toEqual({ kind: 'text', hunks: [] });
  });

  it('reads an image blob as a data URL', async () => {
    const { api, shop } = await setup();
    const versions = await api.listVersions(shop.id);
    const changes = await api.changes(shop.id, null, v(versions[0]!.id));
    const logo = changes.find((c) => c.path === 'assets/logo.svg')!;
    const url = await api.readBlobAsDataUrl(logo.new!.blob!, 'image/svg+xml');
    expect(url.startsWith('data:image/svg+xml;base64,')).toBe(true);
    expect(atob(url.split(',')[1]!)).toContain('<svg');
    await rejectsWith(api.readBlobAsDataUrl('00', 'image/png'), 'not_found');
  });

  it('rejects unknown projects, versions and paths with not_found', async () => {
    const { api, shop, tool } = await setup();
    const toolVersion = (await api.listVersions(tool.id))[0]!.id;
    await rejectsWith(api.listVersions(999), 'not_found');
    await rejectsWith(api.changes(shop.id, toolVersion, WT), 'not_found');
    await rejectsWith(
      api.fileDiff(shop.id, null, WT, 'no/such/file', DEFAULT_DIFF_OPTIONS),
      'not_found',
    );
  });
});

describe('restore', () => {
  it('plans and restores a whole project behind a safety version', async () => {
    const { api, shop } = await setup();
    const versions = await api.listVersions(shop.id);
    const first = versions.at(-1)!;
    const plan = await api.restorePlan(shop.id, first.id);
    expect(plan.target).toBe(first.id);
    expect(plan.write).toContain('docs/old-notes.md');
    expect(plan.delete).toContain('src/generated/schema.ts');
    expect(plan.createDirs).toEqual(['logs/archive']);

    const report = await api.restoreProject(shop.id, first.id);
    expect(report.written).toEqual(plan.write);
    expect(report.deleted).toEqual(plan.delete);
    expect(report.failed).toEqual([]);

    const after = await api.listVersions(shop.id);
    expect(after.length).toBe(versions.length + 1);
    expect(after[0]).toMatchObject({
      id: report.safetyVersion,
      kind: 'safety',
      label: 'Before restore to Initial import',
    });
    // The folder now matches the first version.
    expect(await api.changes(shop.id, first.id, WT)).toEqual([]);
    // The safety version holds the pre-restore state, so the restore is undoable.
    await api.restoreProject(shop.id, report.safetyVersion);
    expect(api.mockReadFile(shop.id, 'src/config.ts')).toContain('API_URL');
  });

  it('restores a single deleted file', async () => {
    const { api, shop } = await setup();
    const latest = (await api.listVersions(shop.id))[0]!;
    expect(api.mockReadFile(shop.id, 'src/payment.ts')).toBeNull();
    const report = await api.restoreFile(shop.id, latest.id, 'src/payment.ts');
    expect(report.written).toEqual(['src/payment.ts']);
    expect(api.mockReadFile(shop.id, 'src/payment.ts')).toContain('payment0');
    await rejectsWith(api.restoreFile(shop.id, latest.id, 'nope.txt'), 'not_found');
  });

  it('reverts a single hunk in the working tree', async () => {
    const { api, tool } = await setup();
    const latest = (await api.listVersions(tool.id))[0]!.id;
    const original = api.mockReadFile(tool.id, 'src/lib.rs')!;
    const lines = original.split('\n');
    lines[1] = '// change one';
    lines[lines.length - 3] = '// change two';
    api.mockWriteFile(tool.id, 'src/lib.rs', lines.join('\n'));

    const before = await api.fileDiff(tool.id, latest, WT, 'src/lib.rs', DEFAULT_DIFF_OPTIONS);
    expect(before.body.kind === 'text' && before.body.hunks.length).toBe(2);

    const report = await api.revertHunk(tool.id, latest, WT, 'src/lib.rs', 0);
    expect(report.written).toEqual(['src/lib.rs']);
    const text = api.mockReadFile(tool.id, 'src/lib.rs')!;
    expect(text).not.toContain('// change one');
    expect(text).toContain('// change two');

    await rejectsWith(api.revertHunk(tool.id, latest, v(latest), 'src/lib.rs', 0), 'invalid_input');
    await rejectsWith(api.revertHunk(tool.id, latest, WT, 'src/lib.rs', 5), 'invalid_input');
  });

  it('refuses to restore a missing project', async () => {
    const { api, notes } = await setup();
    const latest = (await api.listVersions(notes.id))[0]!.id;
    await rejectsWith(api.restoreProject(notes.id, latest), 'project_missing');
  });
});

describe('projects, versions and settings', () => {
  it('adds, renames, relocates and removes projects', async () => {
    const { api, notes } = await setup();
    const path = await api.pickFolder();
    expect(path).toMatch(/new-project-1$/);
    const p = await api.addProject(path!);
    expect(p).toMatchObject({ name: 'new-project-1', missing: false });
    await rejectsWith(api.addProject(path!), 'invalid_input');
    expect((await api.status(p.id)).length).toBe(2);

    expect((await api.renameProject(p.id, 'Renamed')).name).toBe('Renamed');
    await rejectsWith(api.renameProject(p.id, '  '), 'invalid_input');

    const moved = await api.relocateProject(notes.id, 'E:\\notes');
    expect(moved).toMatchObject({ root: 'E:\\notes', missing: false });
    expect(await api.status(notes.id)).toEqual([]);

    await api.removeProject(p.id, true);
    expect((await api.listProjects()).some((x) => x.id === p.id)).toBe(false);
    await rejectsWith(api.removeProject(p.id, false), 'not_found');
  });

  it('pickFolder can be cancelled', async () => {
    const api = createMockApi();
    api.mockSetPickFolderResult(null);
    expect(await api.pickFolder()).toBeNull();
  });

  it('edits labels, pins and deletes versions', async () => {
    const { api, tool } = await setup();
    const [newest, second] = await api.listVersions(tool.id);
    expect((await api.setLabel(newest!.id, 'Renamed')).label).toBe('Renamed');
    await rejectsWith(api.setLabel(newest!.id, ''), 'invalid_input');
    expect((await api.setPinned(newest!.id, true)).pinned).toBe(true);
    await api.deleteVersion(second!.id);
    const after = await api.listVersions(tool.id);
    expect(after.some((x) => x.id === second!.id)).toBe(false);
    await rejectsWith(api.deleteVersion(second!.id), 'not_found');
  });

  it('reads and writes settings with validation', async () => {
    const { api, shop } = await setup();
    expect(await api.getGlobalSettings()).toEqual({
      sizeCapBytes: 50 * 1024 * 1024,
      retentionKeep: 200,
    });
    const s = await api.getProjectSettings(shop.id);
    expect(s).toEqual({ extraIgnore: [], respectGitignore: true, autoSnapshot: { kind: 'off' } });
    await api.setProjectSettings(shop.id, { ...s, autoSnapshot: { kind: 'afterIdle', secs: 30 } });
    expect((await api.getProjectSettings(shop.id)).autoSnapshot).toEqual({
      kind: 'afterIdle',
      secs: 30,
    });
    await rejectsWith(
      api.setProjectSettings(shop.id, { ...s, autoSnapshot: { kind: 'every', secs: 0 } }),
      'invalid_input',
    );
    await rejectsWith(
      api.setGlobalSettings({ sizeCapBytes: 1, retentionKeep: 0 }),
      'invalid_input',
    );
  });

  it('applies retention but keeps pinned versions', async () => {
    const { api, tool } = await setup();
    const pinned = (await api.listVersions(tool.id)).filter((x) => x.pinned);
    await api.setGlobalSettings({ sizeCapBytes: 50 * 1024 * 1024, retentionKeep: 3 });
    const after = await api.listVersions(tool.id);
    expect(after.filter((x) => !x.pinned).length).toBe(3);
    for (const p of pinned) expect(after.some((x) => x.id === p.id)).toBe(true);
  });

  it('returns copies, so callers cannot mutate mock state', async () => {
    const { api, shop } = await setup();
    const p = (await api.listProjects()).find((x) => x.id === shop.id)!;
    p.name = 'hacked';
    expect((await api.listProjects()).find((x) => x.id === shop.id)!.name).toBe('web-shop');
  });
});

describe('latency', () => {
  it('delays results when configured', async () => {
    vi.useFakeTimers();
    try {
      const api = createMockApi({ latencyMs: 100 });
      let done = false;
      const p = api.listProjects().then(() => (done = true));
      await vi.advanceTimersByTimeAsync(50);
      expect(done).toBe(false);
      await vi.advanceTimersByTimeAsync(60);
      await p;
      expect(done).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('contract tables', () => {
  it('maps every Api method to a snake_case command or event', () => {
    const api: Api = new MockApi();
    // Completeness is checked at compile time (`satisfies Record<...>` in commands.ts).
    for (const m of [...Object.keys(COMMANDS), ...Object.keys(EVENTS)]) {
      expect(typeof (api as unknown as Record<string, unknown>)[m]).toBe('function');
    }
    for (const name of Object.values(COMMANDS)) expect(name).toMatch(/^[a-z]+(_[a-z]+)*$/);
    expect(new Set(Object.values(COMMANDS)).size).toBe(Object.keys(COMMANDS).length);
  });
});
