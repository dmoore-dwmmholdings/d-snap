<!--
  Projects sidebar (DSNA-41, F1-F3): project rows with change badges, add, and a context
  menu with rename, show in Explorer, locate (missing) and remove.
-->
<script lang="ts">
  import type { Project, ProjectId } from '../api';
  import { useStore } from '../stores/app.svelte';
  import RemoveProjectDialog from '../dialogs/RemoveProjectDialog.svelte';

  let { onenter }: { onenter?: () => void } = $props();

  const store = useStore();

  let menu = $state<{ id: ProjectId; x: number; y: number } | null>(null);
  let renaming = $state<ProjectId | null>(null);
  let renameValue = $state('');
  let removing = $state<Project | null>(null);
  let list: HTMLUListElement | undefined = $state();

  function openMenu(e: MouseEvent | KeyboardEvent, id: ProjectId) {
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const pos =
      e instanceof MouseEvent && e.clientX
        ? { x: e.clientX, y: e.clientY }
        : { x: r.left + 16, y: r.bottom };
    menu = { id, ...pos };
  }

  function startRename(p: Project) {
    menu = null;
    renaming = p.id;
    renameValue = p.name;
  }

  async function commitRename(id: ProjectId) {
    const name = renameValue.trim();
    const p = store.projects.find((x) => x.id === id);
    renaming = null;
    if (p && name && name !== p.name) await store.renameProject(id, name);
  }

  function focusRow(id: ProjectId) {
    list?.querySelector<HTMLElement>(`[data-project="${id}"]`)?.focus();
  }

  function onkeydown(e: KeyboardEvent, p: Project) {
    const i = store.projects.findIndex((x) => x.id === p.id);
    const go = (j: number) => {
      const next = store.projects[j];
      if (!next) return;
      e.preventDefault();
      void store.selectProject(next.id);
      focusRow(next.id);
    };
    if (e.key === 'ArrowDown') go(i + 1);
    else if (e.key === 'ArrowUp') go(i - 1);
    else if (e.key === 'Home') go(0);
    else if (e.key === 'End') go(store.projects.length - 1);
    else if (e.key === 'Enter') {
      e.preventDefault();
      void store.selectProject(p.id).then(() => onenter?.());
    } else if (e.key === 'F2') {
      e.preventDefault();
      startRename(p);
    } else if (e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey)) openMenu(e, p.id);
  }

  const menuProject = $derived(menu ? store.projects.find((p) => p.id === menu!.id) : undefined);
</script>

<svelte:window
  onclick={() => (menu = null)}
  onkeydown={(e) => e.key === 'Escape' && (menu = null)}
/>

<nav class="sidebar" aria-label="Projects">
  <h1 class="brand">D-Snap</h1>
  <ul class="projects" role="listbox" aria-label="Projects" bind:this={list}>
    {#each store.projects as p (p.id)}
      {@const count = store.counts[p.id] ?? 0}
      <li
        role="option"
        aria-selected={store.projectId === p.id}
        data-project={p.id}
        tabindex={store.projectId === p.id || (store.projectId === null && p === store.projects[0])
          ? 0
          : -1}
        class:selected={store.projectId === p.id}
        class:missing={p.missing}
        title={p.missing ? `Folder not found: ${p.root}` : p.root}
        onclick={() => void store.selectProject(p.id)}
        oncontextmenu={(e) => openMenu(e, p.id)}
        onkeydown={(e) => onkeydown(e, p)}
      >
        {#if renaming === p.id}
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="rename"
            aria-label="Project name"
            bind:value={renameValue}
            autofocus
            onclick={(e) => e.stopPropagation()}
            onkeydown={(e) => {
              e.stopPropagation();
              if (e.key === 'Enter') void commitRename(p.id);
              if (e.key === 'Escape') renaming = null;
            }}
            onblur={() => void commitRename(p.id)}
          />
        {:else}
          <span class="name">{p.name}</span>
          {#if p.missing}
            <span class="tag" aria-label="folder missing">missing</span>
          {:else if count > 0}
            <span class="badge" aria-label="{count} changed">{count}</span>
          {/if}
        {/if}
      </li>
    {:else}
      <li class="empty">No projects yet.</li>
    {/each}
  </ul>

  <div class="footer">
    <button type="button" class="add" onclick={() => void store.addProject()}>+ Add project</button>
    <button
      type="button"
      class="gear"
      aria-label="Settings"
      title="Settings"
      class:active={store.view === 'settings'}
      onclick={() => store.setView(store.view === 'settings' ? 'main' : 'settings')}>⚙</button
    >
  </div>
</nav>

{#if menu && menuProject}
  {@const p = menuProject}
  <ul class="menu" role="menu" style:left="{menu.x}px" style:top="{menu.y}px">
    <li role="none">
      <button role="menuitem" type="button" onclick={() => startRename(p)}>Rename</button>
    </li>
    <li role="none">
      <button
        role="menuitem"
        type="button"
        disabled={p.missing}
        onclick={() => void store.revealProject(p.id)}>Show in Explorer</button
      >
    </li>
    {#if p.missing}
      <li role="none">
        <button role="menuitem" type="button" onclick={() => void store.relocateProject(p.id)}
          >Locate folder…</button
        >
      </li>
    {/if}
    <li role="none">
      <button
        role="menuitem"
        type="button"
        class="danger-text"
        onclick={() => {
          removing = p;
          menu = null;
        }}>Remove…</button
      >
    </li>
  </ul>
{/if}

{#if removing}
  {@const p = removing}
  <RemoveProjectDialog
    project={p}
    oncancel={() => (removing = null)}
    onconfirm={(alsoDelete) => {
      const id = p.id;
      removing = null;
      void store.removeProject(id, alsoDelete);
    }}
  />
{/if}

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--surface-1);
    border-right: 1px solid var(--border);
    min-width: 0;
  }
  .brand {
    font-size: var(--text-lg);
    font-weight: 600;
    margin: 0;
    padding: var(--space-3);
  }
  .projects {
    list-style: none;
    margin: 0;
    padding: 0 var(--space-2);
    flex: 1;
    overflow: auto;
  }
  .projects li {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 6px var(--space-2);
    border-radius: var(--radius-sm);
    cursor: default;
    user-select: none;
  }
  .projects li:hover {
    background: var(--surface-2);
  }
  .projects li.selected {
    background: var(--accent);
    color: var(--accent-text);
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .missing .name {
    color: var(--text-muted);
    text-decoration: line-through;
  }
  .selected.missing .name {
    color: inherit;
  }
  .badge {
    font-size: var(--text-xs);
    min-width: 18px;
    padding: 0 6px;
    border-radius: 9px;
    text-align: center;
    background: var(--status-modified);
    color: #fff;
  }
  .tag {
    font-size: var(--text-xs);
    color: var(--status-deleted);
  }
  .selected .tag {
    color: inherit;
  }
  .empty {
    color: var(--text-muted);
  }
  .rename {
    flex: 1;
    min-width: 0;
  }
  .footer {
    display: flex;
    gap: var(--space-2);
    padding: var(--space-2);
    border-top: 1px solid var(--border);
  }
  .add {
    flex: 1;
  }
  .gear.active {
    background: var(--surface-2);
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
    min-width: 180px;
  }
  .menu button {
    width: 100%;
    text-align: left;
    border: none;
    background: none;
    padding: 6px var(--space-2);
    border-radius: var(--radius-sm);
  }
  .menu button:hover:not(:disabled) {
    background: var(--surface-2);
  }
  .danger-text {
    color: var(--status-deleted);
  }
</style>
