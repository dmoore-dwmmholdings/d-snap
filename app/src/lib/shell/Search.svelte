<!--
  "Find any file, at any point in time" (DSNA-127): searches the folder's known paths and
  opens a file's history. Enter opens the first match, Escape clears.
-->
<script lang="ts">
  import type { RelPath } from '../api';
  import { useStore } from '../stores/app.svelte';

  const store = useStore();
  let q = $state('');
  let paths = $state<RelPath[]>([]);
  let loadedFor = '';
  let active = $state(0);
  let input: HTMLInputElement | undefined = $state();

  async function ensure() {
    const id = store.projectId;
    const key = `${id}:${store.revision}`;
    if (id === null || key === loadedFor) return;
    loadedFor = key;
    try {
      paths = await store.api.knownPaths(id);
    } catch {
      paths = [];
    }
  }

  const results = $derived.by(() => {
    const needle = q.trim().toLowerCase();
    if (!needle) return [];
    // File-name matches first, then path matches.
    const named = paths.filter((p) => (p.split('/').pop() ?? p).toLowerCase().includes(needle));
    const rest = paths.filter((p) => !named.includes(p) && p.toLowerCase().includes(needle));
    return [...named, ...rest].slice(0, 8);
  });

  $effect(() => {
    void results;
    active = 0;
  });

  function pick(path: RelPath) {
    q = '';
    store.select({ kind: 'file', path });
    input?.blur();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      q = '';
    } else if (e.key === 'ArrowDown' && results.length) {
      e.preventDefault();
      active = (active + 1) % results.length;
    } else if (e.key === 'ArrowUp' && results.length) {
      e.preventDefault();
      active = (active - 1 + results.length) % results.length;
    } else if (e.key === 'Enter' && results[active]) {
      e.preventDefault();
      pick(results[active]!);
    }
  }
</script>

<div class="search">
  <label class="box">
    <svg width="15" height="15" viewBox="0 0 24 24" aria-hidden="true"
      ><circle cx="11" cy="11" r="7" fill="none" stroke="currentColor" stroke-width="2" /><path
        d="M20 20l-3.5-3.5"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
      /></svg
    >
    <span class="sr-only">Find a file</span>
    <input
      bind:this={input}
      type="search"
      autocomplete="off"
      spellcheck="false"
      placeholder="Find any file, at any point in time"
      role="combobox"
      aria-expanded={results.length > 0}
      aria-controls="search-results"
      aria-activedescendant={results.length ? `sr-${active}` : undefined}
      bind:value={q}
      onfocus={() => void ensure()}
      oninput={() => void ensure()}
      {onkeydown}
    />
  </label>
  {#if q.trim()}
    <ul id="search-results" class="results" role="listbox" aria-label="Files">
      {#each results as p, i (p)}
        {@const slash = p.lastIndexOf('/')}
        <li
          id="sr-{i}"
          role="option"
          aria-selected={i === active}
          class:active={i === active}
          onmousedown={(e) => {
            e.preventDefault();
            pick(p);
          }}
        >
          <span class="name mono">{p.slice(slash + 1)}</span>
          <span class="dir mono">{slash > 0 ? p.slice(0, slash) : ''}</span>
        </li>
      {:else}
        <li class="none">No file in {store.project?.name} matches “{q.trim()}”.</li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .search {
    position: relative;
    width: 100%;
    max-width: 360px;
  }
  .box {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 38px;
    padding: 0 12px;
    border-radius: var(--radius-md);
    background: var(--surface-1);
    color: var(--text-muted);
  }
  .box:focus-within {
    outline: 2px solid var(--focus);
    outline-offset: 1px;
  }
  input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: transparent;
    padding: 0;
    min-height: 36px;
    color: var(--text);
    outline: none;
  }
  .results {
    position: absolute;
    top: 44px;
    left: 0;
    right: 0;
    margin: 0;
    padding: 6px;
    list-style: none;
    background: var(--surface-0);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-pop);
    z-index: 20;
  }
  li {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-height: 38px;
    padding: 8px 10px;
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  li.active {
    background: var(--surface-1);
  }
  .name {
    font-size: 13px;
  }
  .dir {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .none {
    color: var(--text-muted);
    cursor: default;
  }
</style>
