<!--
  Project header (DSNA-41, F4, F6): name and folder, label field and the Snapshot button,
  snapshot progress with Cancel, and the skipped-files notice.
-->
<script lang="ts">
  import ProgressOverlay from '../dialogs/ProgressOverlay.svelte';
  import SnapshotWarnings from '../dialogs/SnapshotWarnings.svelte';
  import { useStore } from '../stores/app.svelte';

  const store = useStore();
  let label = $state('');
  let showSkipped = $state(true);

  const project = $derived(store.project);
  const canSnap = $derived(
    !!project && !project.missing && !store.snapshotting && store.unsavedCount > 0,
  );
  const progress = $derived(
    store.projectId !== null && store.snapshotting ? store.progress[store.projectId] : undefined,
  );

  async function snap() {
    if (!canSnap) return;
    const r = await store.snapshot(label);
    if (r) {
      label = '';
      showSkipped = true;
    }
  }
</script>

{#if project}
  <header class="header">
    <div class="title">
      <h2>{project.name}</h2>
      <span class="path mono" title={project.root}>{project.root}</span>
    </div>
    {#if project.missing}
      <div class="missing" role="alert">
        Folder not found. Its snapshots are still restorable.
        <button type="button" onclick={() => void store.relocateProject(project.id)}
          >Locate folder…</button
        >
      </div>
    {:else}
      <div class="snap">
        <input
          type="text"
          placeholder="Label (optional)"
          aria-label="Snapshot label"
          maxlength="200"
          bind:value={label}
          onkeydown={(e) => e.key === 'Enter' && void snap()}
        />
        <button
          type="button"
          class="primary"
          disabled={!canSnap}
          title={store.unsavedCount === 0 && !store.snapshotting
            ? 'Nothing changed since the last snapshot'
            : undefined}
          onclick={() => void snap()}
        >
          {#if store.snapshotting}<span class="spinner" aria-hidden="true"></span>{/if}
          Snapshot
        </button>
      </div>
    {/if}
  </header>

  {#if store.snapshotting}
    <ProgressOverlay
      {progress}
      oncancel={progress
        ? () => void store.api.cancelOperation(progress.opId).catch(() => {})
        : undefined}
    />
  {/if}

  {#if store.lastSnapshot && showSkipped}
    <SnapshotWarnings
      skipped={store.lastSnapshot.skipped}
      unstable={store.lastSnapshot.unstablePaths}
      ondismiss={() => (showSkipped = false)}
    />
  {/if}
{/if}

<style>
  .header {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--border);
  }
  .title {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  h2 {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 600;
  }
  .path {
    color: var(--text-muted);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .snap {
    display: flex;
    gap: var(--space-2);
  }
  .snap input {
    width: 220px;
  }
  .missing {
    color: var(--status-deleted);
    display: flex;
    gap: var(--space-2);
    align-items: center;
  }
  .spinner {
    display: inline-block;
    width: 10px;
    height: 10px;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    margin-right: 4px;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
