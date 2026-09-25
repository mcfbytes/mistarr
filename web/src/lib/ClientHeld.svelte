<script lang="ts">
  import { getStatus } from './stores/status.svelte';
  import StatusPill from './StatusPill.svelte';

  /** How the download client is held while a core runs; see docs/UI.md "The client while a core runs". */
  let { pillOnly = false }: { pillOnly?: boolean } = $props();

  const status = $derived(getStatus());
  const core = $derived(status?.corename ?? 'a core');
  const label = $derived(
    status?.client_hold === 'frozen'
      ? `Download client paused while ${core} is running`
      : `Uploads paused while ${core} is running`
  );
  // rtorrent reads a zero limit as none, so its uploads are held at 1 KiB/s.
  const detail = $derived(
    !pillOnly && status?.client_hold === 'uploads' && status.client?.kind === 'rtorrent'
      ? 'rtorrent holds uploads at 1 KiB/s, the lowest limit it keeps.'
      : null
  );
</script>

{#if status?.client_hold}
  <p class="client-held" data-testid={pillOnly ? 'client-held-pill' : 'client-held'} title={detail ?? undefined}>
    <StatusPill status="paused" {label} />
    {#if detail}<span class="muted detail">{detail}</span>{/if}
  </p>
{/if}

<style>
  .client-held {
    margin: 0.3em 0 0.6em;
  }

  .detail {
    margin-left: 0.4em;
    font-size: 0.85em;
  }
</style>
