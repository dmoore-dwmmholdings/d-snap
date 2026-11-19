<!--
  "Take a snapshot" (F4, F7, DSNA-126): an optional name, what will be saved, then the save
  with progress. Files over the size cap are reported after the save (F6).
-->
<script lang="ts">
  import { useStore } from '../stores/app.svelte';
  import { plural } from '../util/format';
  import Dialog from './Dialog.svelte';

  let { onclose }: { onclose: () => void } = $props();
  const store = useStore();
  let name = $state('');
  const n = $derived(store.unsaved.length);
  const placeholder = new Date().toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });

  async function save() {
    if (store.busy) return;
    const r = await store.snapshot(name);
    if (r !== null) onclose();
  }
</script>

<Dialog
  title="Take a snapshot of {store.project?.name ?? 'this folder'}"
  confirmLabel={store.busy === 'snapshot' ? 'Saving…' : 'Save snapshot'}
  busy={store.busy !== null}
  oncancel={onclose}
  onconfirm={() => void save()}
>
  <p>
    {n > 0
      ? `${plural(n, 'file')} changed since the last snapshot.`
      : 'Nothing changed since the last snapshot, so nothing will be saved.'}
  </p>
  <form
    onsubmit={(e) => {
      e.preventDefault();
      void save();
    }}
  >
    <label class="field">
      <span>Name <span class="muted">(optional)</span></span>
      <input type="text" maxlength="200" {placeholder} bind:value={name} />
    </label>
  </form>
  <p class="muted">Named snapshots show as checkpoints in History. Pin one to keep it for good.</p>
</Dialog>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-weight: 600;
    margin: 4px 0 10px;
  }
  .field input {
    font-weight: 400;
    min-height: 42px;
  }
  .muted {
    color: var(--text-muted);
    font-weight: 400;
  }
</style>
