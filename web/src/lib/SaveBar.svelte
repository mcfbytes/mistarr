<script lang="ts">
  /**
   * The bar of a form with unsaved changes: the error or "Unsaved changes", Discard and a
   * submit button for the form around it. It lifts the toasts above itself while it shows.
   */
  let {
    error = null,
    saving = false,
    ondiscard
  }: { error?: string | null; saving?: boolean; ondiscard: () => void } = $props();

  let height = $state(0);

  $effect(() => {
    if (height === 0) {
      return;
    }
    const root = document.documentElement;
    root.style.setProperty('--toast-lift', `${height + 12}px`);
    return () => {
      root.style.removeProperty('--toast-lift');
    };
  });
</script>

<div class="savebar" role="region" aria-label="Unsaved changes" bind:clientHeight={height}>
  {#if error}
    <p class="error" role="alert">{error}</p>
  {:else}
    <p>Unsaved changes</p>
  {/if}
  <div class="actions">
    <button type="button" onclick={ondiscard} disabled={saving}>Discard</button>
    <button type="submit" class="primary" disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
  </div>
</div>

<style>
  .savebar {
    position: sticky;
    bottom: 0.75rem;
    z-index: 2;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem 1rem;
    padding: 0.6rem 0.8rem;
    background: var(--bg-raised);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    box-shadow: 0 6px 20px rgb(0 0 0 / 25%);
  }

  /* Without overflow-x: clip, body scrolls and sticky cannot follow the window. */
  @supports not (overflow-x: clip) {
    .savebar {
      position: fixed;
      right: var(--gutter);
      bottom: var(--gutter);
      left: var(--gutter);
      max-width: 60rem;
      margin: 0 auto;
    }
  }

  p {
    margin: 0;
    font-weight: 500;
  }

  .actions {
    display: flex;
    gap: 0.5rem;
    margin-left: auto;
  }
</style>
