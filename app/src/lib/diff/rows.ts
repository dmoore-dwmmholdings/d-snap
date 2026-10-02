// Row models for the diff viewer (DSNA-44): flat lists that a virtual list renders.
import type { DiffLine, Hunk, LineTag } from '../api';

/** One side of a line. */
export interface Cell {
  no: number;
  text: string;
  tag: LineTag;
}

/** A hunk header, carrying its index for "Revert". */
export interface HunkRow {
  type: 'hunk';
  hunk: number;
  header: string;
}

/** Unchanged lines hidden between hunks. */
export interface GapRow {
  type: 'gap';
  hidden: number;
}

/** Inline mode: one line, with both line numbers. */
export interface LineRow {
  type: 'line';
  hunk: number;
  tag: LineTag;
  oldNo: number | null;
  newNo: number | null;
  text: string;
}

/** Split mode: old and new side of one row (`null` = filler). */
export interface PairRow {
  type: 'pair';
  hunk: number;
  left: Cell | null;
  right: Cell | null;
}

export type InlineRow = HunkRow | GapRow | LineRow;
export type SplitRow = HunkRow | GapRow | PairRow;

function header(h: Hunk): string {
  return `@@ -${h.oldStart},${h.oldLen} +${h.newStart},${h.newLen} @@`;
}

/** Unchanged lines between the end of `prev` and the start of `next` (old side). */
function hidden(prev: Hunk | undefined, next: Hunk): number {
  const prevEnd = prev ? prev.oldStart + prev.oldLen - (prev.oldLen === 0 ? 0 : 1) : 0;
  const nextStart = next.oldLen === 0 ? next.oldStart + 1 : next.oldStart;
  return Math.max(0, nextStart - prevEnd - 1);
}

function withGaps<R>(hunks: Hunk[], body: (h: Hunk, i: number) => R[]): (HunkRow | GapRow | R)[] {
  const rows: (HunkRow | GapRow | R)[] = [];
  hunks.forEach((h, i) => {
    const gap = hidden(hunks[i - 1], h);
    if (gap > 0) rows.push({ type: 'gap', hidden: gap });
    rows.push({ type: 'hunk', hunk: i, header: header(h) });
    rows.push(...body(h, i));
  });
  return rows;
}

/** Rows for the inline view. */
export function inlineRows(hunks: Hunk[]): InlineRow[] {
  return withGaps(hunks, (h, i) =>
    h.lines.map((l): LineRow => ({
      type: 'line',
      hunk: i,
      tag: l.tag,
      oldNo: l.oldNo,
      newNo: l.newNo,
      text: l.text,
    })),
  );
}

/**
 * Rows for the side-by-side view. Equal lines sit on both sides; in each run of changes the
 * n-th deleted line is paired with the n-th inserted line, the longer side gets fillers.
 */
export function splitRows(hunks: Hunk[]): SplitRow[] {
  return withGaps(hunks, (h, i) => {
    const out: PairRow[] = [];
    let dels: DiffLine[] = [];
    let ins: DiffLine[] = [];
    const flush = () => {
      for (let k = 0; k < Math.max(dels.length, ins.length); k++) {
        const d = dels[k];
        const n = ins[k];
        out.push({
          type: 'pair',
          hunk: i,
          left: d ? { no: d.oldNo ?? 0, text: d.text, tag: 'delete' } : null,
          right: n ? { no: n.newNo ?? 0, text: n.text, tag: 'insert' } : null,
        });
      }
      dels = [];
      ins = [];
    };
    for (const l of h.lines) {
      if (l.tag === 'delete') dels.push(l);
      else if (l.tag === 'insert') ins.push(l);
      else {
        flush();
        out.push({
          type: 'pair',
          hunk: i,
          left: { no: l.oldNo ?? 0, text: l.text, tag: 'equal' },
          right: { no: l.newNo ?? 0, text: l.text, tag: 'equal' },
        });
      }
    }
    flush();
    return out;
  });
}

/** Total diff lines, to decide whether to highlight. */
export function lineCount(hunks: Hunk[]): number {
  return hunks.reduce((n, h) => n + h.lines.length, 0);
}
