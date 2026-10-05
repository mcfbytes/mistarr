<script lang="ts">
  import { onMount } from 'svelte';
  import { loadStatus } from '../lib/stores/status.svelte';
  import AboutCard from '../lib/AboutCard.svelte';
  import SettingsForm from '../lib/SettingsForm.svelte';
  import StatusTiles from '../lib/StatusTiles.svelte';

  onMount(() => {
    // The board's own status can't block the settings form; SSE brings it once connected.
    void loadStatus().catch(() => undefined);
  });
</script>

<div class="page system">
  <h1>System</h1>
  <StatusTiles />
  <AboutCard />
  <SettingsForm />
</div>

<style>
  h1 {
    margin: 0.2em 0 0.6em;
  }
</style>
