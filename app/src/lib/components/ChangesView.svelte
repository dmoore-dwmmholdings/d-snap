<!--
  Changes view (DSNA-43, F13, F19): changed files with status, +/- counts and a filter,
  beside the diff of the selected file. "Restore" brings one file back.
-->
<script lang="ts">
  import type { FileChange, RelPath, Version, VersionId } from '../api';
  import { useStore } from '../stores/app.svelte';
  import { plural, splitPath, statusLetter } from '../util/format';
  import ConfirmRestoreDialog from '../dialogs/ConfirmRestoreDialog.svelte';
  import DiffPane from './DiffPane.svelte';
  import VirtualList from './VirtualList.svelte';

  let { onback }: { onback?: () => void } = $props();

  const store = useStore();
  const ROW = 28;

  let filter = $state('');
  let listbox: HTMLDivElement | undefined = $state();
  let confirming = $state<{ path: RelPath; version: Version } | null>(null);
  let busy = $state(false);

  const shown = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return store.changes;
    return store.changes.filter(
      (c) =>
        c.path.toLowerCase().includes(q) ||
        (c.status.kind === 'renamed' && c.status.from.toLowerCase().includes(q)),
    );
  });
  const active = $derived(shown.findIndex((c) => c.path === store.path));

  /** The version to restore `c` from, and the path in it; `null` when not restorable. */
  function restoreTarget(c: FileChange): { version: Version; path: RelPath } | null {
    const sides = store.sides;
    if (!sides) return null;
    let id: VersionId | null;
    let path: RelPath;
    if (sides.to.kind === 'workingTree') {
      // Discard the change: bring back the old side.
      if (!c.old) return null;
      id = sides.from ?? store.versions[0]?.id ?? null;
      path = c.old.path;
    } else {
      // Bring the file back as it is in the selected version.
      if (!c.new) return null;
      id = sides.to.id;
      path = c.path;
    }
    const version = store.versions.find((v) => v.id === id);
    return version ? { version, path } : null;
  }

  function select(i: number) {
    const c = shown[i];
    if (c) store.selectFile(c.path);
  }

  function onkeydown(e: KeyboardEvent) {
    const k = e.key;
    if (k === 'ArrowDown') select(active + 1);
    else if (k === 'ArrowUp') select(Math.max(0, active - 1));
    else if (k === 'Home') select(0);
    else if (k === 'End') select(shown.length - 1);
    else if (k === 'Backspace' || (k === 'ArrowLeft' && e.altKey)) onback?.();
    else return;
    e.preventDefault();
  }

  async function doRestore() {
    if (!confirming) return;
    busy = true;
    await store.restoreFile(confirming.version.id, confirming.path);
    busy = false;
    confirming = null;
  }

  export function focus() {
    listbox?.focus();
    if (active < 0) select(0);
  }

  const title = $derived.by(() => {
    const s = store.sides;
    if (!s) return '';
    const label = (id: VersionId | null) =>
      id === null ? null : (store.versions.find((v) => v.id === id)?.label ?? `#${id}`);
    if (s.to.kind === 'workingTree') {
      return `Folder vs ${label(s.from) ?? store.versions[0]?.label ?? 'nothing'}`;
    }
    const i = store.versions.findIndex((v) => v.id === (s.to as { id: VersionId }).id);
    const prev = s.from !== null ? label(s.from) : (store.versions[i + 1]?.label ?? null);
    return `${label(s.to.id)} vs ${prev ?? 'nothing (first snapshot)'}`;
  });
</script>

<section class="changes" aria-label="Changes">
  <div class="files">
    <div class="toolbar">
      <h3 class="pane-title" {title}>{title}</h3>
      <input
        type="search"
        placeholder="Filter files"
        aria-label="Filter files"
        bind:value={filter}
      />
      <span class="count" aria-live="polite">
        {filter
          ? `${shown.length} of ${plural(store.changes.length, 'file')}`
          : plural(store.changes.length, 'file')}
      </span>
    </div>

    {#if store.project?.missing && store.sides?.to.kind === 'workingTree'}
      <p class="note">The project folder is missing. Pick a version to see what it holds.</p>
    {:else if store.errors.changes}
      <p class="note error">
        Could not load changes: {store.errors.changes}
        <button type="button" onclick={() => void store.loadChanges()}>Retry</button>
      </p>
    {:else if store.loading.changes && store.changes.length === 0}
      <p class="note">Loading…</p>
    {:else if store.changes.length === 0}
      <p class="note">
        {store.sides?.to.kind === 'workingTree'
          ? 'No unsaved changes.'
          : 'No changes in this version.'}
      </p>
    {:else}
      <div
        class="listbox"
        role="listbox"
        tabindex="0"
        aria-label="Changed files"
        aria-activedescendant={active >= 0 ? `f-${active}` : undefined}
        bind:this={listbox}
        {onkeydown}
      >
        <VirtualList items={shown} rowHeight={ROW} activeIndex={active} label="Changed files">
          {#snippet row(c: FileChange, i: number)}
            {@const s = statusLetter(c.status)}
            {@const p = splitPath(c.path)}
            {@const target = restoreTarget(c)}
            <div
              id="f-{i}"
              class="row"
              role="option"
              tabindex="-1"
              aria-selected={i === active}
              class:selected={i === active}
              onclick={() => store.selectFile(c.path)}
              onkeydown={() => {}}
            >
              <span class="status s-{s}" aria-label={c.status.kind}>{s}</span>
              <span class="path" title={c.path}>
                {#if c.status.kind === 'renamed'}<span class="dir"
                    >{c.status.from} →
                  </span>{/if}<span class="dir">{p.dir}</span>{p.name}
              </span>
              {#if c.linesAdded !== null || c.linesRemoved !== null}
                <span class="lines">
                  <span class="a">+{c.linesAdded ?? 0}</span>
                  <span class="d">-{c.linesRemoved ?? 0}</span>
                </span>
              {/if}
              {#if target}
                <button
                  type="button"
                  class="restore"
                  aria-label="Restore {target.path}"
                  title="Restore this file to {target.version.label}"
                  onclick={(e) => {
                    e.stopPropagation();
                    confirming = target;
                  }}>Restore</button
                >
              {/if}
            </div>
          {/snippet}
        </VirtualList>
      </div>
    {/if}
  </div>

  <DiffPane
    onrestore={(c: FileChange) => {
      const t = restoreTarget(c);
      if (t) confirming = t;
    }}
    canRestore={(c: FileChange) => restoreTarget(c) !== null}
  />
</section>

{#if confirming}
  <ConfirmRestoreDialog
    version={confirming.version}
    file={confirming.path}
    {busy}
    oncancel={() => (confirming = null)}
    onconfirm={() => void doRestore()}
  />
{/if}

<style>
  .changes {
    display: grid;
    grid-template-columns: minmax(240px, 34%) 1fr;
    min-height: 0;
    height: 100%;
  }
  .files {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--border);
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--border);
  }
  .toolbar .pane-title {
    flex-basis: 100%;
    padding: 0;
    border: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .toolbar input {
    flex: 1;
    min-width: 0;
  }
  .count {
    color: var(--text-muted);
    font-size: var(--text-sm);
    white-space: nowrap;
  }
  .listbox {
    flex: 1;
    min-height: 0;
    outline-offset: -2px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 28px;
    padding: 0 var(--space-3);
    cursor: default;
    white-space: nowrap;
  }
  .row:hover {
    background: var(--surface-1);
  }
  .row.selected {
    background: var(--surface-2);
  }
  .status {
    width: 16px;
    font-weight: 700;
    font-family: var(--font-mono);
    text-align: center;
  }
  .s-A {
    color: var(--status-added);
  }
  .s-M {
    color: var(--status-modified);
  }
  .s-D {
    color: var(--status-deleted);
  }
  .s-R {
    color: var(--status-renamed);
  }
  .path {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    direction: ltr;
  }
  .dir {
    color: var(--text-muted);
  }
  .lines {
    display: flex;
    gap: 4px;
    font-size: var(--text-sm);
    font-family: var(--font-mono);
  }
  .a {
    color: var(--status-added);
  }
  .d {
    color: var(--status-deleted);
  }
  .restore {
    visibility: hidden;
    font-size: var(--text-xs);
    padding: 1px 6px;
  }
  .row:hover .restore,
  .row.selected .restore,
  .restore:focus-visible {
    visibility: visible;
  }
  .note {
    padding: var(--space-3);
    color: var(--text-muted);
    margin: 0;
  }
  .error {
    color: var(--status-deleted);
  }
</style>
