<!--
  One changed file as a card (DSNA-125/126): status chip, path, +/- counts, the actions the
  view passes in, and the diff (DiffViewer) when opened. The diff loads on first open and
  again when `revision` changes.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { ApiError, type DiffOptions, type FileChange, type FileDiff } from '../api';
  import DiffViewer from '../diff/DiffViewer.svelte';
  import { inlineRows, splitRows } from '../diff/rows';
  import { useStore } from '../stores/app.svelte';
  import { diffPrefs } from '../stores/prefs.svelte';
  import { splitPath } from '../util/format';

  interface Props {
    change: FileChange;
    /** Loads the diff for the current options. */
    loadDiff: (opts: DiffOptions) => Promise<FileDiff>;
    /** Opened by the parent (e.g. the next file to review); the card can also be toggled. */
    open?: boolean;
    ontoggle?: (open: boolean) => void;
    /** Chip text; default from the change status. */
    chip?: string;
    chipClass?: string;
    /** Decided or done: drawn quieter. */
    settled?: boolean;
    /** Line under the header (e.g. "changed since the turn"). */
    note?: string;
    actions?: Snippet;
    /** Per-hunk action on the diff. */
    onHunk?: (index: number, opts: DiffOptions) => void;
    hunkLabel?: string;
  }

  let {
    change,
    loadDiff,
    open = false,
    ontoggle,
    chip,
    chipClass = '',
    settled = false,
    note,
    actions,
    onHunk,
    hunkLabel,
  }: Props = $props();

  const store = useStore();

  const STATUS_CHIP = { added: 'NEW', modified: 'EDITED', deleted: 'DELETED', renamed: 'MOVED' };
  const label = $derived(chip ?? STATUS_CHIP[change.status.kind]);
  const cls = $derived(chipClass || (change.status.kind === 'added' ? 'marker' : ''));
  const parts = $derived(splitPath(change.path));
  const delta = $derived.by(() => {
    const a = change.linesAdded;
    const d = change.linesRemoved;
    if (a === null && d === null) return '';
    return [a ? `+${a}` : '', d ? `−${d}` : ''].filter(Boolean).join(' ') || '±0';
  });

  // Follows `open` from the parent; the card's own toggle overrides it until it changes.
  let expanded = $derived(open);

  function toggle() {
    expanded = !expanded;
    ontoggle?.(expanded);
  }

  let diff = $state<FileDiff | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let seq = 0;

  const opts = $derived<DiffOptions>({
    ignoreWhitespace: diffPrefs.ignoreWhitespace,
    context: diffPrefs.context,
  });

  $effect(() => {
    if (!expanded) return;
    const o = opts;
    void store.revision;
    void change.path;
    const mine = ++seq;
    loading = true;
    error = null;
    loadDiff(o)
      .then((d) => {
        if (mine === seq) diff = d;
      })
      .catch((err: unknown) => {
        if (mine === seq) {
          diff = null;
          error = ApiError.from(err).message;
        }
      })
      .finally(() => {
        if (mine === seq) loading = false;
      });
  });

  /** Room for every row, up to a cap; the viewer scrolls beyond it. */
  const height = $derived.by(() => {
    if (!diff) return 120;
    if (diff.body.kind !== 'text') return diff.body.kind === 'image' ? 220 : 120;
    const hunks = diff.body.hunks;
    const rows = diffPrefs.mode === 'split' ? splitRows(hunks).length : inlineRows(hunks).length;
    return Math.min(560, Math.max(90, rows * 20 + 44));
  });
</script>

<article class="card" class:settled class:open={expanded}>
  <div class="head">
    <span class="chip {cls}">{label}</span>
    <button
      type="button"
      class="path mono"
      aria-expanded={expanded}
      title={change.path}
      onclick={toggle}
    >
      <svg class="caret" width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"
        ><path d="M3 2l4 3-4 3" fill="none" stroke="currentColor" stroke-width="1.6" /></svg
      >
      <span class="name"
        >{#if change.status.kind === 'renamed'}<span class="dir"
            >{change.status.from} →
          </span>{/if}<span class="dir">{parts.dir}</span>{parts.name}</span
      >
    </button>
    {#if delta}<span class="delta mono">{delta}</span>{/if}
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </div>
  {#if note}<p class="note">{note}</p>{/if}
  {#if expanded}
    <div class="body">
      {#if error}
        <p class="msg">Could not load the diff: {error}</p>
      {:else if diff}
        <div class="diff" style:height="{height}px">
          <DiffViewer
            {diff}
            mode={diffPrefs.mode}
            ignoreWhitespace={diffPrefs.ignoreWhitespace}
            onToggleWhitespace={(on) => (diffPrefs.ignoreWhitespace = on)}
            onModeChange={(m) => (diffPrefs.mode = m)}
            onRevertHunk={onHunk ? (i) => onHunk(i, opts) : undefined}
            revertLabel={hunkLabel}
            status={change.status}
            loadImage={(h, m) => store.api.readBlobAsDataUrl(h, m)}
            busy={loading}
          />
        </div>
      {:else}
        <p class="msg">Loading…</p>
      {/if}
    </div>
  {/if}
</article>

<style>
  .card {
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    background: var(--surface-0);
    overflow: hidden;
  }
  .card.settled {
    border-color: var(--border);
    background: var(--surface-1);
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
    padding: 8px 12px 8px 14px;
    min-height: 52px;
  }
  .path {
    flex: 1 1 220px;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    border: 0;
    background: none;
    padding: 4px 0;
    text-align: left;
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path:hover:not(:disabled) {
    background: none;
    text-decoration: underline;
    text-underline-offset: 3px;
  }
  .caret {
    flex: none;
    color: var(--text-muted);
    transition: transform 0.12s;
  }
  .open .caret {
    transform: rotate(90deg);
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .dir {
    color: var(--text-muted);
  }
  .delta {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .note {
    margin: -4px 14px 10px;
    font-size: 13px;
    color: var(--warn-text);
  }
  .body {
    border-top: 1px solid var(--border);
    background: var(--surface-0);
  }
  .diff {
    display: flex;
    flex-direction: column;
  }
  .msg {
    margin: 0;
    padding: 14px;
    color: var(--text-muted);
  }
</style>
