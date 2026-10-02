<!--
  Progress of a snapshot or restore (J2): phase, files done of total, and Cancel.
-->
<script lang="ts">
  import type { Progress } from '../api';

  interface Props {
    progress: Progress | undefined;
    oncancel?: () => void;
  }
  let { progress, oncancel }: Props = $props();

  const text = $derived.by(() => {
    const p = progress;
    if (!p) return 'Starting…';
    switch (p.phase) {
      case 'queued':
        return 'Waiting for another D-Snap operation on this project…';
      case 'walk':
        return p.done ? `Scanning the folder… ${p.done} files seen` : 'Scanning the folder…';
      case 'hash':
        return `Saving changed files… ${p.done} of ${p.total ?? '?'}`;
      case 'restore':
        return 'Writing files…';
      case 'done':
        return 'Finishing…';
    }
  });
</script>

<div class="progress" role="status" aria-live="polite">
  <span class="text">{text}</span>
  {#if progress?.total}
    <progress max={progress.total} value={progress.done} aria-label="Progress"></progress>
  {:else}
    <progress aria-label="Progress"></progress>
  {/if}
  {#if oncancel}
    <button type="button" onclick={oncancel}>Cancel</button>
  {/if}
</div>

<style>
  .progress {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-4);
    background: var(--surface-1);
    border-bottom: 1px solid var(--border);
  }
  progress {
    flex: 1;
  }
</style>
