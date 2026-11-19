<!--
  Tray popover (DSNA-128): the newest turn waiting for review across folders (Review opens
  it in the main window; Undo turn undoes it here, leaving files changed since alone), each
  folder's status, "Snapshot all now" and "Open d-snap".
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import { ApiError, type Api, type Project, type ProjectId, type Turn } from '../api';
  import { hideSelf, openMain } from '../desktop';
  import { plural, relativeTime } from '../util/format';

  let { api }: { api: Api } = $props();

  let projects = $state<Project[]>([]);
  let counts = $state<Record<ProjectId, number>>({});
  let latest = $state<Record<ProjectId, number | null>>({});
  let waiting = $state<Turn[]>([]);
  let message = $state<{ text: string; undo?: () => void } | null>(null);
  let confirming = $state(false);
  let busy = $state(false);

  const top = $derived(waiting[0] ?? null);
  const topProject = $derived(projects.find((p) => p.id === top?.projectId) ?? null);
  const watching = $derived(projects.filter((p) => !p.missing).length);

  async function load() {
    try {
      const list = await api.listProjects();
      const all: Turn[] = [];
      await Promise.all(
        list.map(async (p) => {
          const [turns, versions] = await Promise.all([
            api.listTurns(p.id),
            api.listVersions(p.id),
          ]);
          all.push(...turns.filter((t) => t.state === 'pending'));
          latest[p.id] = versions[0]?.createdAtMs ?? null;
          counts[p.id] = p.missing ? 0 : (await api.status(p.id)).length;
        }),
      );
      projects = list;
      waiting = all.sort((a, b) => (b.endedAtMs ?? 0) - (a.endedAtMs ?? 0));
    } catch (err) {
      message = { text: `Could not load: ${ApiError.from(err).message}` };
    }
  }

  onMount(() => {
    void load();
    const stops = [
      api.onVersionsChanged(() => void load()),
      api.onProjectChanged((e) => (counts[e.projectId] = e.changedCount)),
      api.onProjectsChanged(() => void load()),
    ];
    const onFocus = () => void load();
    window.addEventListener('focus', onFocus);
    return () => {
      stops.forEach((s) => s());
      window.removeEventListener('focus', onFocus);
    };
  });

  async function undoTop() {
    const t = top;
    if (!t) return;
    busy = true;
    confirming = false;
    try {
      const r = await api.undoTurn(t.projectId, t.id, { paths: null, includeChanged: false });
      const left = r.skippedChanged.length;
      message = {
        text: `Turn ${t.seq} undone.${left ? ` ${plural(left, 'file')} that changed since stayed.` : ''}`,
        undo: () => void api.redoTurn(t.projectId, t.id, null).then(() => load()),
      };
      await load();
    } catch (err) {
      message = { text: `Could not undo: ${ApiError.from(err).message}` };
    } finally {
      busy = false;
    }
  }

  async function snapshotAll() {
    busy = true;
    let saved = 0;
    try {
      for (const p of projects.filter((x) => !x.missing && (counts[x.id] ?? 0) > 0)) {
        const r = await api.snapshot(p.id);
        if (r.version) saved++;
      }
      message = { text: saved ? `Saved ${plural(saved, 'folder')}.` : 'Nothing changed anywhere.' };
      await load();
    } catch (err) {
      message = { text: `Snapshot failed: ${ApiError.from(err).message}` };
    } finally {
      busy = false;
    }
  }

  async function locate(p: Project) {
    try {
      const path = await api.pickFolder();
      if (path === null) return;
      await api.relocateProject(p.id, path);
      await load();
    } catch (err) {
      message = { text: `Could not use that folder: ${ApiError.from(err).message}` };
    }
  }

  function status(p: Project): string {
    if (p.missing) return 'Folder missing. Its snapshots are safe.';
    const n = counts[p.id] ?? 0;
    const at = latest[p.id];
    const since = at ? `last snapshot ${relativeTime(at)}` : 'no snapshot yet';
    return n > 0 ? `${plural(n, 'unsaved change')} · ${since}` : `Up to date · ${since}`;
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && void hideSelf()} />

<div class="tray">
  <div class="top">
    <span class="brand">d-snap</span>
    <span class="watch"
      ><span class="ok" aria-hidden="true"></span>Watching {plural(watching, 'folder')}</span
    >
  </div>

  {#if top && topProject}
    <section class="turn" aria-label="Turn waiting for review">
      <span class="where">{topProject.name} · Turn {top.seq} finished</span>
      <span class="what">{top.prompt ? `“${top.prompt}”` : `Turn ${top.seq}`}</span>
      <span class="counts"
        >{plural(top.counts.added + top.counts.modified + top.counts.deleted, 'file')} changed{waiting.length >
        1
          ? ` · ${waiting.length - 1} more waiting`
          : ''}</span
      >
      {#if confirming}
        <p class="confirm">Undo puts its files back. Files that changed since stay. Undo it?</p>
      {/if}
      <div class="buttons">
        {#if confirming}
          <button type="button" class="ink" disabled={busy} onclick={() => void undoTop()}
            >Undo turn</button
          >
          <button type="button" class="line" onclick={() => (confirming = false)}>Cancel</button>
        {:else}
          <button
            type="button"
            class="ink"
            onclick={() => void openMain({ projectId: top.projectId, turnId: top.id })}
            >Review</button
          >
          <button type="button" class="line" disabled={busy} onclick={() => (confirming = true)}
            >Undo turn</button
          >
        {/if}
      </div>
    </section>
  {:else}
    <p class="calm">Nothing waiting for review.</p>
  {/if}

  {#if message}
    <p class="msg" role="status">
      {message.text}
      {#if message.undo}
        <button
          type="button"
          class="link"
          onclick={() => {
            message?.undo?.();
            message = null;
          }}>Put it back</button
        >
      {/if}
    </p>
  {/if}

  <ul class="folders">
    {#each projects as p (p.id)}
      <li>
        <div class="info">
          <span class="name">{p.name}</span>
          <span class="status" class:warn={p.missing}>{status(p)}</span>
        </div>
        {#if p.missing}
          <button type="button" class="small" onclick={() => void locate(p)}>Locate</button>
        {/if}
      </li>
    {/each}
  </ul>

  <div class="foot">
    <button type="button" disabled={busy} onclick={() => void snapshotAll()}
      >Snapshot all now</button
    >
    <button type="button" onclick={() => void openMain()}>Open d-snap</button>
  </div>
</div>

<style>
  .tray {
    height: 100vh;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px;
    background: var(--surface-0);
    border: 1px solid var(--border-strong);
    border-radius: 14px;
    overflow: hidden;
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .brand {
    font-size: 17px;
    font-weight: 700;
    letter-spacing: -0.03em;
  }
  .watch {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .ok {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--status-added);
  }
  .turn {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px;
    border-radius: 12px;
    background: var(--marker);
    color: var(--marker-ink);
  }
  .where {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .what {
    font-size: 16px;
    font-weight: 600;
    line-height: 1.25;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .counts,
  .confirm {
    margin: 0;
    font-size: var(--text-sm);
  }
  .buttons {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
  .buttons button {
    flex: 1;
    min-height: 44px;
    border-radius: 10px;
    font-weight: 600;
  }
  .ink {
    background: #0e0e0e;
    border-color: #0e0e0e;
    color: #fff;
  }
  .ink:hover:not(:disabled) {
    background: #2a2a2a;
  }
  .line {
    background: transparent;
    border: 1.5px solid #0e0e0e;
    color: #0e0e0e;
  }
  .line:hover:not(:disabled) {
    background: rgb(0 0 0 / 6%);
  }
  .calm {
    margin: 0;
    padding: 14px;
    border-radius: 12px;
    background: var(--surface-1);
    color: var(--text-muted);
  }
  .msg {
    margin: 0;
    font-size: 13px;
  }
  .folders {
    list-style: none;
    margin: 0;
    padding: 0;
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .folders li {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 52px;
  }
  .folders li + li {
    border-top: 1px solid var(--border);
  }
  .info {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .name {
    font-weight: 600;
  }
  .status {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .status.warn {
    color: var(--warn-strong);
  }
  .foot {
    display: flex;
    gap: 8px;
  }
  .foot button {
    flex: 1;
    min-height: 40px;
    border-radius: 10px;
  }
</style>
