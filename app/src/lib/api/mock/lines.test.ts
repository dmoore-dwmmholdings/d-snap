import { lineDiff, revertHunkText, splitLines } from './lines';

const opts = { ignoreWhitespace: false, context: 3 };

describe('splitLines', () => {
  it('drops the empty tail after a final newline and keeps CR', () => {
    expect(splitLines('')).toEqual([]);
    expect(splitLines('a\nb\n')).toEqual(['a', 'b']);
    expect(splitLines('a\r\nb')).toEqual(['a\r', 'b']);
  });
});

describe('lineDiff', () => {
  it('returns no hunks for equal text', () => {
    expect(lineDiff('a\nb\n', 'a\nb\n', opts)).toEqual({ hunks: [], added: 0, removed: 0 });
  });

  it('reports a replaced line with context and 1-based numbers', () => {
    const old = 'l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n';
    const nu = 'l1\nl2\nl3\nl4\nX\nl6\nl7\nl8\n';
    const d = lineDiff(old, nu, opts);
    expect(d.added).toBe(1);
    expect(d.removed).toBe(1);
    expect(d.hunks).toHaveLength(1);
    const h = d.hunks[0]!;
    expect([h.oldStart, h.oldLen, h.newStart, h.newLen]).toEqual([2, 7, 2, 7]);
    expect(h.lines.find((l) => l.tag === 'delete')).toEqual({
      tag: 'delete',
      oldNo: 5,
      newNo: null,
      text: 'l5',
    });
    expect(h.lines.find((l) => l.tag === 'insert')).toEqual({
      tag: 'insert',
      oldNo: null,
      newNo: 5,
      text: 'X',
    });
  });

  it('splits distant changes into separate hunks', () => {
    const old = Array.from({ length: 40 }, (_, i) => `line ${i}`).join('\n');
    const lines = old.split('\n');
    lines[2] = 'changed a';
    lines[35] = 'changed b';
    expect(lineDiff(old, lines.join('\n'), opts).hunks).toHaveLength(2);
  });

  it('handles an added file', () => {
    const d = lineDiff('', 'a\nb\n', opts);
    expect(d.added).toBe(2);
    expect(d.hunks[0]).toMatchObject({ oldStart: 0, oldLen: 0, newStart: 1, newLen: 2 });
  });

  it('ignores whitespace and CRLF when asked', () => {
    const d = lineDiff('a  b\r\nc\n', 'a b\nc\n', { ignoreWhitespace: true, context: 3 });
    expect(d.hunks).toEqual([]);
    expect(lineDiff('a\r\n', 'a\n', opts).hunks).toHaveLength(1);
  });

  it('diffs a 5k-line file with 200 changes quickly', () => {
    const a = Array.from({ length: 5000 }, (_, i) => `row ${i}`);
    const b = a.map((l, i) => (i % 25 === 0 ? `${l} changed` : l));
    const start = performance.now();
    const d = lineDiff(a.join('\n'), b.join('\n'), opts);
    expect(performance.now() - start).toBeLessThan(2000);
    expect(d.added).toBe(200);
    expect(d.removed).toBe(200);
  });
});

describe('revertHunkText', () => {
  it('restores the old lines of one hunk only', () => {
    const old = Array.from({ length: 30 }, (_, i) => `line ${i}`).join('\n') + '\n';
    const lines = old.trimEnd().split('\n');
    lines[2] = 'changed a';
    lines[25] = 'changed b';
    const nu = lines.join('\n') + '\n';
    const d = lineDiff(old, nu, opts);
    const reverted = revertHunkText(old, nu, d.hunks[0]!);
    expect(reverted).toContain('line 2\n');
    expect(reverted).toContain('changed b');
    expect(lineDiff(old, reverted, opts).hunks).toHaveLength(1);
  });

  it('reverts a pure insertion', () => {
    const d = lineDiff('a\nb\n', 'a\nnew\nb\n', opts);
    expect(revertHunkText('a\nb\n', 'a\nnew\nb\n', d.hunks[0]!)).toBe('a\nb\n');
  });
});
