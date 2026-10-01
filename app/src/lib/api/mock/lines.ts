// Line diff for the mock Api: Myers O(ND) diff, grouped into unified-style hunks.
import type { DiffLine, DiffOptions, Hunk } from '../types';

/** Splits text into raw lines (keeping any `\r`), dropping the empty tail after a final newline. */
export function splitLines(text: string): string[] {
  if (text === '') return [];
  const lines = text.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  return lines;
}

function normalize(line: string, ignoreWhitespace: boolean): string {
  return ignoreWhitespace ? line.replace(/\s+/g, ' ').trim() : line;
}

function display(line: string): string {
  return line.endsWith('\r') ? line.slice(0, -1) : line;
}

type Op = { tag: 'equal' | 'insert' | 'delete'; oldIdx: number; newIdx: number };

/** Myers diff. Keeps only the frontier for each `d`, so memory is O(D^2). */
function myers(a: string[], b: string[]): Op[] {
  const n = a.length;
  const m = b.length;
  const max = n + m;
  const offset = max + 1;
  const v = new Int32Array(2 * max + 3);
  const trace: Int32Array[] = [];
  let finalD = -1;

  outer: for (let d = 0; d <= max; d++) {
    for (let k = -d; k <= d; k += 2) {
      let x: number;
      if (k === -d || (k !== d && (v[offset + k - 1] ?? 0) < (v[offset + k + 1] ?? 0))) {
        x = v[offset + k + 1] ?? 0;
      } else {
        x = (v[offset + k - 1] ?? 0) + 1;
      }
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[offset + k] = x;
      if (x >= n && y >= m) {
        trace.push(v.slice(offset - d, offset + d + 1));
        finalD = d;
        break outer;
      }
    }
    trace.push(v.slice(offset - d, offset + d + 1));
  }

  const ops: Op[] = [];
  let x = n;
  let y = m;
  for (let d = finalD; d > 0; d--) {
    const prev = trace[d - 1];
    if (!prev) break;
    const at = (k: number): number => prev[k + (d - 1)] ?? 0;
    const k = x - y;
    const prevK = k === -d || (k !== d && at(k - 1) < at(k + 1)) ? k + 1 : k - 1;
    const prevX = at(prevK);
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      x--;
      y--;
      ops.push({ tag: 'equal', oldIdx: x, newIdx: y });
    }
    if (x === prevX) {
      y--;
      ops.push({ tag: 'insert', oldIdx: x, newIdx: y });
    } else {
      x--;
      ops.push({ tag: 'delete', oldIdx: x, newIdx: y });
    }
  }
  while (x > 0 && y > 0) {
    x--;
    y--;
    ops.push({ tag: 'equal', oldIdx: x, newIdx: y });
  }
  return ops.reverse();
}

/** Result of a line diff. */
export interface LineDiff {
  hunks: Hunk[];
  added: number;
  removed: number;
}

/** Diffs two texts into hunks with `opts.context` lines of context. */
export function lineDiff(oldText: string, newText: string, opts: DiffOptions): LineDiff {
  const a = splitLines(oldText);
  const b = splitLines(newText);
  const ops = myers(
    a.map((l) => normalize(l, opts.ignoreWhitespace)),
    b.map((l) => normalize(l, opts.ignoreWhitespace)),
  );

  let added = 0;
  let removed = 0;
  const lines: DiffLine[] = ops.map((op) => {
    if (op.tag === 'insert') {
      added++;
      return {
        tag: 'insert',
        oldNo: null,
        newNo: op.newIdx + 1,
        text: display(b[op.newIdx] ?? ''),
      };
    }
    if (op.tag === 'delete') {
      removed++;
      return {
        tag: 'delete',
        oldNo: op.oldIdx + 1,
        newNo: null,
        text: display(a[op.oldIdx] ?? ''),
      };
    }
    return {
      tag: 'equal',
      oldNo: op.oldIdx + 1,
      newNo: op.newIdx + 1,
      text: display(b[op.newIdx] ?? ''),
    };
  });

  return { hunks: groupHunks(lines, Math.max(0, opts.context)), added, removed };
}

function groupHunks(lines: DiffLine[], context: number): Hunk[] {
  const changeIdx: number[] = [];
  lines.forEach((l, i) => {
    if (l.tag !== 'equal') changeIdx.push(i);
  });
  if (changeIdx.length === 0) return [];

  const ranges: [number, number][] = [];
  let start = Math.max(0, (changeIdx[0] ?? 0) - context);
  let end = Math.min(lines.length - 1, (changeIdx[0] ?? 0) + context);
  for (const i of changeIdx.slice(1)) {
    if (i - context <= end + 1) {
      end = Math.min(lines.length - 1, i + context);
    } else {
      ranges.push([start, end]);
      start = Math.max(0, i - context);
      end = Math.min(lines.length - 1, i + context);
    }
  }
  ranges.push([start, end]);

  // Old/new lines consumed before index i, for hunk start numbers.
  return ranges.map(([s, e]) => {
    let oldBefore = 0;
    let newBefore = 0;
    for (let i = 0; i < s; i++) {
      const l = lines[i];
      if (l?.oldNo !== null) oldBefore++;
      if (l?.newNo !== null) newBefore++;
    }
    const hunkLines = lines.slice(s, e + 1);
    const oldLen = hunkLines.filter((l) => l.oldNo !== null).length;
    const newLen = hunkLines.filter((l) => l.newNo !== null).length;
    return {
      oldStart: oldLen > 0 ? oldBefore + 1 : oldBefore,
      oldLen,
      newStart: newLen > 0 ? newBefore + 1 : newBefore,
      newLen,
      lines: hunkLines,
    };
  });
}

/**
 * Reverts one hunk: replaces its new-side lines in `newText` with its old-side
 * lines from `oldText`. Keeps `newText`'s trailing newline.
 */
export function revertHunkText(oldText: string, newText: string, hunk: Hunk): string {
  const a = splitLines(oldText);
  const b = splitLines(newText);
  const oldFrom = hunk.oldLen > 0 ? hunk.oldStart - 1 : hunk.oldStart;
  const newFrom = hunk.newLen > 0 ? hunk.newStart - 1 : hunk.newStart;
  const result = [
    ...b.slice(0, newFrom),
    ...a.slice(oldFrom, oldFrom + hunk.oldLen),
    ...b.slice(newFrom + hunk.newLen),
  ];
  if (result.length === 0) return '';
  const trailing = newText === '' ? oldText.endsWith('\n') : newText.endsWith('\n');
  return result.join('\n') + (trailing ? '\n' : '');
}
