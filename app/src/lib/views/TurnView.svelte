<!--
  Turn review (DSNA-125): the prompt as the heading, what the agent changed as file cards,
  and the decisions. Keep / Undo per file, "Undo just this part" per hunk, Keep all and
  Undo the whole turn. Files changed after the turn are only undone on purpose.
-->
<script lang="ts">
  import type { RelPath, TurnFile, TurnId } from '../api';
  import Dialog from '../dialogs/Dialog.svelte';
  import { useStore } from '../stores/app.svelte';
  import { absoluteTime, plural, relativeTime } from '../util/format';
  import FileCard from './FileCard.svelte';

  let { turnId }: { turnId: TurnId } = $props();
  const store = useStore();

  const turn = $derived(store.turn(turnId));
  let files = $state<TurnFile[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let open = $state<Record<RelPath, boolean>>({});
  let seq = 0;
  let lastTurn: TurnId | null = null;

  $effect(() => {
    const id = turnId;
    void store.revision;
    const t = store.turn(id);
    if (!t || t.state === 'pruned') {
      files = [];
      loaded = true;
      return;
    }
    const mine = ++seq;
    if (lastTurn !== id) {
      lastTurn = id;
      loaded = false;
      open = {};
    }
    store
      .turnFiles(id)
      .then((list) => {
        if (mine !== seq) return;
        files = list;
        error = null;
        loaded = true;
        // Open the first file still waiting, unless something is open already.
        if (!Object.values(open).some(Boolean)) {
          const first = list.find((f) => f.decision === null);
          if (first) open = { [first.change.path]: true };
        }
      })
      .catch((err: unknown) => {
        if (mine !== seq) return;
        error = String((err as Error)?.message ?? err);
        loaded = true;
      });
  });

  const running = $derived(turn?.state === 'running');
  const pending = $derived(files.filter((f) => f.decision === null));
  const undone = $derived(files.filter((f) => f.decision === 'undone'));
  const kept = $derived(files.filter((f) => f.decision === 'kept'));
  const totals = $derived.by(() => {
    let a = 0;
    let d = 0;
    for (const f of files) {
      a += f.change.linesAdded ?? 0;
      d += f.change.linesRemoved ?? 0;
    }
    return [a ? `+${a}` : '', d ? `−${d}` : ''].filter(Boolean).join(' ');
  });

  const statusText = $derived.by(() => {
    const t = turn;
    if (!t) return '';
    if (t.state === 'running') return `running since ${relativeTime(t.startedAtMs)}`;
    const ended = t.endedAtMs ?? t.startedAtMs;
    const mins = Math.max(1, Math.round((ended - t.startedAtMs) / 60_000));
    const parts = [`finished ${relativeTime(ended)}`, `took ${mins} min`];
    if (pending.length > 0 && files.length > 0) {
      parts.push(`${files.length - pending.length} of ${plural(files.length, 'file')} reviewed`);
    }
    return parts.join(' · ');
  });

  const doneText = $derived.by(() => {
    if (undone.length === files.length) {
      return `This turn is undone. Every file is back as it was before the prompt.`;
    }
    if (undone.length > 0) return `Reviewed: ${kept.length} kept, ${undone.length} undone.`;
    return 'You kept everything from this turn.';
  });

  function openNext(after: RelPath) {
    const next = files.find((f) => f.decision === null && f.change.path !== after);
    open = next ? { [next.change.path]: true } : {};
  }

  async function keep(f: TurnFile) {
    if (await store.decide(turnId, f.change.path, 'kept')) openNext(f.change.path);
  }

  async function undoFile(f: TurnFile, includeChanged = false) {
    const r = await store.undoTurn(turnId, [f.change.path], includeChanged);
    if (r) openNext(f.change.path);
  }

  // ---- dialogs ----

  let confirmFile = $state<TurnFile | null>(null);
  let undoAll = $state(false);
  let includeChanged = $state(false);
  const toUndo = $derived(files.filter((f) => f.decision !== 'undone'));
  const changedOnes = $derived(toUndo.filter((f) => f.changedSince));

  async function confirmUndoAll() {
    const paths = toUndo.filter((f) => includeChanged || !f.changedSince).map((f) => f.change.path);
    undoAll = false;
    if (paths.length === 0) return;
    open = {};
    await store.undoTurn(turnId, paths, includeChanged, true);
    includeChanged = false;
  }

  const loadDiff = (f: TurnFile) => (opts: Parameters<typeof store.api.fileDiff>[4]) => {
    const t = turn!;
    const to =
      running || t.after === null
        ? ({ kind: 'workingTree' } as const)
        : ({ kind: 'version', id: t.after } as const);
    return store.api.fileDiff(t.projectId, t.before, to, f.change.path, opts);
  };
</script>

{#if turn}
  <section class="turn" aria-labelledby="turn-title">
    <div class="intro">
      <div class="eyebrow">
        <span class="chip" class:marker={turn.state === 'pending'} class:ink={running}
          >Turn {turn.seq}</span
        >
        <span title={absoluteTime(turn.startedAtMs)}>{statusText}</span>
      </div>
      <h1 id="turn-title">
        {#if turn.prompt}“{turn.prompt}”{:else}Turn {turn.seq}{/if}
      </h1>
      {#if !turn.prompt}
        <p class="sub">No prompt was recorded for this turn (older hooks, or run by hand).</p>
      {/if}
      {#if loaded && !error && turn.state !== 'pruned'}
        <p class="sub">
          {running ? 'So far the agent changed' : 'The agent changed'}
          {plural(files.length, 'file')} in {store.project?.name}{#if totals},
            <span class="mono">{totals}</span>{/if}.
        </p>
      {/if}
    </div>

    {#if turn.state === 'pruned'}
      <p class="panel">
        The snapshots of this turn were cleaned up by retention, so it can no longer be compared or
        undone. Pin snapshots you want to keep.
      </p>
    {:else if running}
      <p class="panel">
        The agent is still working. Review opens when it stops; until then this shows what it has
        changed so far.
      </p>
    {:else if loaded && files.length > 0}
      {#if pending.length > 0}
        <div class="actions">
          <button type="button" class="primary big" onclick={() => void store.keepTurn(turnId)}
            >{pending.length === files.length ? 'Keep all' : 'Keep the rest'}</button
          >
          <button type="button" class="outline big" onclick={() => (undoAll = true)}
            >Undo the whole turn</button
          >
          <span class="hint"
            >Undo puts the files back as they were before the prompt. A safety copy is saved first.</span
          >
        </div>
      {:else}
        <div class="done">
          <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true"
            ><path
              d="M5 12l5 5L20 7"
              fill="none"
              stroke="currentColor"
              stroke-width="2.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            /></svg
          >
          <span class="done-text">{doneText}</span>
          {#if undone.length < files.length}
            <button type="button" class="outline" onclick={() => (undoAll = true)}
              >Undo the whole turn</button
            >
          {/if}
          {#if undone.length > 0}
            <button type="button" class="primary" onclick={() => void store.redoTurn(turnId, null)}
              >{undone.length === files.length
                ? 'Put the turn back'
                : 'Put undone files back'}</button
            >
          {/if}
        </div>
      {/if}
    {/if}

    {#if error}
      <p class="panel">Could not load this turn's files: {error}</p>
    {:else if !loaded}
      <p class="muted">Loading…</p>
    {:else if files.length === 0 && turn.state !== 'pruned'}
      <p class="muted">{running ? 'Nothing changed yet.' : 'This turn changed nothing.'}</p>
    {:else}
      <div class="cards">
        {#each files as f (f.change.path)}
          {@const isNew = f.change.status.kind === 'added'}
          <FileCard
            change={f.change}
            loadDiff={loadDiff(f)}
            open={open[f.change.path] ?? false}
            ontoggle={(o) => (open = { ...open, [f.change.path]: o })}
            chip={f.decision === 'undone' ? (isNew ? 'REMOVED' : 'UNDONE') : undefined}
            chipClass={f.decision === 'undone' ? 'ink' : ''}
            settled={f.decision !== null}
            note={f.decision === null && f.changedSince
              ? 'You changed this file after the turn. Undoing it would discard those edits.'
              : undefined}
            onHunk={!running &&
            f.decision === null &&
            !f.changedSince &&
            f.change.status.kind === 'modified'
              ? (i, opts) => void store.undoTurnHunk(turnId, f.change.path, i, opts)
              : undefined}
            hunkLabel="Undo just this part"
          >
            {#snippet actions()}
              {#if running}
                <!-- Decisions wait until the turn ends. -->
              {:else if f.decision === 'kept'}
                <span class="decided">
                  <svg width="15" height="15" viewBox="0 0 24 24" aria-hidden="true"
                    ><path
                      d="M5 12l5 5L20 7"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="2.5"
                      stroke-linecap="round"
                    /></svg
                  >Kept</span
                >
                <button
                  type="button"
                  class="link"
                  aria-label="Change decision on {f.change.path}"
                  onclick={() => void store.decide(turnId, f.change.path, null)}>Change</button
                >
              {:else if f.decision === 'undone'}
                <button
                  type="button"
                  class="link"
                  aria-label="Put {f.change.path} back"
                  onclick={() => void store.redoTurn(turnId, [f.change.path])}>Put it back</button
                >
              {:else}
                {#if f.changedSince}
                  <button type="button" onclick={() => (confirmFile = f)}>Undo anyway…</button>
                {:else}
                  <button
                    type="button"
                    disabled={store.busy !== null}
                    onclick={() => void undoFile(f)}>{isNew ? 'Remove file' : 'Undo file'}</button
                  >
                {/if}
                <button type="button" class="primary" onclick={() => void keep(f)}>Keep</button>
              {/if}
            {/snippet}
          </FileCard>
        {/each}
      </div>
    {/if}
  </section>

  {#if confirmFile}
    {@const f = confirmFile}
    <Dialog
      title="Undo {f.change.path} anyway?"
      confirmLabel="Undo and discard my edits"
      danger
      busy={store.busy !== null}
      oncancel={() => (confirmFile = null)}
      onconfirm={() => {
        confirmFile = null;
        void undoFile(f, true);
      }}
    >
      <p>
        You edited this file after Turn {turn.seq}. Undoing puts it back as it was before the
        prompt, which also removes those later edits.
      </p>
      <p class="muted">
        They stay in a safety copy, so “Undo” on the next message brings them back.
      </p>
    </Dialog>
  {/if}

  {#if undoAll}
    <Dialog
      title="Undo Turn {turn.seq}?"
      confirmLabel="Undo turn"
      busy={store.busy !== null ||
        toUndo.filter((f) => includeChanged || !f.changedSince).length === 0}
      oncancel={() => {
        undoAll = false;
        includeChanged = false;
      }}
      onconfirm={() => void confirmUndoAll()}
    >
      <p>These files go back to how they were before the prompt:</p>
      <ul class="list mono">
        {#each toUndo as f (f.change.path)}
          <li class:skip={f.changedSince && !includeChanged}>
            <span>{f.change.path}</span>
            <span class="muted"
              >{f.changedSince && !includeChanged
                ? 'left alone'
                : f.change.status.kind === 'added'
                  ? 'removed'
                  : 'restored'}</span
            >
          </li>
        {/each}
      </ul>
      {#if changedOnes.length > 0}
        <div class="warn">
          <p>
            {plural(changedOnes.length, 'file')} changed again after this turn ({changedOnes
              .map((f) => f.change.path)
              .join(', ')}). They are left alone so your later edits stay.
          </p>
          <label>
            <input type="checkbox" bind:checked={includeChanged} />
            Undo them too, discarding my later edits
          </label>
        </div>
      {/if}
      <p class="muted">A safety copy of the whole folder is saved first, so you can put it back.</p>
    </Dialog>
  {/if}
{:else}
  <p class="muted">This turn is no longer in the history.</p>
{/if}

<style>
  .turn {
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
  .intro {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    max-width: 780px;
  }
  .eyebrow {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    font-size: 13px;
    color: var(--text-muted);
  }
  .eyebrow .chip {
    font-size: 13px;
    letter-spacing: 0;
    padding: 2px 8px;
  }
  h1 {
    margin: 0;
    font-size: var(--text-display);
    line-height: 1.12;
    font-weight: 600;
    letter-spacing: -0.025em;
    text-wrap: balance;
    font-variation-settings: 'opsz' 96;
  }
  .sub {
    margin: 0;
    font-size: 15px;
    color: var(--text-muted);
  }
  .sub .mono {
    font-size: 13px;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 16px;
  }
  .hint {
    flex: 1 1 240px;
    font-size: 13px;
    color: var(--text-muted);
  }
  .done {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 14px;
    max-width: 880px;
    padding: 12px 16px;
    border-radius: var(--radius-lg);
    background: var(--surface-1);
  }
  .done-text {
    flex: 1 1 260px;
    font-size: 15px;
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    max-width: 960px;
  }
  .decided {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    font-weight: 600;
  }
  .panel {
    margin: 0;
    max-width: 780px;
    padding: 14px 16px;
    border-radius: var(--radius-lg);
    background: var(--surface-1);
  }
  .muted {
    color: var(--text-muted);
  }
  .list {
    margin: 0 0 var(--space-3);
    padding: 10px 12px;
    list-style: none;
    border-radius: var(--radius-md);
    background: var(--surface-1);
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12.5px;
  }
  .list li {
    display: flex;
    gap: 12px;
    justify-content: space-between;
  }
  .list li.skip span:first-child {
    color: var(--text-muted);
    text-decoration: line-through;
  }
  .warn {
    margin-bottom: var(--space-3);
    padding: 10px 14px;
    border: 1px solid var(--warn-border);
    border-radius: var(--radius-md);
    background: var(--warn-bg);
    color: var(--warn-text);
  }
  .warn p {
    margin: 0 0 6px;
  }
  .warn label {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 32px;
    color: var(--text);
    cursor: pointer;
  }
</style>
