import { describe, expect, it } from 'vitest';
import type { Turn, Version, VersionKind } from './api';
import { buildHistory, selectionKey, selectionOf } from './history';

let at = 0;
function v(id: number, kind: VersionKind, label = 'x', changed = 1): Version {
  at += 1000;
  return {
    id,
    projectId: 1,
    label,
    createdAtMs: at,
    kind,
    pinned: false,
    unstable: false,
    counts: { added: 0, modified: changed, deleted: 0 },
  };
}

function turn(id: number, before: Version, after: Version | null, state: Turn['state']): Turn {
  return {
    id,
    projectId: 1,
    seq: id,
    sessionId: null,
    prompt: `prompt ${id}`,
    startedAtMs: before.createdAtMs,
    endedAtMs: after?.createdAtMs ?? null,
    before: before.id,
    after: after?.id ?? null,
    counts: { added: 0, modified: 1, deleted: 0 },
    kept: 0,
    undone: 0,
    reviewedAtMs: null,
    state,
  };
}

const kinds = (items: ReturnType<typeof buildHistory>) =>
  items.map((i) => (i.kind === 'turn' ? `turn${i.turn.id}` : `${i.kind}:${i.key.split(':')[1]}`));

describe('buildHistory', () => {
  it('folds hook snapshots into turns and shows edits between them', () => {
    at = 0;
    const v1 = v(1, 'manual', 'Initial');
    const b1 = v(2, 'cli', 'before agent turn');
    const a1 = v(3, 'cli', 'after agent turn');
    const b2 = v(4, 'cli', 'before agent turn'); // user edits since a1
    const a2 = v(5, 'cli', 'after agent turn');
    const versions = [a2, b2, a1, b1, v1];
    const turns = [turn(2, b2, a2, 'pending'), turn(1, b1, a1, 'reviewed')];
    const items = buildHistory(versions, turns);
    expect(kinds(items)).toEqual(['turn2', 'edits:4', 'turn1', 'edits:2', 'version:1']);
    const edits = items[1]!;
    expect(edits.kind === 'edits' && edits.from).toBe(3);
  });

  it('groups consecutive auto-saves and keeps a single one as a version', () => {
    at = 0;
    const a1 = v(1, 'auto');
    const a2 = v(2, 'auto');
    const a3 = v(3, 'auto');
    const c = v(4, 'manual', 'Checkpoint');
    const a5 = v(5, 'auto');
    const items = buildHistory([a5, c, a3, a2, a1], []);
    expect(items.map((i) => i.key)).toEqual(['version:5', 'version:4', 'autos:3,2,1']);
  });

  it('puts a running turn on top and hides empty ones', () => {
    at = 0;
    const b1 = v(1, 'cli', 'before agent turn');
    const a1 = v(2, 'cli', 'after agent turn', 0);
    const b2 = v(3, 'manual', 'Mine');
    const items = buildHistory(
      [b2, a1, b1],
      [turn(2, b2, null, 'running'), turn(1, b1, a1, 'empty')],
    );
    expect(kinds(items)).toEqual(['turn2', 'version:3']);
  });

  it('keys selections like their rows', () => {
    at = 0;
    const b = v(1, 'cli', 'before agent turn');
    const a = v(2, 'cli', 'after agent turn');
    for (const item of buildHistory([a, b, v(0, 'manual')], [turn(1, b, a, 'pending')])) {
      expect(selectionKey(selectionOf(item))).toBe(item.key);
    }
  });
});
