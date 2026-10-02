<!--
  Window frame (DSNA-41): projects sidebar → versions → changes, or Settings.
  Keyboard: arrows move in a list, Enter goes one level in, Backspace / Alt+Left one up.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { api as defaultApi, type Api } from './lib/api';
  import ChangesView from './lib/components/ChangesView.svelte';
  import ProjectHeader from './lib/components/ProjectHeader.svelte';
  import SettingsScreen from './lib/settings/SettingsScreen.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import RestoreResultDialog from './lib/dialogs/RestoreResultDialog.svelte';
  import ToastHost from './lib/toast/ToastHost.svelte';
  import VersionsList from './lib/components/VersionsList.svelte';
  import { AppStore, provideStore } from './lib/stores/app.svelte';

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

  let versionsList: VersionsList | undefined = $state();
  let changesView: ChangesView | undefined = $state();
  let shell: HTMLElement | undefined = $state();

  const SIDEBAR_KEY = 'dsnap.sidebarWidth';
  function savedWidth(): number {
    try {
      const n = Number(globalThis.localStorage?.getItem(SIDEBAR_KEY));
      return n >= 160 && n <= 480 ? n : 240;
    } catch {
      return 240;
    }
  }
  let sidebarWidth = $state(savedWidth());

  function startResize(e: PointerEvent) {
    const startX = e.clientX;
    const start = sidebarWidth;
    const move = (ev: PointerEvent) => {
      sidebarWidth = Math.min(480, Math.max(160, start + ev.clientX - startX));
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      try {
        globalThis.localStorage?.setItem(SIDEBAR_KEY, String(sidebarWidth));
      } catch {
        // Not persisted.
      }
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  function focusSidebar() {
    shell?.querySelector<HTMLElement>('[role="option"][tabindex="0"]')?.focus();
  }
</script>

<main class="shell" bind:this={shell} style:grid-template-columns="{sidebarWidth}px 4px 1fr">
  <Sidebar onenter={() => versionsList?.focus()} />
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="splitter" onpointerdown={startResize} title="Drag to resize"></div>
  <section class="content">
    {#if store.view === 'settings'}
      <SettingsScreen />
    {:else if !store.project}
      <p class="empty">
        {store.projects.length === 0 && !store.loading.projects
          ? 'Add a project folder to start taking snapshots.'
          : 'No project selected.'}
      </p>
    {:else}
      <ProjectHeader />
      <div class="work">
        <VersionsList
          bind:this={versionsList}
          onenter={() => changesView?.focus()}
          onback={focusSidebar}
        />
        <ChangesView bind:this={changesView} onback={() => versionsList?.focus()} />
      </div>
    {/if}
  </section>
  <ToastHost toasts={store.toasts} />
  {#if store.restoreResult}
    {@const r = store.restoreResult}
    <RestoreResultDialog
      report={r}
      onclose={() => (store.restoreResult = null)}
      onundo={() => {
        const safety = r.safetyVersion;
        store.restoreResult = null;
        void store.restoreProject(safety);
      }}
    />
  {/if}
</main>

<style>
  .shell {
    display: grid;
    height: 100vh;
    overflow: hidden;
  }
  .splitter {
    cursor: col-resize;
    background: transparent;
  }
  .splitter:hover {
    background: var(--border);
  }
  .content {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .work {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(240px, 300px) 1fr;
  }
  .empty {
    margin: auto;
    color: var(--text-muted);
  }
</style>
