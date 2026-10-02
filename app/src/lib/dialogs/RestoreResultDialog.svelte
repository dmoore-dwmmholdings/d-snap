<!--
  After a partial restore (J2): which files were not written and why, with
  "Undo using safety snapshot".
-->
<script lang="ts">
  import type { RestoreReport } from '../api';
  import { plural } from '../util/format';
  import Dialog from './Dialog.svelte';

  interface Props {
    report: RestoreReport;
    onundo: () => void;
    onclose: () => void;
  }
  let { report, onundo, onclose }: Props = $props();
</script>

<Dialog
  title="Restore incomplete"
  confirmLabel="Undo using safety snapshot"
  cancelLabel="Keep as is"
  onconfirm={onundo}
  oncancel={onclose}
>
  <p>
    {plural(report.written.length, 'file')} written, {plural(report.deleted.length, 'file')} deleted.
    {plural(report.failed.length, 'file')} could not be restored:
  </p>
  <ul class="failed">
    {#each report.failed as [path, why] (path)}
      <li><span class="mono">{path}</span> — {why}</li>
    {/each}
  </ul>
  {#if report.uncaptured.length > 0}
    <p>
      Left alone because no snapshot holds them: <span class="mono"
        >{report.uncaptured.join(', ')}</span
      >
    </p>
  {/if}
  <p class="muted">
    Close the program holding a file, then restore again, or undo to get the folder back as it was
    before the restore.
  </p>
</Dialog>

<style>
  .failed {
    max-height: 200px;
    overflow: auto;
    padding-left: var(--space-4);
  }
  .muted {
    color: var(--text-muted);
  }
</style>
