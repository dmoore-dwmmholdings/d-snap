<!--
  "Not snapshotted yet" (DSNA-126): the folder now against the latest snapshot, with line
  diffs, "Undo just this part" (F22) and bringing a file back to its last snapshot (F19).
-->
<script lang="ts">
  import { useStore } from '../stores/app.svelte';
  import { formatBytes, plural, relativeTime } from '../util/format';
  import ChangeCards from './ChangeCards.svelte';
  import Intro from './Intro.svelte';

  const store = useStore();
  const latest = $derived(store.versions[0] ?? null);
  const n = $derived(store.unsaved.length);
  const lastTurn = $derived(store.turns.find((t) => t.state !== 'running'));
  const skipped = $derived(store.lastSnapshot?.skipped ?? []);
</script>

<section class="view" aria-labelledby="unsaved-title">
  <Intro
    chip="Now"
    meta={latest
      ? `Since the last snapshot, ${relativeTime(latest.createdAtMs)}`
      : 'No snapshot yet'}
    title={n > 0
      ? `${plural(n, 'file')} changed and not snapshotted yet.`
      : 'Nothing new since the last snapshot.'}
    titleId="unsaved-title"
  >
    <p>
      {#if n === 0}
        The folder matches its latest snapshot.
      {:else if lastTurn}
        Your own edits since Turn {lastTurn.seq}. The next agent turn snapshots them on its own, or
        save a named checkpoint now.
      {:else}
        Save a checkpoint to be able to come back to this point.
      {/if}
    </p>
    {#snippet actions()}
      {#if n > 0}
        <button
          type="button"
          class="primary big"
          disabled={store.busy !== null}
          onclick={() => (store.snapshotDialog = true)}>Take a snapshot</button
        >
      {/if}
    {/snippet}
  </Intro>

  {#if skipped.length > 0}
    <div class="warn" role="note">
      {#each skipped.slice(0, 3) as s (s.path)}
        <p>
          <span class="mono">{s.path}</span>
          {#if s.reason.kind === 'tooLarge'}
            is {formatBytes(s.reason.size)}, over the size limit, so snapshots skip it. Raise the
            limit in Settings or ignore the folder it is in.
          {:else}
            could not be read last time, so it was skipped.
          {/if}
        </p>
      {/each}
    </div>
  {/if}

  {#if n > 0}
    <ChangeCards
      from={null}
      to={{ kind: 'workingTree' }}
      onHunk={(c, i, opts) => void store.revertHunk(null, { kind: 'workingTree' }, c.path, i, opts)}
      hunkLabel="Undo just this part"
    >
      {#snippet actions(c)}
        {#if latest && c.old}
          <button type="button" onclick={() => void store.restoreFile(latest.id, c.path)}
            >Bring back last snapshot</button
          >
        {/if}
      {/snippet}
    </ChangeCards>
  {/if}
</section>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
  .warn {
    max-width: 880px;
    padding: 12px 14px;
    border: 1px solid var(--warn-border);
    border-radius: var(--radius-md);
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .warn p {
    margin: 0;
  }
</style>
