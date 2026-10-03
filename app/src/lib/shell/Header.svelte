<!--
  App header (DSNA-124): wordmark, folder chips (a lime dot when a turn waits for review,
  "missing" when the folder is gone), add folder (F1), the folder menu (F2: rename, show,
  locate, remove), "Find any file" and Settings.
-->
<script lang="ts">
  import type { Project } from '../api';
  import Dialog from '../dialogs/Dialog.svelte';
  import RemoveProjectDialog from '../dialogs/RemoveProjectDialog.svelte';
  import { useStore } from '../stores/app.svelte';
  import Search from './Search.svelte';

  const store = useStore();

  const platform = globalThis.navigator?.userAgent ?? '';
  const revealLabel = /Mac/.test(platform)
    ? 'Show in Finder'
    : /Win/.test(platform)
      ? 'Show in Explorer'
      : 'Show in file manager';

  let menu = $state<{ project: Project; x: number; y: number } | null>(null);
  let renaming = $state<Project | null>(null);
  let newName = $state('');
  let removing = $state<Project | null>(null);
  let menuEl: HTMLUListElement | undefined = $state();

  function openMenu(project: Project, x: number, y: number) {
    menu = { project, x, y };
    queueMicrotask(() => menuEl?.querySelector<HTMLElement>('button')?.focus());
  }

  function menuKey(e: KeyboardEvent) {
    const items = [...(menuEl?.querySelectorAll<HTMLElement>('button:not(:disabled)') ?? [])];
    const i = items.indexOf(document.activeElement as HTMLElement);
    if (e.key === 'Escape') {
      menu = null;
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      items[(i + 1) % items.length]?.focus();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      items[(i - 1 + items.length) % items.length]?.focus();
    }
  }

  function run(fn: () => void) {
    menu = null;
    fn();
  }
</script>

<svelte:window onclick={() => (menu = null)} />

<header class="header">
  <span class="brand">d-snap</span>

  <nav class="folders" aria-label="Folders">
    {#each store.projects as p (p.id)}
      {@const active = p.id === store.projectId && store.view === 'main'}
      <button
        type="button"
        class="folder"
        class:active
        aria-pressed={active}
        title={p.missing ? `Folder not found: ${p.root}` : p.root}
        onclick={() => void store.selectProject(p.id)}
        oncontextmenu={(e) => {
          e.preventDefault();
          e.stopPropagation();
          openMenu(p, e.clientX, e.clientY);
        }}
      >
        {p.name}
        {#if p.missing}
          <span class="missing">missing</span>
        {:else if (store.waiting[p.id] ?? 0) > 0}
          <span class="dot" aria-label="{store.waiting[p.id]} waiting for review"></span>
        {/if}
      </button>
    {/each}
    {#if store.project}
      <button
        type="button"
        class="icon"
        aria-label="Folder options for {store.project.name}"
        aria-haspopup="menu"
        onclick={(e) => {
          e.stopPropagation();
          const r = e.currentTarget.getBoundingClientRect();
          openMenu(store.project!, r.left, r.bottom + 4);
        }}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true"
          ><circle cx="5" cy="12" r="1.8" fill="currentColor" /><circle
            cx="12"
            cy="12"
            r="1.8"
            fill="currentColor"
          /><circle cx="19" cy="12" r="1.8" fill="currentColor" /></svg
        >
      </button>
    {/if}
    <button
      type="button"
      class="icon"
      aria-label="Add a folder"
      title="Add a folder"
      onclick={() => void store.addProject()}
    >
      <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true"
        ><path
          d="M12 5v14M5 12h14"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
        /></svg
      >
    </button>
  </nav>

  <div class="right">
    {#if store.project && !store.project.missing}
      <Search />
    {/if}
    <button
      type="button"
      class="icon"
      class:on={store.view === 'settings'}
      aria-label="Settings"
      aria-pressed={store.view === 'settings'}
      title="Settings"
      onclick={() => store.setView(store.view === 'settings' ? 'main' : 'settings')}
    >
      <svg width="17" height="17" viewBox="0 0 24 24" fill="none" aria-hidden="true"
        ><path
          d="M4 7h10M18 7h2M4 12h4M12 12h8M4 17h12M20 17h0"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
        /><circle cx="16" cy="7" r="2" stroke="currentColor" stroke-width="2" /><circle
          cx="10"
          cy="12"
          r="2"
          stroke="currentColor"
          stroke-width="2"
        /><circle cx="18" cy="17" r="2" stroke="currentColor" stroke-width="2" /></svg
      >
    </button>
  </div>
</header>

{#if menu}
  {@const p = menu.project}
  <ul
    class="menu"
    role="menu"
    tabindex="-1"
    aria-label="{p.name} options"
    style:left="{menu.x}px"
    style:top="{menu.y}px"
    bind:this={menuEl}
    onkeydown={menuKey}
    onclick={(e) => e.stopPropagation()}
  >
    <li role="none">
      <button
        role="menuitem"
        type="button"
        onclick={() =>
          run(() => {
            newName = p.name;
            renaming = p;
          })}>Rename…</button
      >
    </li>
    <li role="none">
      <button
        role="menuitem"
        type="button"
        disabled={p.missing}
        onclick={() => run(() => void store.revealProject(p.id))}>{revealLabel}</button
      >
    </li>
    {#if p.missing}
      <li role="none">
        <button
          role="menuitem"
          type="button"
          onclick={() => run(() => void store.relocateProject(p.id))}>Locate folder…</button
        >
      </li>
    {/if}
    <li role="none">
      <button role="menuitem" type="button" onclick={() => run(() => (removing = p))}
        >Stop watching…</button
      >
    </li>
  </ul>
{/if}

{#if renaming}
  {@const p = renaming}
  <Dialog
    title="Rename {p.name}"
    confirmLabel="Rename"
    busy={!newName.trim()}
    oncancel={() => (renaming = null)}
    onconfirm={async () => {
      if (await store.renameProject(p.id, newName)) renaming = null;
    }}
  >
    <label class="field">
      <span>Name in d-snap (the folder itself is not renamed)</span>
      <input type="text" maxlength="200" bind:value={newName} />
    </label>
  </Dialog>
{/if}

{#if removing}
  {@const p = removing}
  <RemoveProjectDialog
    project={p}
    oncancel={() => (removing = null)}
    onconfirm={(deleteSnapshots) => {
      removing = null;
      void store.removeProject(p.id, deleteSnapshots);
    }}
  />
{/if}

<style>
  .header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 20px;
    padding: 10px 24px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-0);
    position: relative;
    z-index: 5;
  }
  .brand {
    font-size: 20px;
    font-weight: 700;
    letter-spacing: -0.03em;
    font-variation-settings: 'opsz' 48;
  }
  .folders {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .folder {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 36px;
    padding: 0 14px;
    border-radius: 18px;
    border: 1px solid var(--border);
    background: var(--surface-0);
    font-weight: 600;
  }
  .folder.active {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-text);
  }
  .folder.active:hover {
    background: color-mix(in srgb, var(--accent) 85%, var(--bg));
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--marker);
    box-shadow: 0 0 0 1.5px var(--text);
  }
  .folder.active .dot {
    box-shadow: 0 0 0 1.5px var(--accent-text);
  }
  .missing {
    font-size: var(--text-xs);
    font-weight: 500;
    opacity: 0.8;
  }
  .icon {
    display: inline-grid;
    place-items: center;
    width: 36px;
    height: 36px;
    min-height: 36px;
    padding: 0;
    border-radius: 18px;
    border: 1px solid var(--border);
    background: var(--surface-0);
    color: var(--text);
  }
  .icon.on {
    background: var(--accent);
    color: var(--accent-text);
    border-color: var(--accent);
  }
  .right {
    flex: 1 1 260px;
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 10px;
  }
  .menu {
    position: fixed;
    z-index: 60;
    margin: 0;
    padding: 6px;
    list-style: none;
    min-width: 200px;
    background: var(--surface-0);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-pop);
  }
  .menu button {
    width: 100%;
    text-align: left;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    min-height: 36px;
  }
  .menu button:hover:not(:disabled),
  .menu button:focus-visible {
    background: var(--surface-1);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
</style>
