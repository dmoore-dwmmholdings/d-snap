<!--
  Modal dialog (J2). Escape or the backdrop cancels; focus moves into the dialog on open and
  stays there (Tab cycles); the confirm button is focused unless `danger`.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    confirmLabel?: string;
    cancelLabel?: string;
    /** Red confirm button; Cancel gets the initial focus. */
    danger?: boolean;
    /** Disables the confirm button. */
    busy?: boolean;
    onconfirm: () => void;
    oncancel: () => void;
    children: Snippet;
  }

  let {
    title,
    confirmLabel = 'OK',
    cancelLabel = 'Cancel',
    danger = false,
    busy = false,
    onconfirm,
    oncancel,
    children,
  }: Props = $props();

  let box: HTMLDivElement | undefined = $state();
  const titleId = `dlg-${Math.random().toString(36).slice(2)}`;

  $effect(() => {
    const prev = document.activeElement as HTMLElement | null;
    const target = box?.querySelector<HTMLElement>(danger ? '[data-cancel]' : '[data-confirm]');
    target?.focus();
    return () => prev?.focus?.();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      oncancel();
    } else if (e.key === 'Tab' && box) {
      const f = [
        ...box.querySelectorAll<HTMLElement>('button, input, select, textarea, [tabindex]'),
      ].filter((el) => !el.hasAttribute('disabled') && el.tabIndex >= 0);
      if (f.length === 0) return;
      const first = f[0]!;
      const last = f[f.length - 1]!;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="backdrop" onclick={(e) => e.target === e.currentTarget && oncancel()} {onkeydown}>
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby={titleId} bind:this={box}>
    <h2 id={titleId}>{title}</h2>
    <div class="body">{@render children()}</div>
    <div class="actions">
      <button type="button" data-cancel onclick={oncancel}>{cancelLabel}</button>
      <button
        type="button"
        data-confirm
        class:primary={!danger}
        class:danger
        disabled={busy}
        onclick={onconfirm}>{confirmLabel}</button
      >
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 35%);
    display: grid;
    place-items: center;
    z-index: 50;
  }
  .dialog {
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: 0 10px 30px rgb(0 0 0 / 25%);
    width: min(560px, calc(100vw - 32px));
    max-height: calc(100vh - 64px);
    display: flex;
    flex-direction: column;
    padding: var(--space-4);
    gap: var(--space-3);
  }
  h2 {
    margin: 0;
    font-size: var(--text-lg);
  }
  .body {
    overflow: auto;
    min-height: 0;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
