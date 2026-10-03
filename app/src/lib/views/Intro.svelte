<!--
  Heading block of a main-area view (DSNA-124): a chip and meta line, the big title, an
  optional paragraph, and the view's main actions.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    chip: string;
    chipClass?: string;
    meta?: string;
    title: string;
    titleId?: string;
    children?: Snippet;
    actions?: Snippet;
  }
  let { chip, chipClass = '', meta, title, titleId, children, actions }: Props = $props();
</script>

<div class="intro">
  <div class="eyebrow">
    <span class="chip {chipClass}">{chip}</span>
    {#if meta}<span>{meta}</span>{/if}
  </div>
  <h1 id={titleId}>{title}</h1>
  {#if children}<div class="body">{@render children()}</div>{/if}
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</div>

<style>
  .intro {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    max-width: 820px;
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
    overflow-wrap: anywhere;
    font-variation-settings: 'opsz' 96;
  }
  .body {
    font-size: 15px;
    color: var(--text-muted);
    max-width: 66ch;
  }
  .body :global(p) {
    margin: 0;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 14px;
    margin-top: var(--space-2);
  }
</style>
