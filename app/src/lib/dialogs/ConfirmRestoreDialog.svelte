<!--
  Confirm a restore (J2, F19, F20). For a project: what is overwritten, deleted and
  recreated, and what is left alone because no snapshot holds it (Rule 1). For one file:
  which file goes back to which version. A safety snapshot makes either undoable.
-->
<script lang="ts">
  import type { RelPath, RestorePlan, Version } from '../api';
  import { plural } from '../util/format';
  import Dialog from './Dialog.svelte';

  interface Props {
    version: Version;
    /** Whole-project restore. */
    plan?: RestorePlan;
    /** Single-file restore. */
    file?: RelPath;
    busy?: boolean;
    onconfirm: () => void;
    oncancel: () => void;
  }

  let { version, plan, file, busy = false, onconfirm, oncancel }: Props = $props();

  const nothing = $derived(
    !!plan && plan.write.length + plan.delete.length + plan.createDirs.length === 0,
  );
  const title = $derived(
    file
      ? `Restore ${file.slice(file.lastIndexOf('/') + 1)}?`
      : `Restore project to “${version.label}”?`,
  );
</script>

<Dialog
  {title}
  confirmLabel={nothing ? 'Close' : file ? 'Restore file' : 'Restore'}
  danger={!!plan && plan.delete.length > 0}
  {busy}
  onconfirm={nothing ? oncancel : onconfirm}
  {oncancel}
>
  {#if file}
    <p>
      <span class="mono">{file}</span> goes back to its content in “{version.label}”.
    </p>
    <p class="safety">A safety snapshot is taken first, so this can be undone.</p>
  {:else if plan && nothing}
    <p>The folder already matches this version.</p>
  {:else if plan}
    <p class="safety">A safety snapshot is taken first, so this can be undone.</p>
    {#snippet group(title: string, paths: string[], cls: string)}
      {#if paths.length > 0}
        <details open={paths.length <= 10}>
          <summary class={cls}>{title} ({paths.length})</summary>
          <ul class="paths mono">
            {#each paths.slice(0, 500) as p (p)}<li>{p}</li>{/each}
            {#if paths.length > 500}<li>…and {paths.length - 500} more</li>{/if}
          </ul>
        </details>
      {/if}
    {/snippet}
    {@render group(`Overwrite ${plural(plan.write.length, 'file')}`, plan.write, 'write')}
    {@render group(
      `Delete ${plural(plan.delete.length, 'file')} added since`,
      plan.delete,
      'delete',
    )}
    {@render group(
      `Recreate ${plural(plan.createDirs.length, 'empty folder')}`,
      plan.createDirs,
      'dirs',
    )}
  {/if}
  {#if plan && plan.uncaptured.length > 0}
    <div class="uncaptured">
      <strong>Left alone ({plan.uncaptured.length}):</strong> their current content is in no
      snapshot (ignored, too large, locked or unreadable), so restoring them could lose data.
      <ul class="paths mono">
        {#each plan.uncaptured.slice(0, 200) as p (p)}<li>{p}</li>{/each}
      </ul>
    </div>
  {/if}
</Dialog>

<style>
  .paths {
    margin: var(--space-1) 0 var(--space-2);
    padding-left: var(--space-4);
    max-height: 160px;
    overflow: auto;
    font-size: var(--text-sm);
  }
  summary {
    cursor: pointer;
    font-weight: 600;
  }
  .write {
    color: var(--status-modified);
  }
  .delete {
    color: var(--status-deleted);
  }
  .dirs {
    color: var(--status-added);
  }
  .safety {
    color: var(--text-muted);
  }
  .uncaptured {
    margin-top: var(--space-2);
    padding: var(--space-2);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--status-modified) 12%, var(--bg));
  }
</style>
