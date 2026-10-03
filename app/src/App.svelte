<!--
  "Turn review" window (DSNA-124): header with folders and search, the review area, and
  History. Settings is the only other screen. The tray popover is TrayView (DSNA-128).
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { api as defaultApi, type Api } from './lib/api';
  import ConfirmRestoreDialog from './lib/dialogs/ConfirmRestoreDialog.svelte';
  import ProgressOverlay from './lib/dialogs/ProgressOverlay.svelte';
  import RestoreResultDialog from './lib/dialogs/RestoreResultDialog.svelte';
  import SnapshotDialog from './lib/dialogs/SnapshotDialog.svelte';
  import SettingsScreen from './lib/settings/SettingsScreen.svelte';
  import Header from './lib/shell/Header.svelte';
  import HistoryColumn from './lib/shell/HistoryColumn.svelte';
  import { AppStore, provideStore } from './lib/stores/app.svelte';
  import ToastHost from './lib/toast/ToastHost.svelte';
  import AutosView from './lib/views/AutosView.svelte';
  import CompareView from './lib/views/CompareView.svelte';
  import EditsView from './lib/views/EditsView.svelte';
  import FileHistoryView from './lib/views/FileHistoryView.svelte';
  import MissingView from './lib/views/MissingView.svelte';
  import TurnView from './lib/views/TurnView.svelte';
  import UnsavedView from './lib/views/UnsavedView.svelte';
  import VersionView from './lib/views/VersionView.svelte';

  let { api = defaultApi }: { api?: Api } = $props();

  // The store lives as long as the window; `api` is fixed at startup.
  // svelte-ignore state_referenced_locally
  const store = new AppStore(api);
  provideStore(store);

  onMount(() => {
    void store.start();
    const onFocus = () => void store.refreshAll();
    window.addEventListener('focus', onFocus);
    return () => {
      window.removeEventListener('focus', onFocus);
      store.stop();
    };
  });

  /** A missing folder shows its panel until the user chooses to browse its snapshots. */
  let browseMissing = $state<number | null>(null);
  const showMissing = $derived(!!store.project?.missing && browseMissing !== store.projectId);
  const sel = $derived(store.selection);
  const progress = $derived(store.projectId === null ? undefined : store.progress[store.projectId]);
  let main: HTMLElement | undefined = $state();

  // A new selection starts at the top of the review area.
  $effect(() => {
    void store.selectedKey;
    main?.scrollTo?.({ top: 0 });
  });
</script>

<div class="app">
  <Header />

  {#if store.view === 'settings'}
    <main class="settings">
      <SettingsScreen />
    </main>
  {:else if store.projects.length === 0}
    <main class="welcome">
      {#if !store.loading.projects}
        <span class="chip marker">Welcome</span>
        <h1>Point d-snap at a folder.</h1>
        <p>
          It snapshots the folder before and after every Claude Code turn, so you can review what
          the agent changed and undo any file, any part of a file, or the whole turn.
        </p>
        <button type="button" class="primary big" onclick={() => void store.addProject()}
          >Add a folder…</button
        >
        <p class="muted">Then add the hooks from Settings to record each prompt.</p>
      {/if}
    </main>
  {:else}
    <div class="body">
      <main class="review" bind:this={main} aria-label="Review">
        {#if showMissing}
          <MissingView onbrowse={() => (browseMissing = store.projectId)} />
        {:else if !sel}
          <p class="muted">
            {store.loading.history ? 'Loading…' : 'Pick something in History.'}
          </p>
        {:else if sel.kind === 'turn'}
          <TurnView turnId={sel.id} />
        {:else if sel.kind === 'unsaved'}
          <UnsavedView />
        {:else if sel.kind === 'version'}
          <VersionView versionId={sel.id} />
        {:else if sel.kind === 'autos'}
          <AutosView ids={sel.ids} />
        {:else if sel.kind === 'edits'}
          <EditsView from={sel.from} to={sel.to} />
        {:else if sel.kind === 'file'}
          <FileHistoryView path={sel.path} />
        {:else if sel.kind === 'compare'}
          <CompareView from={sel.from} to={sel.to} />
        {/if}
      </main>
      <HistoryColumn />
    </div>
  {/if}
</div>

{#if store.snapshotDialog}
  <SnapshotDialog onclose={() => (store.snapshotDialog = false)} />
{/if}

{#if store.restoreAsk}
  {@const ask = store.restoreAsk}
  <ConfirmRestoreDialog
    version={ask.version}
    plan={ask.plan ?? undefined}
    busy={!ask.plan || store.busy !== null}
    oncancel={() => (store.restoreAsk = null)}
    onconfirm={() => {
      store.restoreAsk = null;
      void store.restoreProject(ask.version.id);
    }}
  />
{/if}

{#if store.busy && progress && progress.phase !== 'done'}
  <ProgressOverlay
    {progress}
    oncancel={() => void store.api.cancelOperation(progress.opId).catch(() => {})}
  />
{/if}

{#if store.restoreResult}
  {@const r = store.restoreResult}
  <RestoreResultDialog
    report={r}
    onclose={() => (store.restoreResult = null)}
    onundo={() => {
      store.restoreResult = null;
      void store.restoreProject(r.safetyVersion, 'before the restore');
    }}
  />
{/if}

<ToastHost toasts={store.toasts} />

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    min-height: 0;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(280px, 340px);
  }
  .review {
    min-width: 0;
    overflow: auto;
    padding: 36px 48px 96px;
  }
  .settings {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .welcome {
    max-width: 620px;
    margin: 12vh auto 0;
    padding: 0 24px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-4);
  }
  .welcome h1 {
    margin: 0;
    font-size: 40px;
    line-height: 1.08;
    letter-spacing: -0.03em;
    font-variation-settings: 'opsz' 96;
  }
  .welcome p {
    margin: 0;
    font-size: 16px;
    color: var(--text-muted);
  }
  .muted {
    color: var(--text-muted);
  }
  @media (max-width: 860px) {
    .body {
      grid-template-columns: 1fr;
      grid-template-rows: minmax(0, 1fr) auto;
    }
    .review {
      padding: 24px 16px 48px;
    }
  }
</style>
