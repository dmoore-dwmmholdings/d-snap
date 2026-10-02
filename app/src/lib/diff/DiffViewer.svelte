<!--
  Diff viewer (Chain I: DSNA-44/45/46, F14, F15, F17). Text diffs side by side or inline,
  virtualized, with optional syntax highlighting, hunk "Revert" (F22) and "show hidden
  lines". Binary, image and too-large bodies get their own panels.
-->
<script lang="ts">
  import type { BlobHash, ChangeStatus, FileDiff } from '../api';
  import VirtualList from '../components/VirtualList.svelte';
  import { formatBytes } from '../util/format';
  import { escapeHtml, highlightHunks, languageOf, type Highlighted } from './highlight';
  import { inlineRows, lineCount, splitRows, type InlineRow, type SplitRow } from './rows';

  interface Props {
    diff: FileDiff;
    mode: 'split' | 'inline';
    ignoreWhitespace: boolean;
    onToggleWhitespace: (on: boolean) => void;
    onModeChange?: (mode: 'split' | 'inline') => void;
    /** Shows a "Revert" button on each hunk when set. */
    onRevertHunk?: (hunkIndex: number) => void;
    /** "Show hidden lines" between hunks; omitted = no button. */
    onExpand?: () => void;
    /** Status of the file, for the "renamed, contents identical" note. */
    status?: ChangeStatus;
    /** Image bytes as a data URL (`Api.readBlobAsDataUrl`). */
    loadImage?: (hash: BlobHash, mime: string) => Promise<string>;
    /** A newer diff is loading; dims the current one. */
    busy?: boolean;
  }

  let {
    diff,
    mode,
    ignoreWhitespace,
    onToggleWhitespace,
    onModeChange,
    onRevertHunk,
    onExpand,
    status,
    loadImage,
    busy = false,
  }: Props = $props();

  const ROW = 20;
  const HL_KEY = 'dsnap.highlight';

  function readHighlightPref(): boolean {
    try {
      return globalThis.localStorage?.getItem(HL_KEY) !== 'off';
    } catch {
      return true;
    }
  }

  let highlightOn = $state(readHighlightPref());
  let highlighted = $state<Highlighted | null>(null);

  const hunks = $derived(diff.body.kind === 'text' ? diff.body.hunks : []);
  const canHighlight = $derived(diff.body.kind === 'text' && languageOf(diff.path) !== null);
  const inline = $derived<InlineRow[]>(mode === 'inline' ? inlineRows(hunks) : []);
  const split = $derived<SplitRow[]>(mode === 'split' ? splitRows(hunks) : []);

  $effect(() => {
    const path = diff.path;
    const h = hunks;
    highlighted = null;
    if (!highlightOn || !canHighlight || h.length === 0) return;
    let cancelled = false;
    highlightHunks(path, h)
      .then((r) => {
        if (!cancelled) highlighted = r;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  });

  function toggleHighlight(on: boolean) {
    highlightOn = on;
    try {
      globalThis.localStorage?.setItem(HL_KEY, on ? 'on' : 'off');
    } catch {
      // Not persisted; fine.
    }
  }

  function code(side: 'old' | 'new', no: number | null, text: string): string {
    const h = no !== null ? highlighted?.[side].get(no) : undefined;
    return h ?? escapeHtml(text);
  }

  const sign = { equal: ' ', insert: '+', delete: '-' } as const;

  // Images (DSNA-46)
  let images = $state<{ old: string | null; new: string | null; error: string | null }>({
    old: null,
    new: null,
    error: null,
  });
  let dims = $state<{ old: string; new: string }>({ old: '', new: '' });
  $effect(() => {
    const b = diff.body;
    images = { old: null, new: null, error: null };
    dims = { old: '', new: '' };
    if (b.kind !== 'image' || !loadImage) return;
    let cancelled = false;
    const load = (h: BlobHash | null) => (h ? loadImage(h, b.mime) : Promise.resolve(null));
    Promise.all([load(b.old), load(b.new)])
      .then(([o, n]) => {
        if (!cancelled) images = { old: o, new: n, error: null };
      })
      .catch((e: unknown) => {
        if (!cancelled)
          images = { old: null, new: null, error: String((e as Error)?.message ?? e) };
      });
    return () => {
      cancelled = true;
    };
  });

  function short(h: BlobHash | null): string {
    return h ? h.slice(0, 12) : '—';
  }
</script>

<div class="viewer" class:busy>
  {#if diff.body.kind === 'text'}
    <div class="toolbar">
      {#if onModeChange}
        <div class="seg" role="group" aria-label="Diff layout">
          <button
            type="button"
            aria-pressed={mode === 'split'}
            onclick={() => onModeChange('split')}>Side by side</button
          >
          <button
            type="button"
            aria-pressed={mode === 'inline'}
            onclick={() => onModeChange('inline')}>Inline</button
          >
        </div>
      {/if}
      <label>
        <input
          type="checkbox"
          checked={ignoreWhitespace}
          onchange={(e) => onToggleWhitespace(e.currentTarget.checked)}
        />
        Ignore whitespace / EOL
      </label>
      <label title={canHighlight ? undefined : 'No highlighting for this file type'}>
        <input
          type="checkbox"
          checked={highlightOn && canHighlight}
          disabled={!canHighlight}
          onchange={(e) => toggleHighlight(e.currentTarget.checked)}
        />
        Highlight
      </label>
      <span class="stat"
        >{lineCount(hunks)} lines in {hunks.length} {hunks.length === 1 ? 'hunk' : 'hunks'}</span
      >
    </div>

    {#if hunks.length === 0}
      <p class="note">
        {#if status?.kind === 'renamed'}
          Renamed from <span class="mono">{status.from}</span>, contents identical.
        {:else if ignoreWhitespace}
          Only whitespace or line endings changed.
        {:else}
          No line changes.
        {/if}
      </p>
    {:else}
      <div class="code" class:split={mode === 'split'}>
        {#snippet hunkRow(r: { hunk: number; header: string })}
          <div class="hunk">
            <span class="hdr mono">{r.header}</span>
            {#if onRevertHunk}
              <button type="button" class="revert" onclick={() => onRevertHunk(r.hunk)}
                >Revert</button
              >
            {/if}
          </div>
        {/snippet}
        {#snippet gapRow(n: number)}
          <div class="gap">
            {#if onExpand}
              <button type="button" class="link" onclick={onExpand}>⋯ {n} unchanged lines</button>
            {:else}
              <span>⋯ {n} unchanged lines</span>
            {/if}
          </div>
        {/snippet}

        {#if mode === 'inline'}
          <VirtualList items={inline} rowHeight={ROW} label="Diff lines">
            {#snippet row(r: InlineRow)}
              {#if r.type === 'hunk'}
                {@render hunkRow(r)}
              {:else if r.type === 'gap'}
                {@render gapRow(r.hidden)}
              {:else}
                {@const html =
                  r.tag === 'delete' ? code('old', r.oldNo, r.text) : code('new', r.newNo, r.text)}
                <div class="line t-{r.tag}">
                  <span class="no">{r.oldNo ?? ''}</span>
                  <span class="no">{r.newNo ?? ''}</span>
                  <span class="sign">{sign[r.tag]}</span>
                  <!-- eslint-disable-next-line svelte/no-at-html-tags -- escaped: highlight.js output or escapeHtml -->
                  <span class="text mono">{@html html}</span>
                </div>
              {/if}
            {/snippet}
          </VirtualList>
        {:else}
          <VirtualList items={split} rowHeight={ROW} label="Diff lines">
            {#snippet row(r: SplitRow)}
              {#if r.type === 'hunk'}
                {@render hunkRow(r)}
              {:else if r.type === 'gap'}
                {@render gapRow(r.hidden)}
              {:else}
                <div class="pair">
                  {#each [['old', r.left], ['new', r.right]] as const as [side, c] (side)}
                    <div class="half t-{c?.tag ?? 'empty'}">
                      <span class="no">{c?.no ?? ''}</span>
                      <span class="sign">{c ? sign[c.tag] : ''}</span>
                      <!-- eslint-disable-next-line svelte/no-at-html-tags -- escaped: highlight.js output or escapeHtml -->
                      <span class="text mono">{@html c ? code(side, c.no, c.text) : ''}</span>
                    </div>
                  {/each}
                </div>
              {/if}
            {/snippet}
          </VirtualList>
        {/if}
      </div>
    {/if}
  {:else if diff.body.kind === 'binary'}
    {@const b = diff.body}
    <div class="panel">
      <p><strong>Binary file</strong> — contents differ; no line diff.</p>
      <table>
        <tbody>
          <tr
            ><th>Old</th><td>{b.oldSize === null ? 'absent' : formatBytes(b.oldSize)}</td><td
              class="mono">{short(b.oldHash)}</td
            ></tr
          >
          <tr
            ><th>New</th><td>{b.newSize === null ? 'absent' : formatBytes(b.newSize)}</td><td
              class="mono">{short(b.newHash)}</td
            ></tr
          >
          {#if b.oldSize !== null && b.newSize !== null}
            {@const delta = b.newSize - b.oldSize}
            <tr
              ><th>Change</th><td colspan="2"
                >{delta >= 0 ? '+' : '−'}{formatBytes(Math.abs(delta))}</td
              ></tr
            >
          {/if}
        </tbody>
      </table>
    </div>
  {:else if diff.body.kind === 'image'}
    {@const b = diff.body}
    <div class="images">
      {#each [['old', 'Before', b.old], ['new', 'After', b.new]] as const as [side, title, hash] (side)}
        <figure>
          <figcaption>{title}{dims[side] ? ` · ${dims[side]}` : ''}</figcaption>
          {#if !hash}
            <div class="absent">{side === 'old' ? 'Added' : 'Deleted'}</div>
          {:else if images[side]}
            <img
              src={images[side]}
              alt="{title}: {diff.path}"
              onload={(e) => {
                const i = e.currentTarget as HTMLImageElement;
                dims[side] = `${i.naturalWidth}×${i.naturalHeight}`;
              }}
            />
          {:else if images.error}
            <div class="absent">Could not load: {images.error}</div>
          {:else}
            <div class="absent">Loading…</div>
          {/if}
        </figure>
      {/each}
    </div>
  {:else}
    <p class="note">
      This file is too large to diff ({formatBytes(diff.body.size)}). Use Restore to bring back a
      version of it.
    </p>
  {/if}
</div>

<style>
  .viewer {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .busy {
    opacity: 0.6;
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-1) var(--space-3);
    border-bottom: 1px solid var(--border);
    font-size: var(--text-sm);
  }
  .toolbar label {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .stat {
    margin-left: auto;
    color: var(--text-muted);
  }
  .seg {
    display: inline-flex;
  }
  .seg button {
    border-radius: 0;
    font-size: var(--text-sm);
  }
  .seg button:first-child {
    border-radius: var(--radius-sm) 0 0 var(--radius-sm);
  }
  .seg button:last-child {
    border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
    margin-left: -1px;
  }
  .seg button[aria-pressed='true'] {
    background: var(--surface-2);
    font-weight: 600;
  }
  .code {
    flex: 1;
    min-height: 0;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 20px;
    tab-size: 4;
  }
  .line,
  .half {
    display: flex;
    height: 20px;
    white-space: pre;
  }
  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    height: 20px;
  }
  .half {
    overflow: hidden;
  }
  .half:first-child {
    border-right: 1px solid var(--border);
  }
  .no {
    width: 48px;
    flex: none;
    text-align: right;
    padding-right: 8px;
    color: var(--text-muted);
    user-select: none;
  }
  .sign {
    width: 14px;
    flex: none;
    user-select: none;
    color: var(--text-muted);
  }
  .text {
    flex: 1;
    overflow: hidden;
    text-overflow: clip;
  }
  .t-insert {
    background: var(--diff-insert-bg);
  }
  .t-insert .no {
    background: var(--diff-insert-gutter);
  }
  .t-delete {
    background: var(--diff-delete-bg);
  }
  .t-delete .no {
    background: var(--diff-delete-gutter);
  }
  .t-empty {
    background: var(--surface-1);
  }
  .hunk {
    display: flex;
    align-items: center;
    height: 20px;
    padding: 0 var(--space-2);
    background: var(--diff-hunk-bg);
    color: var(--text-muted);
  }
  .hdr {
    flex: 1;
  }
  .revert {
    font-size: var(--text-xs);
    padding: 0 6px;
    line-height: 16px;
  }
  .gap {
    height: 20px;
    padding-left: 62px;
    color: var(--text-muted);
    background: var(--surface-1);
  }
  .note,
  .panel {
    padding: var(--space-3);
    color: var(--text-muted);
    margin: 0;
  }
  .panel table {
    border-collapse: collapse;
  }
  .panel th,
  .panel td {
    text-align: left;
    padding: 2px var(--space-3) 2px 0;
  }
  .images {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
    padding: var(--space-3);
    overflow: auto;
  }
  figure {
    margin: 0;
  }
  figcaption {
    color: var(--text-muted);
    margin-bottom: var(--space-1);
  }
  img {
    max-width: 100%;
    background: repeating-conic-gradient(var(--surface-2) 0% 25%, transparent 0% 50%) 50% / 16px
      16px;
  }
  .absent {
    padding: var(--space-4);
    border: 1px dashed var(--border);
    color: var(--text-muted);
    text-align: center;
  }
  /* highlight.js token colors from the theme */
  .code :global(.hljs-keyword),
  .code :global(.hljs-selector-tag),
  .code :global(.hljs-built_in) {
    color: var(--hl-keyword);
  }
  .code :global(.hljs-string),
  .code :global(.hljs-regexp),
  .code :global(.hljs-attr) {
    color: var(--hl-string);
  }
  .code :global(.hljs-comment),
  .code :global(.hljs-quote) {
    color: var(--hl-comment);
    font-style: italic;
  }
  .code :global(.hljs-number),
  .code :global(.hljs-literal) {
    color: var(--hl-number);
  }
  .code :global(.hljs-title),
  .code :global(.hljs-function),
  .code :global(.hljs-section) {
    color: var(--hl-title);
  }
  .code :global(.hljs-type),
  .code :global(.hljs-name),
  .code :global(.hljs-tag) {
    color: var(--hl-type);
  }
  .code :global(.hljs-meta),
  .code :global(.hljs-symbol),
  .code :global(.hljs-variable) {
    color: var(--hl-meta);
  }
</style>
