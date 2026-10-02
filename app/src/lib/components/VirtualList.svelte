<!--
  Renders only the visible rows of a long list (fixed row height). `scrollToIndex` keeps a
  row in view, e.g. the keyboard selection.
-->
<script lang="ts" generics="T">
  import type { Snippet } from 'svelte';

  interface Props {
    items: T[];
    rowHeight: number;
    /** Row to keep visible. */
    activeIndex?: number;
    overscan?: number;
    label?: string;
    row: Snippet<[T, number]>;
  }

  let { items, rowHeight, activeIndex = -1, overscan = 8, label, row }: Props = $props();

  let viewport: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let height = $state(0);

  // Without layout (tests, first paint) assume a tall window so rows still render.
  const visibleHeight = $derived(height > 0 ? height : 1200);
  const start = $derived(Math.max(0, Math.floor(scrollTop / rowHeight) - overscan));
  const end = $derived(
    Math.min(items.length, Math.ceil((scrollTop + visibleHeight) / rowHeight) + overscan),
  );

  $effect(() => {
    const i = activeIndex;
    if (!viewport || i < 0) return;
    const top = i * rowHeight;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (
      top + rowHeight > viewport.scrollTop + viewport.clientHeight &&
      viewport.clientHeight > 0
    )
      viewport.scrollTop = top + rowHeight - viewport.clientHeight;
  });
</script>

<div
  class="viewport"
  bind:this={viewport}
  bind:clientHeight={height}
  onscroll={() => (scrollTop = viewport?.scrollTop ?? 0)}
  aria-label={label}
>
  <div class="spacer" style:height="{items.length * rowHeight}px">
    <div class="rows" style:transform="translateY({start * rowHeight}px)">
      {#each items.slice(start, end) as item, i (start + i)}
        <div class="row" style:height="{rowHeight}px">{@render row(item, start + i)}</div>
      {/each}
    </div>
  </div>
</div>

<style>
  .viewport {
    overflow: auto;
    height: 100%;
    min-height: 0;
  }
  .spacer {
    position: relative;
  }
  .rows {
    position: absolute;
    inset: 0 0 auto 0;
  }
  .row {
    overflow: hidden;
  }
</style>
