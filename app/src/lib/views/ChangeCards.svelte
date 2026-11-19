<!--
  The changed files between two sides as cards (DSNA-126): loads `changes(from, to)`, opens
  the first card, and lets the view add per-file actions and a per-hunk action.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import {
    ApiError,
    type DiffOptions,
    type FileChange,
    type VersionId,
    type VersionRef,
  } from '../api';
  import { useStore } from '../stores/app.svelte';
  import FileCard from './FileCard.svelte';

  interface Props {
    from: VersionId | null;
    to: VersionRef;
    /** Text when nothing changed. */
    empty?: string;
    actions?: Snippet<[FileChange]>;
    onHunk?: (c: FileChange, index: number, opts: DiffOptions) => void;
    hunkLabel?: string;
  }
  let { from, to, empty = 'No changes.', actions: actionsOf, onHunk, hunkLabel }: Props = $props();

  const store = useStore();
  let changes = $state<FileChange[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let open = $state<Record<string, boolean>>({});
  let seq = 0;
  let lastKey = '';

  $effect(() => {
    const id = store.projectId;
    const f = from;
    const t = to;
    void store.revision;
    if (id === null) return;
    const key = `${id}:${f}:${JSON.stringify(t)}`;
    if (key !== lastKey) {
      lastKey = key;
      loaded = false;
      open = {};
    }
    const mine = ++seq;
    store.api
      .changes(id, f, t)
      .then((list) => {
        if (mine !== seq) return;
        changes = list;
        error = null;
        loaded = true;
        if (!Object.keys(open).length && list[0]) open = { [list[0].path]: true };
      })
      .catch((err: unknown) => {
        if (mine !== seq) return;
        changes = [];
        error = ApiError.from(err).message;
        loaded = true;
      });
  });
</script>

{#if error}
  <p class="muted">Could not load the changes: {error}</p>
{:else if !loaded}
  <p class="muted">Loading…</p>
{:else if changes.length === 0}
  <p class="muted">{empty}</p>
{:else}
  <div class="cards">
    {#each changes as c (c.path)}
      <FileCard
        change={c}
        loadDiff={(opts) => store.api.fileDiff(store.projectId!, from, to, c.path, opts)}
        open={open[c.path] ?? false}
        ontoggle={(o) => (open = { ...open, [c.path]: o })}
        onHunk={onHunk ? (i, opts) => onHunk(c, i, opts) : undefined}
        {hunkLabel}
      >
        {#snippet actions()}
          {@render actionsOf?.(c)}
        {/snippet}
      </FileCard>
    {/each}
  </div>
{/if}

<style>
  .cards {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    max-width: 960px;
  }
  .muted {
    color: var(--text-muted);
  }
</style>
