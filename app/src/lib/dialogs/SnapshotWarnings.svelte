<!--
  What the last snapshot left out (J2, F6): skipped files grouped by reason, and files that
  kept changing while they were captured.
-->
<script lang="ts">
  import type { RelPath, SkippedFile } from '../api';
  import { formatBytes, plural } from '../util/format';

  interface Props {
    skipped: SkippedFile[];
    unstable: RelPath[];
    ondismiss?: () => void;
  }
  let { skipped, unstable, ondismiss }: Props = $props();

  const groups = $derived.by(() => {
    const g = {
      tooLarge: [] as { path: string; note: string }[],
      locked: [] as { path: string; note: string }[],
      unreadable: [] as { path: string; note: string }[],
    };
    for (const s of skipped) {
      const r = s.reason;
      if (r.kind === 'tooLarge') g.tooLarge.push({ path: s.path, note: formatBytes(r.size) });
      else if (r.kind === 'locked') g.locked.push({ path: s.path, note: '' });
      else
        g.unreadable.push({
          path: s.path,
          note: r.kind === 'unreadable' ? r.msg : 'name is not valid Unicode',
        });
    }
    return g;
  });

  const titles = {
    tooLarge: 'Larger than the size cap',
    locked: 'Locked by another program',
    unreadable: 'Could not be read',
  } as const;
</script>

{#if skipped.length + unstable.length > 0}
  <div class="warnings" role="status" aria-label="Snapshot warnings">
    <div class="head">
      <strong>
        {#if skipped.length > 0}{plural(skipped.length, 'file')} left out of the last snapshot{/if}
        {#if skipped.length > 0 && unstable.length > 0}·{/if}
        {#if unstable.length > 0}{plural(unstable.length, 'file')} kept changing{/if}
      </strong>
      {#if ondismiss}
        <button type="button" class="close" aria-label="Dismiss" onclick={ondismiss}>×</button>
      {/if}
    </div>
    {#each Object.entries(groups) as [key, items] (key)}
      {#if items.length > 0}
        <details open={items.length <= 5}>
          <summary>{titles[key as keyof typeof titles]} ({items.length})</summary>
          <ul>
            {#each items.slice(0, 100) as it (it.path)}
              <li>
                <span class="mono">{it.path}</span>{#if it.note}
                  — {it.note}{/if}
              </li>
            {/each}
          </ul>
        </details>
      {/if}
    {/each}
    {#if unstable.length > 0}
      <details open={unstable.length <= 5}>
        <summary>Kept changing while captured ({unstable.length})</summary>
        <ul>
          {#each unstable.slice(0, 100) as p (p)}<li class="mono">{p}</li>{/each}
        </ul>
      </details>
    {/if}
  </div>
{/if}

<style>
  .warnings {
    padding: var(--space-2) var(--space-4);
    background: color-mix(in srgb, var(--status-modified) 12%, var(--bg));
    border-bottom: 1px solid var(--border);
    max-height: 200px;
    overflow: auto;
  }
  .head {
    display: flex;
    justify-content: space-between;
  }
  ul {
    margin: var(--space-1) 0;
    padding-left: var(--space-4);
  }
  summary {
    cursor: pointer;
  }
  .close {
    border: none;
    background: none;
  }
</style>
