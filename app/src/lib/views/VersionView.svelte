<!--
  One snapshot (DSNA-126): a checkpoint, safety copy, auto-save or command-line snapshot.
  Rename, pin and delete it (F11), restore the folder to it (F20), compare it with another
  entry (F16), and see what changed in it, bringing any file back (F19).
-->
<script lang="ts">
  import type { VersionId } from '../api';
  import Dialog from '../dialogs/Dialog.svelte';
  import { useStore } from '../stores/app.svelte';
  import { plural, whenText } from '../util/format';
  import ChangeCards from './ChangeCards.svelte';
  import Intro from './Intro.svelte';

  let { versionId }: { versionId: VersionId } = $props();
  const store = useStore();
  const v = $derived(store.version(versionId));
  const prev = $derived(v ? store.previousVersion(v.id) : null);
  const missing = $derived(store.project?.missing ?? false);

  const CHIP = {
    manual: 'Checkpoint',
    auto: 'Auto-save',
    cli: 'From dsnap',
    safety: 'Safety copy',
  };
  const chip = $derived(
    v ? (v.pinned ? `Pinned ${CHIP[v.kind].toLowerCase()}` : CHIP[v.kind]) : '',
  );
  const total = $derived(v ? v.counts.added + v.counts.modified + v.counts.deleted : 0);

  let renaming = $state(false);
  let name = $state('');
  let confirmDelete = $state(false);

  function startRename() {
    name = v?.label ?? '';
    renaming = true;
  }

  async function saveRename() {
    renaming = false;
    if (v) await store.setLabel(v.id, name);
  }
</script>

{#if v}
  <section class="view" aria-labelledby="version-title">
    <Intro
      {chip}
      chipClass={v.pinned ? 'ink' : ''}
      meta={`${whenText(v.createdAtMs)}${v.unstable ? ' · some files kept changing while it was taken' : ''}`}
      title={v.kind === 'safety' ? v.label : `“${v.label}”`}
      titleId="version-title"
    >
      <p>
        {#if v.kind === 'safety'}
          Saved automatically just before a restore wrote anything, so that restore can be undone.
        {:else if v.pinned}
          Pinned snapshots are never cleaned up.
        {:else}
          Kept until it is older than the newest snapshots retention keeps. Pin it to keep it for
          good.
        {/if}
        {#if total > 0}{plural(total, 'file')} changed since the snapshot before it.{/if}
      </p>
      {#snippet actions()}
        <button
          type="button"
          class="primary big"
          disabled={missing || store.busy !== null}
          title={missing ? 'Locate the folder first' : undefined}
          onclick={() => void store.askRestore(v.id)}>Restore folder to this…</button
        >
        <button type="button" onclick={() => void store.setPinned(v.id, !v.pinned)}
          >{v.pinned ? 'Unpin' : 'Pin'}</button
        >
        {#if renaming}
          <form
            class="rename"
            onsubmit={(e) => {
              e.preventDefault();
              void saveRename();
            }}
          >
            <label class="sr-only" for="rename-version">Name</label>
            <!-- svelte-ignore a11y_autofocus -->
            <input id="rename-version" type="text" maxlength="200" bind:value={name} autofocus />
            <button type="submit" class="primary">Save</button>
            <button type="button" onclick={() => (renaming = false)}>Cancel</button>
          </form>
        {:else}
          <button type="button" onclick={startRename}>Rename</button>
        {/if}
        <button type="button" onclick={() => store.startCompare(v.id)}>Compare with…</button>
        <button type="button" class="link" onclick={() => (confirmDelete = true)}>Delete</button>
      {/snippet}
    </Intro>

    <ChangeCards
      from={prev?.id ?? null}
      to={{ kind: 'version', id: v.id }}
      empty="Nothing changed in this snapshot."
    >
      {#snippet actions(c)}
        {#if c.new && !missing}
          <button type="button" onclick={() => void store.restoreFile(v.id, c.path)}
            >Bring back this version</button
          >
        {/if}
        <button
          type="button"
          class="link"
          onclick={() => store.select({ kind: 'file', path: c.path })}>History</button
        >
      {/snippet}
    </ChangeCards>
  </section>

  {#if confirmDelete}
    <Dialog
      title="Delete “{v.label}”?"
      confirmLabel="Delete snapshot"
      danger
      oncancel={() => (confirmDelete = false)}
      onconfirm={() => {
        confirmDelete = false;
        void store.deleteVersion(v.id);
      }}
    >
      <p>The folder is not touched. Files only this snapshot held can no longer be brought back.</p>
    </Dialog>
  {/if}
{:else}
  <p class="muted">This snapshot is no longer in the history.</p>
{/if}

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
  .rename {
    display: flex;
    gap: 8px;
  }
  .muted {
    color: var(--text-muted);
  }
</style>
