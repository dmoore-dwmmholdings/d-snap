<script lang="ts">
  import type { Toasts } from './toasts.svelte';

  let { toasts }: { toasts: Toasts } = $props();
</script>

<div class="toasts" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <div class="toast {t.kind}" role={t.kind === 'error' ? 'alert' : 'status'}>
      <span class="msg">{t.message}</span>
      {#if t.action}
        <button
          type="button"
          class="link"
          onclick={() => {
            t.action?.run();
            toasts.dismiss(t.id);
          }}>{t.action.label}</button
        >
      {/if}
      <button type="button" class="close" aria-label="Dismiss" onclick={() => toasts.dismiss(t.id)}
        >×</button
      >
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: var(--space-4);
    bottom: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    z-index: 60;
    max-width: min(440px, calc(100vw - 32px));
  }
  .toast {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    border-left-width: 4px;
    background: var(--surface-1);
    box-shadow: 0 4px 14px rgb(0 0 0 / 15%);
  }
  .success {
    border-left-color: var(--status-added);
  }
  .warning {
    border-left-color: var(--status-modified);
  }
  .error {
    border-left-color: var(--status-deleted);
  }
  .info {
    border-left-color: var(--accent);
  }
  .msg {
    flex: 1;
    overflow-wrap: anywhere;
  }
  .close {
    border: none;
    background: none;
    color: var(--text-muted);
    padding: 0 var(--space-1);
  }
</style>
