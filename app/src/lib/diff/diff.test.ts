// Chain I (DSNA-44/45/46): rows, highlighting and the viewer.
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';
import type { FileDiff, Hunk } from '../api';
import DiffViewer from './DiffViewer.svelte';
import { highlightHunks, languageOf, splitHtmlLines } from './highlight';
import { inlineRows, splitRows } from './rows';

const hunk: Hunk = {
  oldStart: 10,
  oldLen: 3,
  newStart: 10,
  newLen: 4,
  lines: [
    { tag: 'equal', oldNo: 10, newNo: 10, text: 'const a = 1;' },
    { tag: 'delete', oldNo: 11, newNo: null, text: 'const b = 2;' },
    { tag: 'insert', oldNo: null, newNo: 11, text: 'const b = 3;' },
    { tag: 'insert', oldNo: null, newNo: 12, text: 'const c = 4;' },
    { tag: 'equal', oldNo: 12, newNo: 13, text: 'export { a };' },
  ],
};
const second: Hunk = {
  oldStart: 40,
  oldLen: 1,
  newStart: 41,
  newLen: 0,
  lines: [{ tag: 'delete', oldNo: 40, newNo: null, text: 'gone();' }],
};
const text = (hunks: Hunk[], path = 'src/a.ts'): FileDiff => ({
  path,
  body: { kind: 'text', hunks },
});

describe('rows', () => {
  it('pairs deletes with inserts side by side and adds gaps', () => {
    const rows = splitRows([hunk, second]);
    expect(rows[0]).toEqual({ type: 'gap', hidden: 9 });
    expect(rows[1]).toMatchObject({ type: 'hunk', header: '@@ -10,3 +10,4 @@' });
    expect(rows[3]).toMatchObject({
      left: { no: 11, tag: 'delete' },
      right: { no: 11, tag: 'insert', text: 'const b = 3;' },
    });
    expect(rows[4]).toMatchObject({ left: null, right: { no: 12 } });
    expect(rows[6]).toEqual({ type: 'gap', hidden: 27 });
  });

  it('keeps every line inline with both numbers', () => {
    const rows = inlineRows([hunk]).filter((r) => r.type === 'line');
    expect(rows.map((r) => r.type === 'line' && [r.oldNo, r.newNo])).toEqual([
      [10, 10],
      [11, null],
      [null, 11],
      [null, 12],
      [12, 13],
    ]);
  });
});

describe('highlighting', () => {
  it('detects languages from extensions', () => {
    expect(languageOf('src/main.rs')).toBe('rust');
    expect(languageOf('App.SVELTE')).toBe('xml');
    expect(languageOf('README')).toBeNull();
    expect(languageOf('data.bin')).toBeNull();
  });

  it('splits multi-line spans per line and reopens them', () => {
    const lines = splitHtmlLines('<span class="c">/* a\nb */</span> x');
    expect(lines).toEqual(['<span class="c">/* a</span>', '<span class="c">b */</span> x']);
  });

  it('does not change line text', async () => {
    const h = await highlightHunks('src/a.ts', [hunk]);
    expect(h).not.toBeNull();
    const strip = (s: string) =>
      s
        .replace(/<[^>]+>/g, '')
        .replace(/&#(\d+);|&lt;|&gt;|&amp;|&quot;/g, (m, n: string | undefined) =>
          n
            ? String.fromCharCode(Number(n))
            : { '&lt;': '<', '&gt;': '>', '&amp;': '&', '&quot;': '"' }[m]!,
        );
    expect(strip(h!.new.get(11)!)).toBe('const b = 3;');
    expect(strip(h!.old.get(11)!)).toBe('const b = 2;');
  });

  it('highlights a 5,000-line diff quickly', async () => {
    const lines = Array.from({ length: 5000 }, (_, i) => ({
      tag: 'insert' as const,
      oldNo: null,
      newNo: i + 1,
      text: `export const v${i} = { id: ${i}, name: "item ${i}" }; // ${i}`,
    }));
    const t = performance.now();
    const h = await highlightHunks('big.ts', [
      { oldStart: 0, oldLen: 0, newStart: 1, newLen: 5000, lines },
    ]);
    expect(h?.new.size).toBe(5000);
    expect(performance.now() - t).toBeLessThan(3000);
  });
});

describe('DiffViewer', () => {
  const base = { mode: 'split' as const, ignoreWhitespace: false, onToggleWhitespace: () => {} };

  it('renders hunks and line numbers side by side and inline', async () => {
    const r = render(DiffViewer, { props: { ...base, diff: text([hunk]) } });
    expect(screen.getByText('@@ -10,3 +10,4 @@')).toBeInTheDocument();
    expect(screen.getAllByText('const b = 3;').length).toBeGreaterThan(0);
    await r.rerender({ ...base, mode: 'inline', diff: text([hunk]) });
    const numbers = screen.getAllByText('13');
    expect(numbers.length).toBeGreaterThan(0);
  });

  it('calls back on the whitespace toggle', async () => {
    const onToggleWhitespace = vi.fn();
    render(DiffViewer, { props: { ...base, onToggleWhitespace, diff: text([hunk]) } });
    await fireEvent.click(screen.getByLabelText(/Ignore whitespace/));
    expect(onToggleWhitespace).toHaveBeenCalledWith(true);
  });

  it('shows Revert only with onRevertHunk', async () => {
    const { unmount } = render(DiffViewer, { props: { ...base, diff: text([hunk, second]) } });
    expect(screen.queryByRole('button', { name: 'Revert' })).toBeNull();
    unmount();
    const onRevertHunk = vi.fn();
    render(DiffViewer, { props: { ...base, onRevertHunk, diff: text([hunk, second]) } });
    const buttons = screen.getAllByRole('button', { name: 'Revert' });
    await fireEvent.click(buttons[1]!);
    expect(onRevertHunk).toHaveBeenCalledWith(1);
  });

  it('notes a rename with identical contents', () => {
    render(DiffViewer, {
      props: { ...base, diff: text([]), status: { kind: 'renamed', from: 'src/old.ts' } },
    });
    expect(screen.getByText(/contents identical/)).toBeInTheDocument();
  });

  it('renders binary, too-large and image bodies', async () => {
    const hash = 'ab'.repeat(32);
    const { unmount } = render(DiffViewer, {
      props: {
        ...base,
        diff: {
          path: 'm.bin',
          body: { kind: 'binary', oldSize: 512, newSize: 640, oldHash: hash, newHash: null },
        },
      },
    });
    expect(screen.getByText(/Binary file/)).toBeInTheDocument();
    expect(screen.getByText('+128 B')).toBeInTheDocument();
    expect(screen.getByText(hash.slice(0, 12))).toBeInTheDocument();
    unmount();

    const big = render(DiffViewer, {
      props: {
        ...base,
        diff: { path: 'x.log', body: { kind: 'tooLarge', size: 3 * 1024 * 1024 } },
      },
    });
    expect(screen.getByText(/too large to diff \(3\.0 MB\)/)).toBeInTheDocument();
    big.unmount();

    const loadImage = vi.fn(async () => 'data:image/png;base64,AAAA');
    render(DiffViewer, {
      props: {
        ...base,
        loadImage,
        diff: {
          path: 'logo.png',
          body: { kind: 'image', old: null, new: hash, mime: 'image/png' },
        },
      },
    });
    await waitFor(() => expect(screen.getByRole('img', { name: /After/ })).toBeInTheDocument());
    expect(
      within(screen.getByText(/^Before/).closest('figure')!).getByText('Added'),
    ).toBeInTheDocument();
    expect(loadImage).toHaveBeenCalledWith(hash, 'image/png');
  });
});
