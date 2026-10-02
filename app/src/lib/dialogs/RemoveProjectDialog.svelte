<!-- Confirm removing a project (J2, F2), optionally deleting its snapshots now. -->
<script lang="ts">
  import type { Project } from '../api';
  import Dialog from './Dialog.svelte';

  interface Props {
    project: Project;
    onconfirm: (deleteSnapshots: boolean) => void;
    oncancel: () => void;
  }
  let { project, onconfirm, oncancel }: Props = $props();

  let alsoDelete = $state(false);
</script>

<Dialog
  title="Remove {project.name}?"
  confirmLabel="Remove"
  danger
  {oncancel}
  onconfirm={() => onconfirm(alsoDelete)}
>
  <p>
    D-Snap stops tracking <span class="mono">{project.root}</span>. The folder itself is not
    touched.
  </p>
  <label class="check">
    <input type="checkbox" bind:checked={alsoDelete} />
    Also delete its snapshots now (frees disk space; cannot be undone)
  </label>
</Dialog>

<style>
  .check {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
  }
</style>
