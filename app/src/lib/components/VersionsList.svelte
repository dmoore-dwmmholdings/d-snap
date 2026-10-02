<!--
  Versions list (DSNA-42, F10): the pinned "Unsaved changes" row, then versions newest
  first. Selecting a row shows its changes; "Restore…" restores the project to it (F20).
  Compare mode (DSNA-70, F16): pick two rows (or Ctrl+click one) to compare them.
  Version menu (DSNA-71, F11): edit label (F2), pin, delete (Delete key).
-->
<script lang="ts">
  import type { RestorePlan, Version } from '../api';
  import ConfirmRestoreDialog from '../dialogs/ConfirmRestoreDialog.svelte';
  import Dialog from '../dialogs/Dialog.svelte';
  import { useStore, type Selection } from '../stores/app.svelte';
  import { KIND_ICON, KIND_LABEL, absoluteTime, plural, relativeTime } from '../util/format';
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
  let menu = $state<{ version: Version; x: number; y: number } | null>(null);
  let editing = $state<number | null>(null);
  let editValue = $state('');
  let deleting = $state<Version | null>(null);

  /** -1 = Unsaved changes, else an index into `store.versions`. */
  const active = $derived.by(() => {
    const sel = store.selection;
    if (!sel || sel.kind === 'unsaved') return -1;
    return store.versions.findIndex((v) => v.id === sel.id);
  });
  const now = $derived(store.versions.length >= 0 ? Date.now() : 0);

  /** "A"/"B" marker of a row in compare mode. */
  function compareMark(sel: Selection): 'A' | 'B' | null {
    const same = (x: Selection | null) =>
      !!x &&
      x.kind === sel.kind &&
      (x.kind === 'unsaved' || (sel.kind === 'version' && x.id === sel.id));
    if (same(store.compareA)) return 'A';
    const c = store.compare;
    if (!c) return null;
    if (sel.kind === 'version' && c.from === sel.id) return 'A';
    if (
      c.to.kind === 'workingTree'
        ? sel.kind === 'unsaved'
        : sel.kind === 'version' && c.to.id === sel.id
    )
      return 'B';
    return null;
  }

  function selOf(i: number): Selection {
    return i === -1 ? { kind: 'unsaved' } : { kind: 'version', id: store.versions[i]!.id };
  }

  function selectIndex(i: number) {
    if (i < -1 || i >= store.versions.length) return;
    void store.select(selOf(i));
  }

  function click(i: number, e: MouseEvent) {
    if (store.comparing) {
      void store.pickCompare(selOf(i));
    } else if (e.ctrlKey || e.metaKey) {
      const current = store.selection;
      store.startCompare();
      if (current) void store.pickCompare(current).then(() => store.pickCompare(selOf(i)));
    } else {
      selectIndex(i);
    }
  }

  function onkeydown(e: KeyboardEvent) {
    const k = e.key;
    const v = active >= 0 ? store.versions[active] : undefined;
    if (k === 'ArrowDown') selectIndex(active + 1);
    else if (k === 'ArrowUp') selectIndex(active - 1);
    else if (k === 'Home') selectIndex(-1);
    else if (k === 'End') selectIndex(store.versions.length - 1);
    else if (k === 'PageDown') selectIndex(Math.min(store.versions.length - 1, active + 10));
    else if (k === 'PageUp') selectIndex(Math.max(-1, active - 10));
    else if (k === 'Enter') onenter?.();
    else if (k === 'Backspace' || (k === 'ArrowLeft' && e.altKey)) onback?.();
    else if (k === 'Escape' && store.comparing) void store.exitCompare();
    else if (k === 'F2' && v) startEdit(v);
    else if (k === 'Delete' && v) deleting = v;
    else if ((k === 'ContextMenu' || (k === 'F10' && e.shiftKey)) && v) {
      const r = listbox?.querySelector(`#v-${v.id}`)?.getBoundingClientRect();
      menu = { version: v, x: (r?.left ?? 0) + 24, y: r?.bottom ?? 0 };
    } else return;
    e.preventDefault();
  }

  function startEdit(v: Version) {
    menu = null;
    editing = v.id;
    editValue = v.label;
  }

  function commitEdit(v: Version) {
    if (editing !== v.id) return;
    editing = null;
    void store.setLabel(v.id, editValue);
    listbox?.focus();
  }

  async function askRestore(v: Version) {
    menu = null;
    const plan = await store.restorePlan(v.id);
    if (plan) restoring = { version: v, plan };
  }

  async function doRestore() {
    if (!restoring) return;
    const id = restoring.version.id;
    busy = true;
    const r = await store.restoreProject(id);
    busy = false;
    restoring = null;
    if (r) await store.select({ kind: 'version', id: r.safetyVersion });
  }

  export function focus() {
    listbox?.focus();
  }
</script>

<svelte:window onclick={() => (menu = null)} />

<section class="versions" aria-label="Versions">
  <div class="head">
    <h3 class="pane-title">Versions</h3>
    {#if store.comparing}
      <span class="hint">{store.compareA ? 'Pick the second version' : 'Pick two versions'}</span>
      <button type="button" class="small" onclick={() => void store.exitCompare()}>Done</button>
    {:else}
      <button
        type="button"
        class="small"
        title="Compare any two versions (or Ctrl+click a version)"
        onclick={() => store.startCompare()}>Compare…</button
      >
    {/if}
  </div>
  <div
    class="listbox"
    role="listbox"
    tabindex="0"
    aria-label="Versions"
    aria-activedescendant={active === -1 ? 'v-unsaved' : `v-${store.versions[active]?.id}`}
    bind:this={listbox}
    {onkeydown}
  >
    {#snippet mark(sel: Selection)}
      {@const m = store.comparing || store.compare ? compareMark(sel) : null}
      {#if m}<span class="mark" aria-label="Compare side {m}">{m}</span>{/if}
    {/snippet}

    <div
      id="v-unsaved"
      class="row unsaved"
      role="option"
      tabindex="-1"
      aria-selected={active === -1}
      class:selected={active === -1 && !store.comparing}
      onclick={(e) => click(-1, e)}
      onkeydown={() => {}}
    >
      <span class="icon" aria-hidden="true">✎</span>
      <div class="main">
        <span class="label">Unsaved changes</span>
        <span class="meta">
          {store.unsavedCount > 0 ? `${plural(store.unsavedCount, 'file')} changed` : 'No changes'}
        </span>
      </div>
      {@render mark({ kind: 'unsaved' })}
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
            class:selected={i === active && !store.comparing}
            onclick={(e) => click(i, e)}
            oncontextmenu={(e) => {
              e.preventDefault();
              e.stopPropagation();
              menu = { version: v, x: e.clientX, y: e.clientY };
            }}
            onkeydown={() => {}}
          >
            <span
              class="icon kind-{v.kind}"
              title={KIND_LABEL[v.kind]}
              aria-label={KIND_LABEL[v.kind]}>{KIND_ICON[v.kind]}</span
            >
            <div class="main">
              {#if editing === v.id}
                <!-- svelte-ignore a11y_autofocus -->
                <input
                  class="edit"
                  aria-label="Version label"
                  maxlength="200"
                  bind:value={editValue}
                  autofocus
                  onclick={(e) => e.stopPropagation()}
                  onkeydown={(e) => {
                    e.stopPropagation();
                    if (e.key === 'Enter') commitEdit(v);
                    if (e.key === 'Escape') {
                      editing = null;
                      listbox?.focus();
                    }
                  }}
                  onblur={() => commitEdit(v)}
                />
              {:else}
                <span class="label" title={v.label}>
                  {#if v.pinned}<span class="pin" title="Never pruned by retention">📌</span>{/if}
                  {v.label}
                  {#if v.unstable}<span
                      class="warn"
                      title="Some files kept changing while this snapshot was taken">⚠</span
                    >{/if}
                </span>
              {/if}
              <span class="meta">
                <span title={absoluteTime(v.createdAtMs)}>{relativeTime(v.createdAtMs, now)}</span>
                <span class="counts">
                  <span class="a">+{v.counts.added}</span>
                  <span class="m">~{v.counts.modified}</span>
                  <span class="d">-{v.counts.deleted}</span>
                </span>
              </span>
            </div>
            {@render mark({ kind: 'version', id: v.id })}
            {#if !store.comparing}
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
            {/if}
          </div>
        {/snippet}
      </VirtualList>
    </div>
  </div>
</section>

{#if menu}
  {@const v = menu.version}
  <ul class="menu" role="menu" style:left="{menu.x}px" style:top="{menu.y}px">
    <li role="none">
      <button role="menuitem" type="button" onclick={() => startEdit(v)}>Edit label</button>
    </li>
    <li role="none">
      <button
        role="menuitem"
        type="button"
        onclick={() => {
          const { id, pinned } = v;
          menu = null;
          void store.setPinned(id, !pinned);
        }}>{v.pinned ? 'Unpin' : 'Pin (never prune)'}</button
      >
    </li>
    <li role="none">
      <button role="menuitem" type="button" onclick={() => void askRestore(v)}
        >Restore project to this version…</button
      >
    </li>
    <li role="none">
      <button
        role="menuitem"
        type="button"
        class="danger-text"
        onclick={() => {
          deleting = v;
          menu = null;
        }}>Delete…</button
      >
    </li>
  </ul>
{/if}

{#if restoring}
  <ConfirmRestoreDialog
    plan={restoring.plan}
    version={restoring.version}
    {busy}
    oncancel={() => (restoring = null)}
    onconfirm={() => void doRestore()}
  />
{/if}

{#if deleting}
  {@const v = deleting}
  <Dialog
    title="Delete “{v.label}”?"
    confirmLabel="Delete"
    danger
    oncancel={() => (deleting = null)}
    onconfirm={() => {
      const id = v.id;
      deleting = null;
      void store.deleteVersion(id);
    }}
  >
    <p>The snapshot is removed. Files only it holds are freed on the next cleanup.</p>
    {#if v.kind === 'safety'}
      <p class="warn-text">
        This is a safety snapshot taken before a restore. Deleting it removes the way to undo that
        restore.
      </p>
    {/if}
  </Dialog>
{/if}

<style>
  .versions {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    border-right: 1px solid var(--border);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    border-bottom: 1px solid var(--border);
    padding-right: var(--space-2);
  }
  .head .pane-title {
    flex: 1;
    border: 0;
  }
  .hint {
    font-size: var(--text-xs);
    color: var(--accent);
  }
  .small {
    font-size: var(--text-xs);
    padding: 1px 8px;
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
  .edit {
    min-width: 0;
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
    font-size: var(--text-xs);
  }
  .warn {
    color: var(--status-modified);
  }
  .mark {
    font-weight: 700;
    color: var(--accent-text);
    background: var(--accent);
    border-radius: 50%;
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    font-size: var(--text-xs);
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
  .error,
  .warn-text,
  .danger-text {
    color: var(--status-deleted);
  }
  .menu {
    position: fixed;
    z-index: 40;
    list-style: none;
    margin: 0;
    padding: var(--space-1);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: 0 6px 20px rgb(0 0 0 / 20%);
    min-width: 220px;
  }
  .menu button {
    width: 100%;
    text-align: left;
    border: none;
    background: none;
    padding: 6px var(--space-2);
  }
  .menu button:hover {
    background: var(--surface-2);
  }
</style>
