<script lang="ts">
  import { onMount } from 'svelte';
  import { getWizard, loadWizard } from './stores/status.svelte';

  onMount(() => {
    void loadWizard().catch(() => undefined);
  });

  const wizard = $derived(getWizard());
  const missing = $derived(
    wizard
      ? [
          !wizard.dats && 'Load a DAT so titles can be listed.',
          !wizard.client && 'Connect a download client.',
          !wizard.sources && 'Add a source.'
        ].filter((s): s is string => !!s)
      : []
  );
</script>

{#if missing.length > 0}
  <section class="card hints">
    <h2>Setup not finished</h2>
    <ul>
      {#each missing as item (item)}
        <li>{item}</li>
      {/each}
    </ul>
    <a href="#/wizard">Open setup</a>
  </section>
{/if}

<style>
  .hints {
    margin-bottom: 1em;
  }

  .hints h2 {
    margin-top: 0;
    font-size: 1.1em;
  }
</style>
