<!--
  "Your edits" (DSNA-126): what changed between two turns, outside any agent turn, captured
  by the snapshot taken when the next prompt was submitted.
-->
<script lang="ts">
  import type { VersionId } from '../api';
  import { useStore } from '../stores/app.svelte';
  import { plural, whenText } from '../util/format';
  import ChangeCards from './ChangeCards.svelte';
  import Intro from './Intro.svelte';

  let { from, to }: { from: VersionId | null; to: VersionId } = $props();
  const store = useStore();
  const v = $derived(store.version(to));
  const next = $derived(store.turns.find((t) => t.before === to));
  const total = $derived(v ? v.counts.added + v.counts.modified + v.counts.deleted : 0);
</script>

{#if v}
  <section class="view" aria-labelledby="edits-title">
    <Intro
      chip="Your edits"
      meta={`${whenText(v.createdAtMs)}`}
      title={`You changed ${plural(total, 'file')} between turns.`}
      titleId="edits-title"
    >
      <p>
        Edits made outside an agent turn{#if next}, saved when you sent Turn {next.seq}'s prompt{/if}.
      </p>
      {#snippet actions()}
        <button type="button" onclick={() => store.startCompare(to)}>Compare with…</button>
      {/snippet}
    </Intro>
    <ChangeCards {from} to={{ kind: 'version', id: to }}>
      {#snippet actions(c)}
        {#if c.old && from !== null && !store.project?.missing}
          <button type="button" onclick={() => void store.restoreFile(from, c.old!.path)}
            >Bring back the version before</button
          >
        {/if}
      {/snippet}
    </ChangeCards>
  </section>
{/if}

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
</style>
