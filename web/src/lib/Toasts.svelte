<script lang="ts">
  import { dismissToast, getToasts } from './stores/toast.svelte';
  import StatusPill from './StatusPill.svelte';

  const toasts = $derived(getToasts());
</script>

<div class="toasts" aria-live="polite">
  {#each toasts as toast (toast.id)}
    <div class="toast {toast.tone}" role={toast.tone === 'error' ? 'alert' : undefined}>
      {#if toast.tone === 'success'}
        <StatusPill status="done" />
      {:else if toast.tone === 'error'}
        <StatusPill status="failed" />
      {/if}
      <span class="text">{toast.text}</span>
      <button type="button" class="close" aria-label="Dismiss" onclick={() => dismissToast(toast.id)}>
        <svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true">
          <path d="M3 3l6 6M9 3l-6 6" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
        </svg>
      </button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: var(--gutter);
    bottom: var(--gutter);
    left: var(--gutter);
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0.5em;
    z-index: 30;
    pointer-events: none;
  }

  .toast {
    display: flex;
    align-items: center;
    gap: 0.6em;
    width: min(100%, 26rem);
    padding: 0.55em 0.5em 0.55em 0.8em;
    border: 1px solid var(--border);
    border-left: 4px solid var(--accent);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.25);
    font-size: 0.9em;
    pointer-events: auto;
    animation: rise 0.18s ease-out;
  }

  .toast.success {
    border-left-color: var(--ok);
  }

  .toast.error {
    border-left-color: var(--danger);
  }

  .text {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .close {
    flex: none;
    display: grid;
    place-items: center;
    width: 2rem;
    height: 2rem;
    padding: 0;
    border-color: transparent;
    background: transparent;
    color: var(--fg-dim);
  }

  @keyframes rise {
    from {
      transform: translateY(6px);
      opacity: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .toast {
      animation: none;
    }
  }
</style>
