// The History column's timeline (DSNA-124): versions and agent turns merged into the items
// people care about. A turn's own snapshots fold into the turn; the snapshot taken when a
// prompt was submitted becomes "Your edits" (what changed between turns); consecutive
// auto-saves are grouped. Pure, so it is tested on its own.
import type { Turn, TurnId, Version, VersionId } from './api';

/** One row of the History column, newest first. */
export type HistoryItem =
  | { kind: 'turn'; key: string; time: number; turn: Turn }
  | {
      kind: 'edits';
      key: string;
      time: number;
      /** The version before the edits (`null` for the first snapshot). */
      from: VersionId | null;
      /** The "before agent turn" snapshot holding them. */
      to: Version;
      /** The turn they lead into. */
      turnId: TurnId;
    }
  | { kind: 'autos'; key: string; time: number; versions: Version[] }
  | { kind: 'version'; key: string; time: number; version: Version };

/** What a selected History row (or another screen) shows in the main area. */
export type Selection =
  | { kind: 'unsaved' }
  | { kind: 'turn'; id: TurnId }
  | { kind: 'edits'; from: VersionId | null; to: VersionId }
  | { kind: 'autos'; ids: VersionId[] }
  | { kind: 'version'; id: VersionId }
  | { kind: 'file'; path: string }
  | { kind: 'compare'; from: VersionId; to: VersionId | null };

/** The selection a History row stands for. */
export function selectionOf(item: HistoryItem): Selection {
  switch (item.kind) {
    case 'turn':
      return { kind: 'turn', id: item.turn.id };
    case 'edits':
      return { kind: 'edits', from: item.from, to: item.to.id };
    case 'autos':
      return { kind: 'autos', ids: item.versions.map((v) => v.id) };
    case 'version':
      return { kind: 'version', id: item.version.id };
  }
}

/** Stable key of a selection, for comparing it with History rows. */
export function selectionKey(s: Selection): string {
  switch (s.kind) {
    case 'unsaved':
      return 'unsaved';
    case 'turn':
      return `turn:${s.id}`;
    case 'edits':
      return `edits:${s.to}`;
    case 'autos':
      return `autos:${s.ids.join(',')}`;
    case 'version':
      return `version:${s.id}`;
    case 'file':
      return `file:${s.path}`;
    case 'compare':
      return `compare:${s.from}:${s.to ?? 'now'}`;
  }
}

const isBeforeSnapshot = (v: Version) => v.kind === 'cli' && v.label === 'before agent turn';
const total = (v: { counts: { added: number; modified: number; deleted: number } }) =>
  v.counts.added + v.counts.modified + v.counts.deleted;

/**
 * Merge `versions` and `turns` (both newest first, as the Api lists them) into History rows,
 * newest first. Empty turns (nothing changed) are left out.
 */
export function buildHistory(versions: Version[], turns: Turn[]): HistoryItem[] {
  const oldest = [...versions].reverse();
  const afterOf = new Map<VersionId, Turn>();
  const beforeOf = new Map<VersionId, Turn>();
  for (const t of turns) {
    if (t.after !== null) afterOf.set(t.after, t);
    if (t.before !== null) beforeOf.set(t.before, t);
  }

  const items: HistoryItem[] = [];
  let autos: Version[] = [];
  const flushAutos = () => {
    if (autos.length === 1) {
      const v = autos[0]!;
      items.push({ kind: 'version', key: `version:${v.id}`, time: v.createdAtMs, version: v });
    } else if (autos.length > 1) {
      const newestFirst = [...autos].reverse();
      items.push({
        kind: 'autos',
        key: `autos:${newestFirst.map((v) => v.id).join(',')}`,
        time: newestFirst[0]!.createdAtMs,
        versions: newestFirst,
      });
    }
    autos = [];
  };

  oldest.forEach((v, i) => {
    if (afterOf.has(v.id)) {
      // Shown by its turn. A version that is both one turn's after and the next one's
      // before (no edits in between) is shown once, by the earlier turn.
      flushAutos();
      return;
    }
    const startsTurn = beforeOf.get(v.id);
    if (startsTurn && isBeforeSnapshot(v)) {
      flushAutos();
      if (i > 0 && total(v) > 0) {
        items.push({
          kind: 'edits',
          key: `edits:${v.id}`,
          time: v.createdAtMs,
          from: oldest[i - 1]!.id,
          to: v,
          turnId: startsTurn.id,
        });
      }
      return;
    }
    if (v.kind === 'auto') {
      autos.push(v);
      return;
    }
    flushAutos();
    items.push({ kind: 'version', key: `version:${v.id}`, time: v.createdAtMs, version: v });
  });
  flushAutos();

  for (const t of turns) {
    if (t.state === 'empty') continue;
    items.push({
      kind: 'turn',
      key: `turn:${t.id}`,
      // Running turns sort at the top; ended ones where they ended.
      time: t.endedAtMs ?? Number.MAX_SAFE_INTEGER,
      turn: t,
    });
  }

  // Newest first; a turn sorts after the edits it started from at the same time.
  const rank = (x: HistoryItem) => (x.kind === 'turn' ? 1 : 0);
  return items.sort((a, b) => b.time - a.time || rank(b) - rank(a));
}

/** Turns waiting for review (pending), newest first. */
export function waitingTurns(turns: Turn[]): Turn[] {
  return turns.filter((t) => t.state === 'pending');
}
