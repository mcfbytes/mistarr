<script lang="ts">
  import { getStatus } from './stores/status.svelte';
  import StatusPill from './StatusPill.svelte';

  /** Shows that the client's uploads are held while a core runs; see docs/UI.md "Uploads while a game runs". */
  const paused = $derived(getStatus()?.uploads_paused === true);
</script>

{#if paused}
  <p class="uploads-paused" data-testid="uploads-paused">
    <StatusPill status="paused" label="Uploads paused while a game runs" />
  </p>
{/if}

<style>
  .uploads-paused {
    margin: 0.3em 0 0.6em;
  }
</style>
