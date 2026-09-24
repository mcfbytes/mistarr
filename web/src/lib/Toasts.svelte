<script lang="ts">
  import { dismissToast, getToasts } from './stores/toast.svelte';

  const toasts = $derived(getToasts());
</script>

{#if toasts.length > 0}
  <div class="toasts">
    {#each toasts as toast (toast.id)}
      <button type="button" class="toast" onclick={() => dismissToast(toast.id)}>
        {toast.text}
      </button>
    {/each}
  </div>
{/if}

<style>
  .toasts {
    position: fixed;
    left: 16px;
    right: 16px;
    bottom: 16px;
    display: flex;
    flex-direction: column;
    gap: 0.5em;
    z-index: 10;
  }

  .toast {
    background: var(--danger);
    color: #fff;
    border: none;
    text-align: left;
    padding: 0.6em 0.9em;
    border-radius: var(--radius);
    font-size: 0.9em;
    cursor: pointer;
  }
</style>
