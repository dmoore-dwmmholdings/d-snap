<!--
  Versions list (DSNA-42, F10): the pinned "Unsaved changes" row, then versions newest
  first. Selecting a row shows its changes; "Restore…" restores the project to it (F20).
-->
<script lang="ts">
  import type { RestorePlan, Version } from '../api';
  import { useStore } from '../stores/app.svelte';
  import { KIND_ICON, KIND_LABEL, absoluteTime, plural, relativeTime } from '../util/format';
  import ConfirmRestoreDialog from '../dialogs/ConfirmRestoreDialog.svelte';
  import VirtualList from './VirtualList.svelte';

  interface Props {
    onenter?: () => void;
    onback?: () => void;
  }
  let { onenter, onback }: Props = $props();

  const store = useStore();
  const ROW = 52;

  let listbox: HTMLDivElement | undefined = $state();
  let restoring = $state<{ version: Version; plan: RestorePlan } | null>(null);
  let busy = $state(false);

  /** -1 = Unsaved changes, else an index into `store.versions`. */
  const active = $derived.by(() => {
    const sel = store.selection;
    if (!sel || sel.kind === 'unsaved') return -1;
    return store.versions.findIndex((v) => v.id === sel.id);
  });
  const now = $derived(store.versions.length >= 0 ? Date.now() : 0);

  function selectIndex(i: number) {
    if (i < -1 || i >= store.versions.length) return;
    void store.select(
      i === -1 ? { kind: 'unsaved' } : { kind: 'version', id: store.versions[i]!.id },
    );
  }

  function onkeydown(e: KeyboardEvent) {
    const k = e.key;
    if (k === 'ArrowDown') selectIndex(active + 1);
    else if (k === 'ArrowUp') selectIndex(active - 1);
    else if (k === 'Home') selectIndex(-1);
    else if (k === 'End') selectIndex(store.versions.length - 1);
    else if (k === 'PageDown') selectIndex(Math.min(store.versions.length - 1, active + 10));
    else if (k === 'PageUp') selectIndex(Math.max(-1, active - 10));
    else if (k === 'Enter') onenter?.();
    else if (k === 'Backspace' || (k === 'ArrowLeft' && e.altKey)) onback?.();
    else return;
    e.preventDefault();
  }

  async function askRestore(v: Version) {
    const plan = await store.restorePlan(v.id);
    if (plan) restoring = { version: v, plan };
  }

  async function doRestore() {
    if (!restoring) return;
    busy = true;
    const r = await store.restoreProject(restoring.version.id);
    busy = false;
    restoring = null;
    if (r) await store.select({ kind: 'version', id: r.safetyVersion });
  }

  export function focus() {
    listbox?.focus();
  }
</script>

<section class="versions" aria-label="Versions">
  <h3 class="pane-title">Versions</h3>
  <div
    class="listbox"
    role="listbox"
    tabindex="0"
    aria-label="Versions"
    aria-activedescendant={active === -1 ? 'v-unsaved' : `v-${store.versions[active]?.id}`}
    bind:this={listbox}
    {onkeydown}
  >
    <div
      id="v-unsaved"
      class="row unsaved"
      role="option"
      tabindex="-1"
      aria-selected={active === -1}
      class:selected={active === -1}
      onclick={() => selectIndex(-1)}
      onkeydown={() => {}}
    >
      <span class="icon" aria-hidden="true">✎</span>
      <div class="main">
        <span class="label">Unsaved changes</span>
        <span class="meta">
          {store.unsavedCount > 0 ? `${plural(store.unsavedCount, 'file')} changed` : 'No changes'}
        </span>
      </div>
    </div>

    {#if store.errors.versions}
      <p class="note error">
        Could not load versions: {store.errors.versions}
        <button type="button" onclick={() => void store.loadVersions()}>Retry</button>
      </p>
    {:else if store.versions.length === 0 && !store.loading.versions}
      <p class="note">No snapshots yet. Press Snapshot to take the first one.</p>
    {/if}

    <div class="scroll">
      <VirtualList items={store.versions} rowHeight={ROW} activeIndex={active} label="Snapshots">
        {#snippet row(v: Version, i: number)}
          <div
            id="v-{v.id}"
            class="row"
            role="option"
            tabindex="-1"
            aria-selected={i === active}
            class:selected={i === active}
            onclick={() => selectIndex(i)}
            onkeydown={() => {}}
          >
            <span
              class="icon kind-{v.kind}"
              title={KIND_LABEL[v.kind]}
              aria-label={KIND_LABEL[v.kind]}>{KIND_ICON[v.kind]}</span
            >
            <div class="main">
              <span class="label" title={v.label}>
                {#if v.pinned}<span class="pin" title="Pinned: never removed by retention">★</span
                  >{/if}
                {v.label}
                {#if v.unstable}<span
                    class="warn"
                    title="Some files kept changing while this snapshot was taken">⚠</span
                  >{/if}
              </span>
              <span class="meta">
                <span title={absoluteTime(v.createdAtMs)}>{relativeTime(v.createdAtMs, now)}</span>
                <span class="counts">
                  <span class="a">+{v.counts.added}</span>
                  <span class="m">~{v.counts.modified}</span>
                  <span class="d">-{v.counts.deleted}</span>
                </span>
              </span>
            </div>
            <button
              type="button"
              class="restore"
              aria-label="Restore project to {v.label}"
              title="Restore project to this version"
              onclick={(e) => {
                e.stopPropagation();
                void askRestore(v);
              }}>Restore…</button
            >
          </div>
        {/snippet}
      </VirtualList>
    </div>
  </div>
</section>

{#if restoring}
  <ConfirmRestoreDialog
    plan={restoring.plan}
    version={restoring.version}
    {busy}
    oncancel={() => (restoring = null)}
    onconfirm={() => void doRestore()}
  />
{/if}

<style>
  .versions {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    border-right: 1px solid var(--border);
  }
  .listbox {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    outline-offset: -2px;
  }
  .scroll {
    flex: 1;
    min-height: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 52px;
    padding: 0 var(--space-3);
    border-bottom: 1px solid var(--border);
    cursor: default;
  }
  .row:hover {
    background: var(--surface-1);
  }
  .row.selected {
    background: var(--surface-2);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .unsaved {
    flex: none;
    background: var(--surface-1);
  }
  .icon {
    width: 20px;
    text-align: center;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .kind-safety {
    color: var(--status-renamed);
  }
  .kind-auto {
    color: var(--accent);
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }
  .meta {
    display: flex;
    gap: var(--space-2);
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .counts {
    display: flex;
    gap: 6px;
  }
  .a {
    color: var(--status-added);
  }
  .m {
    color: var(--status-modified);
  }
  .d {
    color: var(--status-deleted);
  }
  .pin {
    color: var(--status-modified);
  }
  .warn {
    color: var(--status-modified);
  }
  .restore {
    visibility: hidden;
    font-size: var(--text-sm);
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
