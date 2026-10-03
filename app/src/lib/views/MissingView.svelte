<!--
  A folder that moved or was deleted (DSNA-127): its snapshots stay restorable once found.
-->
<script lang="ts">
  import { useStore } from '../stores/app.svelte';
  import { plural } from '../util/format';

  let { onbrowse }: { onbrowse: () => void } = $props();
  const store = useStore();
  const p = $derived(store.project);
  let looking = $state(false);

  async function locate() {
    if (!p) return;
    looking = true;
    try {
      await store.relocateProject(p.id);
    } finally {
      looking = false;
    }
  }
</script>

{#if p}
  <section class="missing" aria-labelledby="missing-title">
    <span class="label">Folder missing</span>
    <h1 id="missing-title">We can’t find {p.name} at <span class="mono">{p.root}</span>.</h1>
    <p>
      It was moved, renamed or deleted. Its {plural(store.versions.length, 'snapshot')} are safe. Point
      d-snap at the folder’s new place to watch it again and restore from them.
    </p>
    <div class="actions">
      <button type="button" class="primary big" disabled={looking} onclick={() => void locate()}
        >{looking ? 'Looking…' : 'Locate folder…'}</button
      >
      <button type="button" class="outline big" onclick={onbrowse}>Browse its snapshots</button>
    </div>
  </section>
{/if}

<style>
  .missing {
    max-width: 640px;
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    padding-top: var(--space-5);
  }
  .label {
    font-size: 13px;
    font-weight: 600;
    color: var(--warn-strong);
  }
  h1 {
    margin: 0;
    font-size: var(--text-display);
    line-height: 1.15;
    font-weight: 600;
    letter-spacing: -0.025em;
    overflow-wrap: anywhere;
  }
  h1 .mono {
    font-size: 0.7em;
    letter-spacing: 0;
  }
  p {
    margin: 0;
    font-size: 15px;
    color: var(--text-muted);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }
</style>
