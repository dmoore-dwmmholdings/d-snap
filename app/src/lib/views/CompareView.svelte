<!--
  Compare any two entries (F16, DSNA-126): older → newer, or a snapshot → the folder now.
-->
<script lang="ts">
  import type { VersionId } from '../api';
  import { useStore } from '../stores/app.svelte';
  import { absoluteTime } from '../util/format';
  import ChangeCards from './ChangeCards.svelte';
  import Intro from './Intro.svelte';

  let { from, to }: { from: VersionId; to: VersionId | null } = $props();
  const store = useStore();
  const a = $derived(store.version(from));
  const b = $derived(store.version(to));
  const name = (v: typeof a) => (v ? `“${v.label}”` : 'a deleted snapshot');
</script>

<section class="view" aria-labelledby="compare-title">
  <Intro
    chip="Compare"
    meta={a
      ? `A: ${absoluteTime(a.createdAtMs)}${b ? ` · B: ${absoluteTime(b.createdAtMs)}` : ' · B: the folder now'}`
      : ''}
    title={`${name(a)} → ${to === null ? 'now' : name(b)}`}
    titleId="compare-title"
  >
    {#snippet actions()}
      {#if to !== null}
        <button type="button" onclick={() => store.select({ kind: 'compare', from: to, to: from })}
          >Swap A ⇄ B</button
        >
      {/if}
      <button type="button" onclick={() => store.select({ kind: 'version', id: to ?? from })}
        >Done</button
      >
    {/snippet}
  </Intro>
  <ChangeCards
    {from}
    to={to === null ? { kind: 'workingTree' } : { kind: 'version', id: to }}
    empty="These two are the same."
  >
    {#snippet actions(c)}
      {#if !store.project?.missing}
        {#if c.old}
          <button type="button" onclick={() => void store.restoreFile(from, c.old!.path)}
            >Bring back A</button
          >
        {/if}
        {#if c.new && to !== null}
          <button type="button" onclick={() => void store.restoreFile(to, c.path)}
            >Bring back B</button
          >
        {/if}
      {/if}
    {/snippet}
  </ChangeCards>
</section>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-5);
  }
</style>
