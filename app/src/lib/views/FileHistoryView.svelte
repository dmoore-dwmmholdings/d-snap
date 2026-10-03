<!--
  A file's life story (DSNA-127), opened from "Find any file": every snapshot where it
  changed, which turn or edit made it, the diff against the folder now, and "Bring back
  this version".
-->
<script lang="ts">
  import { ApiError, type FileChange, type PathVersion } from '../api';
  import { useStore } from '../stores/app.svelte';
  import { absoluteTime, plural, relativeTime } from '../util/format';
  import FileCard from './FileCard.svelte';
  import Intro from './Intro.svelte';

  let { path }: { path: string } = $props();
  const store = useStore();
  let entries = $state<PathVersion[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let open = $state<Record<number, boolean>>({});
  let seq = 0;

  $effect(() => {
    const id = store.projectId;
    const p = path;
    void store.revision;
    if (id === null) return;
    const mine = ++seq;
    store.api
      .pathHistory(id, p)
      .then((list) => {
        if (mine !== seq) return;
        entries = list;
        error = null;
        loaded = true;
      })
      .catch((err: unknown) => {
        if (mine !== seq) return;
        error = ApiError.from(err).message;
        loaded = true;
      });
  });

  const unsaved = $derived(store.unsaved.find((c) => c.path === path));
  const missing = $derived(store.project?.missing ?? false);

  /** Who made this version of the file. */
  function origin(pv: PathVersion): { who: string; what: string } {
    const v = pv.version;
    const turn = store.turns.find((t) => t.after === v.id);
    if (turn)
      return { who: `Turn ${turn.seq}`, what: turn.prompt ? `“${turn.prompt}”` : 'Agent turn' };
    const next = store.turns.find((t) => t.before === v.id);
    if (next && v.label === 'before agent turn')
      return { who: 'Your edits', what: `Before Turn ${next.seq}` };
    const kind = { manual: 'Checkpoint', auto: 'Auto-save', cli: 'dsnap', safety: 'Safety copy' }[
      v.kind
    ];
    return { who: kind, what: v.label };
  }

  function asChange(pv: PathVersion): FileChange {
    return {
      path,
      status: pv.entry ? { kind: 'modified' } : { kind: 'deleted' },
      old: pv.entry,
      new: null,
      linesAdded: null,
      linesRemoved: null,
    };
  }
</script>

<section class="view" aria-labelledby="file-title">
  <Intro
    chip="File history"
    meta={loaded ? plural(entries.length, 'version') : ''}
    title={path}
    titleId="file-title"
  >
    <p>
      Every version d-snap holds, newest first, with the turn or edit that made it. Open one to
      compare it with the file now; bringing one back saves a safety copy first.
    </p>
    {#snippet actions()}
      <button type="button" onclick={() => (store.selection = store.defaultSelection())}
        >Back to review</button
      >
    {/snippet}
  </Intro>

  {#if error}
    <p class="muted">Could not load the history: {error}</p>
  {:else if !loaded}
    <p class="muted">Loading…</p>
  {:else}
    <div class="cards">
      {#if unsaved}
        <div class="now">
          <span class="chip marker">NOW</span>
          <span>Changed in the folder and not snapshotted yet.</span>
        </div>
      {/if}
      {#each entries as pv, i (pv.version.id)}
        {@const o = origin(pv)}
        <div class="entry">
          <div class="when">
            <span class="who">{o.who}</span>
            <span class="time" title={absoluteTime(pv.version.createdAtMs)}
              >{relativeTime(pv.version.createdAtMs)}</span
            >
          </div>
          <p class="what">{o.what}</p>
          {#if pv.entry}
            <FileCard
              change={asChange(pv)}
              chip={i === 0 && !unsaved ? 'CURRENT' : 'THEN → NOW'}
              chipClass={i === 0 && !unsaved ? 'marker' : ''}
              loadDiff={(opts) =>
                store.api.fileDiff(
                  store.projectId!,
                  pv.version.id,
                  { kind: 'workingTree' },
                  path,
                  opts,
                )}
              open={open[pv.version.id] ?? false}
              ontoggle={(x) => (open = { ...open, [pv.version.id]: x })}
            >
              {#snippet actions()}
                {#if !(i === 0 && !unsaved)}
                  <button
                    type="button"
                    disabled={missing || store.busy !== null}
                    onclick={() => void store.restoreFile(pv.version.id, path)}
                    >Bring back this version</button
                  >
                {/if}
              {/snippet}
            </FileCard>
          {:else}
            <p class="deleted">Deleted in this snapshot.</p>
          {/if}
        </div>
      {/each}
      {#if entries.length === 0}
        <p class="muted">No snapshot holds this file yet.</p>
      {/if}
    </div>
  {/if}
</section>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
  .view :global(h1) {
    font-family: var(--font-mono);
    font-size: 26px;
    letter-spacing: -0.01em;
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    max-width: 960px;
  }
  .now {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 14px;
  }
  .entry {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .when {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .who {
    font-weight: 600;
  }
  .time {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .what {
    margin: 0;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .deleted {
    margin: 0;
    padding: 12px 14px;
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    color: var(--text-muted);
  }
  .muted {
    color: var(--text-muted);
  }
</style>
