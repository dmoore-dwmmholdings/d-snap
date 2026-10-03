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
    left: var(--space-5);
    bottom: var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    z-index: 60;
    max-width: min(560px, calc(100vw - 48px));
  }
  .toast {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 10px 10px 10px 18px;
    border-radius: var(--radius-lg);
    background: var(--accent);
    color: var(--accent-text);
    box-shadow: var(--shadow-dialog);
    animation: rise 0.18s ease-out;
  }
  .toast.error {
    background: var(--status-deleted);
    color: #fff;
  }
  .toast.warning::before {
    content: '';
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--warn-strong);
  }
  .msg {
    flex: 1;
    overflow-wrap: anywhere;
  }
  .toast :global(button.link) {
    flex: none;
    min-height: 34px;
    padding: 0 12px;
    border-radius: var(--radius-sm);
    background: var(--marker);
    color: var(--marker-ink);
    font-weight: 600;
    text-decoration: none;
    white-space: nowrap;
  }
  .close {
    flex: none;
    width: 34px;
    min-height: 34px;
    border: none;
    background: none;
    color: inherit;
    opacity: 0.8;
    font-size: 18px;
    padding: 0;
  }
  .close:hover:not(:disabled) {
    background: rgb(255 255 255 / 12%);
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(8px);
    }
  }
</style>
