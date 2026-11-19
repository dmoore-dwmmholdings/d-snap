// Agent turns in the mock Api (DSNA-123): the seeded story and the review operations.
import { describe, expect, it } from 'vitest';
import { createMockApi } from '../mock';
import type { Project } from '../types';

async function setup() {
  const api = createMockApi();
  const projects = await api.listProjects();
  const byName = (n: string) => projects.find((p) => p.name === n) as Project;
  return { api, shop: byName('web-shop'), tool: byName('rust-tool') };
}

describe('mock turns', () => {
  it('seeds web-shop with reviewed turns and one waiting', async () => {
    const { api, shop } = await setup();
    const turns = await api.listTurns(shop.id);
    expect(turns.map((t) => t.seq)).toEqual([5, 4, 3, 2, 1]);
    expect(turns[0]!.state).toBe('pending');
    expect(turns[0]!.prompt).toMatch(/payment step/);
    expect(turns.slice(1).every((t) => t.state === 'reviewed')).toBe(true);
  });

  it('lists turn files with later edits flagged', async () => {
    const { api, shop } = await setup();
    const [t] = await api.listTurns(shop.id);
    const files = await api.turnFiles(t!.id);
    const payment = files.find((f) => f.change.path === 'src/payment.ts')!;
    expect(payment.change.status.kind).toBe('added');
    expect(payment.changedSince).toBe(true); // deleted in the unsaved changes
    expect(files.every((f) => f.decision === null)).toBe(true);
  });

  it('keeps, undoes, skips changed files and redoes', async () => {
    const { api, shop } = await setup();
    const [t] = await api.listTurns(shop.id);
    const files = await api.turnFiles(t!.id);
    const clean = files.filter((f) => !f.changedSince).map((f) => f.change.path);

    const undo = await api.undoTurn(shop.id, t!.id, { paths: null, includeChanged: false });
    expect(undo.skippedChanged).toContain('src/payment.ts');
    expect(undo.turn.undone).toBe(clean.length);
    expect(undo.turn.state).toBe('pending');
    const versions = await api.listVersions(shop.id);
    expect(versions[0]!.label).toBe('Before undoing Turn 5');

    const kept = await api.keepTurn(t!.id);
    expect(kept.state).toBe('reviewed');

    const redo = await api.redoTurn(shop.id, t!.id, null);
    expect(redo.turn.undone).toBe(0);
    expect(redo.turn.state).toBe('pending');
  });

  it('rejects decisions on files the turn did not change', async () => {
    const { api, shop } = await setup();
    const [t] = await api.listTurns(shop.id);
    await expect(
      api.setTurnDecisions(t!.id, [{ path: 'README.md', decision: 'kept' }]),
    ).rejects.toMatchObject({ code: 'invalid_input' });
  });

  it('simulates the hooks', async () => {
    const { api, tool } = await setup();
    const begun = api.mockBeginTurn(tool.id, 'Rename the crate');
    expect(begun.state).toBe('running');
    api.mockWriteFile(tool.id, 'src/main.rs', 'fn main() {}\n');
    const ended = api.mockEndTurn(tool.id)!;
    expect(ended.state).toBe('pending');
    expect(ended.seq).toBe(begun.seq);
    const files = await api.turnFiles(ended.id);
    expect(files.map((f) => f.change.path)).toEqual(['src/main.rs']);
  });

  it('gives a file history and known paths', async () => {
    const { api, shop } = await setup();
    const history = await api.pathHistory(shop.id, 'src/cart.ts');
    expect(history.length).toBeGreaterThanOrEqual(3);
    expect(history[0]!.version.id).toBeGreaterThan(history.at(-1)!.version.id);
    const paths = await api.knownPaths(shop.id);
    expect(paths).toContain('src/config.ts');
    expect(paths).toContain('src/helpers.ts');
  });
});
