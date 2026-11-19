<!--
  History (DSNA-124): the folder's unsaved changes, then turns, your edits between turns,
  grouped auto-saves, checkpoints and safety copies, newest first. Arrow keys move between
  rows. While "Compare with…" waits, picking a row compares with it.
-->
<script lang="ts">
  import type { HistoryItem } from '../history';
  import { selectionOf } from '../history';
  import { useStore } from '../stores/app.svelte';
  import { absoluteTime, plural } from '../util/format';

  const store = useStore();
  const items = $derived(store.history);
  const comparing = $derived(store.comparePick !== null);
  let list: HTMLElement | undefined = $state();

  function when(ms: number): string {
    const day = (t: number) => new Date(t).toDateString();
    const now = Date.now();
    if (day(ms) === day(now)) {
      return new Date(ms).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
    }
    if (day(ms) === day(now - 86_400_000)) return 'Yesterday';
    return new Date(ms).toLocaleDateString(undefined, {
      weekday: 'short',
      day: 'numeric',
      month: 'short',
    });
  }

  const files = (c: { added: number; modified: number; deleted: number }) =>
    plural(c.added + c.modified + c.deleted, 'file');

  interface Row {
    who: string;
    time: string;
    title: string;
    text: string;
    meta: string;
    dot: string;
  }

  function row(item: HistoryItem): Row {
    switch (item.kind) {
      case 'turn': {
        const t = item.turn;
        const n = files(t.counts);
        const meta =
          t.state === 'running'
            ? 'Working…'
            : t.state === 'pending'
              ? `Waiting for review · ${n}`
              : t.state === 'pruned'
                ? 'Snapshots cleaned up'
                : t.undone > 0 && t.kept > 0
                  ? `${t.undone} undone, ${t.kept} kept`
                  : t.undone > 0
                    ? `Undone · ${n}`
                    : `Kept · ${n}`;
        return {
          who: `Turn ${t.seq}`,
          time: t.state === 'running' ? 'now' : when(item.time),
          title: absoluteTime(t.startedAtMs),
          text: t.prompt ? `“${t.prompt}”` : 'No prompt recorded',
          meta,
          dot: t.state === 'running' ? 'running' : t.state === 'pending' ? 'waiting' : 'turn',
        };
      }
      case 'edits':
        return {
          who: 'You',
          time: when(item.time),
          title: absoluteTime(item.time),
          text: `Edited ${files(item.to.counts)}`,
          meta: 'Between turns',
          dot: 'you',
        };
      case 'autos':
        return {
          who: 'Auto-saves',
          time: when(item.time),
          title: absoluteTime(item.time),
          text: `${item.versions.length} snapshots while you worked`,
          meta: 'Grouped',
          dot: 'auto',
        };
      case 'version': {
        const v = item.version;
        const who = {
          manual: 'Checkpoint',
          auto: 'Auto-save',
          cli: 'dsnap',
          safety: 'Safety copy',
        }[v.kind];
        return {
          who,
          time: when(item.time),
          title: absoluteTime(item.time),
          // Auto-saves are labelled with their timestamp; the time column already shows it.
          text: v.kind === 'auto' ? `Changed ${files(v.counts)}` : v.label,
          meta:
            v.kind === 'auto'
              ? 'Saved automatically'
              : `${v.pinned ? 'Pinned · ' : ''}${files(v.counts)}`,
          dot: v.kind === 'manual' ? 'checkpoint' : v.kind === 'safety' ? 'safety' : 'auto',
        };
      }
    }
  }

  function pick(item: HistoryItem) {
    if (comparing) store.finishCompare(store.versionOfItem(item));
    else store.select(selectionOf(item));
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const rows = [...(list?.querySelectorAll<HTMLElement>('[data-row]') ?? [])];
    const i = rows.indexOf(document.activeElement as HTMLElement);
    const next =
      rows[e.key === 'ArrowDown' ? Math.min(rows.length - 1, i + 1) : Math.max(0, i - 1)];
    if (next) {
      e.preventDefault();
      next.focus();
    }
  }
</script>

<aside class="history" aria-label="History">
  <div class="head">
    <h2>History</h2>
    <span class="count">{plural(items.length, 'entry', 'entries')}</span>
  </div>

  {#if comparing}
    <div class="compare" role="status">
      <span>Pick an entry to compare with.</span>
      <div class="row-actions">
        <button type="button" onclick={() => store.finishCompare(null)}>The folder now</button>
        <button type="button" class="link" onclick={() => store.cancelCompare()}>Cancel</button>
      </div>
    </div>
  {/if}

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="list" bind:this={list} {onkeydown}>
    {#if store.unsaved.length > 0 && !store.project?.missing}
      <button
        type="button"
        data-row
        class="row unsaved"
        aria-current={store.selectedKey === 'unsaved' ? 'true' : undefined}
        onclick={() => (comparing ? store.finishCompare(null) : store.select({ kind: 'unsaved' }))}
      >
        <span class="dot dashed" aria-hidden="true"></span>
        <span class="main">
          <span class="line1"><span class="who">Not snapshotted yet</span></span>
          <span class="meta">{plural(store.unsaved.length, 'file')} changed</span>
        </span>
      </button>
    {/if}

    {#if store.errors.history}
      <p class="note">
        Could not load the history: {store.errors.history}
        <button type="button" class="link" onclick={() => void store.loadHistory()}>Retry</button>
      </p>
    {:else if items.length === 0 && !store.loading.history}
      <p class="note">
        No snapshots yet. Take one, or install the Claude Code hooks (Settings) to record every
        agent turn.
      </p>
    {/if}

    <ol>
      {#each items as item (item.key)}
        {@const r = row(item)}
        <li>
          <button
            type="button"
            data-row
            class="row"
            class:current={store.selectedKey === item.key}
            aria-current={store.selectedKey === item.key ? 'true' : undefined}
            title={r.title}
            onclick={() => pick(item)}
          >
            <span class="dot {r.dot}" aria-hidden="true"></span>
            <span class="main">
              <span class="line1">
                <span class="who">{r.who}</span>
                <span class="time">{r.time}</span>
              </span>
              <span class="text">{r.text}</span>
              <span class="meta">{r.meta}</span>
            </span>
          </button>
        </li>
      {/each}
    </ol>
  </div>

  <div class="foot">
    <button
      type="button"
      class="snap"
      disabled={store.busy !== null || !store.project || store.project.missing}
      onclick={() => (store.snapshotDialog = true)}>Take a snapshot now</button
    >
  </div>
</aside>

<style>
  .history {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--surface-0);
    border-left: 1px solid var(--border);
  }
  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    padding: 24px 24px 10px;
  }
  h2 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .count {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .compare {
    margin: 0 16px 8px;
    padding: 10px 12px;
    border-radius: var(--radius-md);
    background: var(--marker-soft);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row-actions {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 0 12px 12px;
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row {
    display: flex;
    gap: 12px;
    width: 100%;
    padding: 10px 12px;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    text-align: left;
    min-height: 44px;
  }
  .row:hover:not(:disabled) {
    background: var(--surface-1);
  }
  .row.current,
  .row[aria-current='true'] {
    background: var(--surface-2);
  }
  .unsaved {
    margin-bottom: 6px;
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .unsaved[aria-current='true'] {
    box-shadow: inset 0 0 0 1.5px var(--text);
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .line1 {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }
  .who {
    flex: 1;
    font-weight: 600;
  }
  .time {
    font-size: var(--text-sm);
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .text {
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .dot {
    flex: none;
    width: 10px;
    height: 10px;
    margin-top: 5px;
    border-radius: 50%;
    background: var(--border-strong);
  }
  .dot.waiting {
    background: var(--marker);
    box-shadow: 0 0 0 1.5px var(--text);
  }
  .dot.running {
    background: var(--marker);
    box-shadow: 0 0 0 1.5px var(--text);
    animation: pulse 1.4s ease-in-out infinite;
  }
  .dot.turn {
    background: var(--text);
  }
  .dot.you {
    background: transparent;
    border: 2px solid var(--text);
  }
  .dot.checkpoint {
    border-radius: 2px;
    background: var(--text);
    transform: rotate(45deg) scale(0.85);
  }
  .dot.safety {
    border-radius: 2px;
    background: transparent;
    border: 2px solid var(--warn-strong);
  }
  .dot.dashed {
    border-radius: 2px;
    background: transparent;
    border: 2px dashed var(--text);
  }
  @keyframes pulse {
    50% {
      transform: scale(0.7);
    }
  }
  .note {
    margin: 8px 12px;
    color: var(--text-muted);
    font-size: 13px;
  }
  .foot {
    padding: 12px 16px 16px;
    border-top: 1px solid var(--border);
  }
  .snap {
    width: 100%;
    min-height: 44px;
    border-radius: var(--radius-md);
    font-weight: 600;
  }
</style>
