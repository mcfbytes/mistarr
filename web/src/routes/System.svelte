<script lang="ts">
  import { onMount } from 'svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import AboutCard from '../lib/AboutCard.svelte';
  import SettingsForm from '../lib/SettingsForm.svelte';
  import StatusTiles from '../lib/StatusTiles.svelte';

  const notice = $derived.by(() => {
    const status = getStatus();
    if (!status || status.dat_import_in_ram.possible) {
      return null;
    }
    const i = status.dat_import_in_ram;
    let text =
      'DAT imports run on the card, which is slower: a copy of the database in memory needs ' +
      `${i.need_mib} MiB plus ${i.floor_mib} MiB kept free, and ${i.available_mib} MiB is free.`;
    if (status.card_sync_mount) {
      text +=
        ' The SD card is mounted with sync, which makes imports on the card slower still.' +
        ' Mounting it without sync speeds them up, but more recent changes can be lost if the power is cut.';
    }
    return text;
  });

  onMount(() => {
    // The board's own status can't block the settings form; SSE brings it once connected.
    void loadStatus().catch(() => undefined);
  });
</script>

<div class="page system">
  <h1>System</h1>
  <StatusTiles />
  {#if notice}<p class="notice" role="status">{notice}</p>{/if}
  <AboutCard />
  <SettingsForm />
</div>

<style>
  h1 {
    margin: 0.2em 0 0.6em;
  }

  .notice {
    margin: 0 0 1rem;
    padding: 0.6em 0.8em;
    background: var(--bg-raised);
    border: 1px solid var(--warn);
    border-radius: var(--radius);
    font-size: 0.9rem;
    overflow-wrap: anywhere;
  }
</style>
