<!--
  Diff pane of the changes view: loads the selected file's diff for the current sides and
  options, with the file header and "Restore this file". The diff itself is DiffViewer.
-->
<script lang="ts">
  import { ApiError, type FileChange, type FileDiff } from '../api';
  import { useStore } from '../stores/app.svelte';
  import { diffPrefs } from '../stores/prefs.svelte';
  import DiffViewer from '../diff/DiffViewer.svelte';

  interface Props {
    onrestore: (c: FileChange) => void;
    canRestore: (c: FileChange) => boolean;
  }
  let { onrestore, canRestore }: Props = $props();

  const store = useStore();

  let diff = $state<FileDiff | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let seq = 0;

  const change = $derived(store.change);

  $effect(() => {
    const c = change;
    const sides = store.sides;
    const id = store.projectId;
    const opts = { ignoreWhitespace: diffPrefs.ignoreWhitespace, context: diffPrefs.context };
    // Reload when the folder or versions change too.
    void store.changes;
    if (!c || !sides || id === null) {
      diff = null;
      return;
    }
    const mine = ++seq;
    loading = true;
    error = null;
    store.api
      .fileDiff(id, sides.from, sides.to, c.path, opts)
      .then((d) => {
        if (mine === seq) diff = d;
      })
      .catch((err: unknown) => {
        if (mine === seq) {
          diff = null;
          error = ApiError.from(err).message;
        }
      })
      .finally(() => {
        if (mine === seq) loading = false;
      });
  });
</script>

<section class="pane" aria-label="Diff">
  {#if !change}
    <p class="note">Select a file to see what changed.</p>
  {:else}
    <header class="head">
      <span class="path mono" title={change.path}>
        {#if change.status.kind === 'renamed'}{change.status.from} →
        {/if}{change.path}
      </span>
      {#if canRestore(change)}
        <button type="button" onclick={() => onrestore(change)}>Restore this file</button>
      {/if}
    </header>
    <div class="body">
      {#if error}
        <p class="note error">Could not load the diff: {error}</p>
      {:else if diff}
        <DiffViewer
          {diff}
          busy={loading}
          status={change.status}
          mode={diffPrefs.mode}
          ignoreWhitespace={diffPrefs.ignoreWhitespace}
          onModeChange={(m) => (diffPrefs.mode = m)}
          onToggleWhitespace={(on) => (diffPrefs.ignoreWhitespace = on)}
          onExpand={() => (diffPrefs.context = Math.min(diffPrefs.context * 4 + 10, 100_000))}
          loadImage={(h, m) => store.api.readBlobAsDataUrl(h, m)}
        />
      {:else if loading}
        <p class="note">Loading…</p>
      {/if}
    </div>
  {/if}
</section>

<style>
  .pane {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--border);
  }
  .path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .note {
    padding: var(--space-3);
    color: var(--text-muted);
    margin: 0;
  }
  .error {
    color: var(--status-deleted);
  }
</style>
