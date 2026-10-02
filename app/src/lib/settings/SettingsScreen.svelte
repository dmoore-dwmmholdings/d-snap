<!--
  Settings (DSNA-47): global size cap and retention, the data folder, and per-project
  ignore rules, .gitignore, auto-snapshot (F8) and the Claude Code hooks snippet (F9).
  Save writes both; leaving with unsaved edits asks first.
-->
<script lang="ts">
  import type { AutoSnapshot, GlobalSettings, ProjectId, ProjectSettings } from '../api';
  import Dialog from '../dialogs/Dialog.svelte';
  import { useStore } from '../stores/app.svelte';
  import { validate, type Form } from './form';

  const store = useStore();
  const IGNORE_EXAMPLE = ['*.log', 'build/', '!keep.log'].join('\n');

  const HOOKS = JSON.stringify(
    {
      hooks: {
        UserPromptSubmit: [
          {
            hooks: [
              {
                type: 'command',
                command: 'dsnap snap --hook "$CLAUDE_PROJECT_DIR" -m "before agent turn"',
              },
            ],
          },
        ],
        Stop: [
          {
            hooks: [
              {
                type: 'command',
                command: 'dsnap snap --hook "$CLAUDE_PROJECT_DIR" -m "after agent turn"',
              },
            ],
          },
        ],
      },
    },
    null,
    2,
  );

  let projectId = $state<ProjectId | null>(store.projectId ?? store.projects[0]?.id ?? null);
  let saved = $state<{ global: GlobalSettings; project: ProjectSettings | null } | null>(null);
  let form = $state<Form | null>(null);
  let dataDir = $state('');
  let saving = $state(false);
  let copied = $state(false);
  let discard = $state<(() => void) | null>(null);
  let loadError = $state<string | null>(null);

  function toForm(g: GlobalSettings, p: ProjectSettings | null): Form {
    const a: AutoSnapshot = p?.autoSnapshot ?? { kind: 'off' };
    return {
      sizeCapMb: String(Math.round(g.sizeCapBytes / (1024 * 1024))),
      retentionKeep: String(g.retentionKeep),
      extraIgnore: p ? p.extraIgnore.join('\n') : '',
      respectGitignore: p?.respectGitignore ?? true,
      autoKind: a.kind,
      everyMinutes: a.kind === 'every' ? String(Math.max(1, Math.round(a.secs / 60))) : '10',
      idleSeconds: a.kind === 'afterIdle' ? String(a.secs) : '60',
    };
  }

  async function load(id: ProjectId | null) {
    loadError = null;
    try {
      const [g, p, dir] = await Promise.all([
        store.api.getGlobalSettings(),
        id === null ? Promise.resolve(null) : store.api.getProjectSettings(id),
        store.api.getDataDir(),
      ]);
      saved = { global: g, project: p };
      form = toForm(g, p);
      dataDir = dir;
    } catch (err) {
      loadError = String((err as Error).message ?? err);
    }
  }

  $effect(() => {
    void load(projectId);
  });

  const errors = $derived(form ? validate(form) : {});
  const valid = $derived(Object.keys(errors).length === 0);
  const dirty = $derived(
    !!form &&
      !!saved &&
      JSON.stringify(form) !== JSON.stringify(toForm(saved.global, saved.project)),
  );

  // Ask before leaving with unsaved edits.
  $effect(() => {
    store.leaveGuard = (proceed) => {
      if (!dirty) return true;
      discard = proceed;
      return false;
    };
    return () => {
      store.leaveGuard = null;
    };
  });

  async function save() {
    if (!form || !valid || !saved) return;
    saving = true;
    try {
      const f = form;
      const global: GlobalSettings = {
        sizeCapBytes: Number(f.sizeCapMb) * 1024 * 1024,
        retentionKeep: Number(f.retentionKeep),
      };
      await store.api.setGlobalSettings(global);
      let project: ProjectSettings | null = null;
      if (projectId !== null && saved.project) {
        const autoSnapshot: AutoSnapshot =
          f.autoKind === 'every'
            ? { kind: 'every', secs: Number(f.everyMinutes) * 60 }
            : f.autoKind === 'afterIdle'
              ? { kind: 'afterIdle', secs: Number(f.idleSeconds) }
              : { kind: 'off' };
        project = {
          ...saved.project,
          extraIgnore: f.extraIgnore
            .split('\n')
            .map((l) => l.trim())
            .filter((l) => l.length > 0),
          respectGitignore: f.respectGitignore,
          autoSnapshot,
        };
        await store.api.setProjectSettings(projectId, project);
      }
      saved = { global, project };
      form = toForm(global, project);
      store.toasts.push('success', 'Settings saved.');
      await store.loadProjects();
      if (store.projectId !== null) void store.refreshCount(store.projectId);
    } catch (err) {
      store.toasts.error('Could not save settings', err);
    } finally {
      saving = false;
    }
  }

  function cancel() {
    if (saved) form = toForm(saved.global, saved.project);
  }

  function pickProject(id: ProjectId) {
    if (dirty) {
      discard = () => (projectId = id);
    } else {
      projectId = id;
    }
  }

  async function copyHooks() {
    try {
      await navigator.clipboard.writeText(HOOKS);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch {
      store.toasts.push('error', 'Could not copy to the clipboard.');
    }
  }
</script>

<section class="settings" aria-label="Settings">
  <header>
    <h2>Settings</h2>
    <button type="button" onclick={() => store.setView('main')}>Close</button>
  </header>

  {#if loadError}
    <p class="error">Could not load settings: {loadError}</p>
  {:else if form}
    <form
      onsubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <fieldset>
        <legend>All projects</legend>
        <label>
          <span>Skip files larger than</span>
          <input
            type="text"
            inputmode="numeric"
            class="num"
            min="1"
            max="102400"
            bind:value={form.sizeCapMb}
            aria-invalid={!!errors.sizeCapMb}
          />
          MB
        </label>
        {#if errors.sizeCapMb}<p class="field-error">{errors.sizeCapMb}</p>{/if}
        <label>
          <span>Keep the last</span>
          <input
            type="text"
            inputmode="numeric"
            class="num"
            min="1"
            max="100000"
            bind:value={form.retentionKeep}
            aria-invalid={!!errors.retentionKeep}
          />
          snapshots per project (pinned ones are always kept)
        </label>
        {#if errors.retentionKeep}<p class="field-error">{errors.retentionKeep}</p>{/if}
        <div class="row">
          <span>Data folder</span>
          <span class="mono path" title={dataDir}>{dataDir}</span>
          <button
            type="button"
            onclick={() => void store.api.revealInExplorer(dataDir).catch(() => {})}
            >Open folder</button
          >
        </div>
      </fieldset>

      {#if projectId !== null && store.projects.length > 0}
        <fieldset>
          <legend>
            Project
            <select
              aria-label="Project"
              value={projectId}
              onchange={(e) => pickProject(Number(e.currentTarget.value))}
            >
              {#each store.projects as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
            </select>
          </legend>
          <label class="block">
            <span>Also ignore (one pattern per line, .gitignore syntax)</span>
            <textarea
              rows="5"
              class="mono"
              bind:value={form.extraIgnore}
              placeholder={IGNORE_EXAMPLE}></textarea>
            <small>Always ignored: .git, node_modules, target, dist, .venv</small>
          </label>
          <label class="check">
            <input type="checkbox" bind:checked={form.respectGitignore} />
            Respect the project's .gitignore files
          </label>
          <div class="auto" role="radiogroup" aria-label="Auto-snapshot">
            <span>Auto-snapshot</span>
            <label class="check"
              ><input type="radio" value="off" bind:group={form.autoKind} /> Off</label
            >
            <label class="check">
              <input type="radio" value="every" bind:group={form.autoKind} /> Every
              <input
                type="text"
                inputmode="numeric"
                class="num"
                min="1"
                max="1440"
                bind:value={form.everyMinutes}
                disabled={form.autoKind !== 'every'}
                aria-label="Minutes"
                aria-invalid={!!errors.everyMinutes}
              />
              minutes (if something changed)
            </label>
            {#if errors.everyMinutes}<p class="field-error">{errors.everyMinutes}</p>{/if}
            <label class="check">
              <input type="radio" value="afterIdle" bind:group={form.autoKind} /> After
              <input
                type="text"
                inputmode="numeric"
                class="num"
                min="5"
                max="86400"
                bind:value={form.idleSeconds}
                disabled={form.autoKind !== 'afterIdle'}
                aria-label="Seconds"
                aria-invalid={!!errors.idleSeconds}
              />
              seconds without changes
            </label>
            {#if errors.idleSeconds}<p class="field-error">{errors.idleSeconds}</p>{/if}
          </div>
        </fieldset>
      {/if}

      <div class="actions">
        <button type="button" disabled={!dirty || saving} onclick={cancel}>Cancel</button>
        <button type="submit" class="primary" disabled={!dirty || !valid || saving}>Save</button>
      </div>
    </form>

    <fieldset>
      <legend>Claude Code hooks</legend>
      <p>
        Add this to <span class="mono">.claude/settings.json</span> to snapshot before and after
        every agent turn. It needs <span class="mono">dsnap</span> on your PATH.
      </p>
      <pre class="snippet">{HOOKS}</pre>
      <button type="button" onclick={() => void copyHooks()}>{copied ? 'Copied' : 'Copy'}</button>
    </fieldset>
  {:else}
    <p class="muted">Loading…</p>
  {/if}
</section>

{#if discard}
  {@const go = discard}
  <Dialog
    title="Discard unsaved settings?"
    confirmLabel="Discard"
    danger
    oncancel={() => (discard = null)}
    onconfirm={() => {
      const proceed = go;
      discard = null;
      if (saved) form = toForm(saved.global, saved.project);
      proceed();
    }}
  >
    <p>Your changes to the settings are not saved.</p>
  </Dialog>
{/if}

<style>
  .settings {
    overflow: auto;
    padding: var(--space-4) var(--space-5);
    max-width: 760px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  h2 {
    margin: 0;
    font-size: var(--text-xl);
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin: var(--space-4) 0;
    padding: var(--space-3) var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  legend {
    font-weight: 600;
    padding: 0 var(--space-1);
  }
  label,
  .row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  label.block {
    flex-direction: column;
    align-items: stretch;
  }
  .num {
    width: 90px;
  }
  textarea {
    font-family: var(--font-mono);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: var(--space-2);
  }
  small {
    color: var(--text-muted);
  }
  .auto {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .path {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
  .snippet {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: var(--space-2);
    overflow: auto;
    font-size: var(--text-sm);
  }
  .field-error,
  .error {
    color: var(--status-deleted);
    margin: 0;
    font-size: var(--text-sm);
  }
  .muted {
    color: var(--text-muted);
  }
</style>
