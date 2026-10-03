<!--
  A run of auto-saves (DSNA-126), grouped so they do not bury turns and checkpoints.
-->
<script lang="ts">
  import type { VersionId } from '../api';
  import { useStore } from '../stores/app.svelte';
  import { absoluteTime, plural } from '../util/format';
  import Intro from './Intro.svelte';

  let { ids }: { ids: VersionId[] } = $props();
  const store = useStore();
  const versions = $derived(ids.map((id) => store.version(id)).filter((v) => v !== null));
  const time = (ms: number) =>
    new Date(ms).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
</script>

<section class="view" aria-labelledby="autos-title">
  <Intro
    chip="Auto-saves"
    meta={plural(versions.length, 'snapshot')}
    title={`${plural(versions.length, 'auto-save')} while you worked.`}
    titleId="autos-title"
  >
    <p>
      Taken on the auto-snapshot timer or after the folder went quiet. Open one to see what changed
      in it.
    </p>
  </Intro>

  <ol class="rows">
    {#each versions as v (v.id)}
      <li>
        <span class="dot" aria-hidden="true"></span>
        <div class="main">
          <span class="title" title={absoluteTime(v.createdAtMs)}
            >Auto-save · {time(v.createdAtMs)}</span
          >
          <span class="meta">+{v.counts.added} ~{v.counts.modified} −{v.counts.deleted}</span>
        </div>
        <button type="button" onclick={() => store.select({ kind: 'version', id: v.id })}
          >Open</button
        >
        <button
          type="button"
          disabled={store.project?.missing || store.busy !== null}
          onclick={() => void store.askRestore(v.id)}>Restore folder to this…</button
        >
      </li>
    {/each}
  </ol>
</section>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    max-width: 880px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    background: var(--surface-0);
    overflow: hidden;
  }
  li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 14px;
    padding: 12px 16px;
  }
  li + li {
    border-top: 1px solid var(--border);
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--border-strong);
  }
  .main {
    flex: 1 1 220px;
    display: flex;
    flex-direction: column;
  }
  .title {
    font-weight: 600;
  }
  .meta {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
</style>
