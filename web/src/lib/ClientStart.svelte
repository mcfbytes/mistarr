<script lang="ts">
  import { api, errorMessage } from './api';
  import { applyStatus, getStatus } from './stores/status.svelte';
  import type { ClientKind } from './types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  const client = $derived(getStatus()?.client ?? null);
  const canTransmission = $derived(
    !!client && (client.transmission_service || client.transmission_on_path)
  );
  const canRtorrent = $derived(!!client && client.rtorrent_on_path);
  let starting = $state<ClientKind | null>(null);
  let error = $state<string | null>(null);

  async function start(kind: ClientKind): Promise<void> {
    if (isMock) {
      return;
    }
    starting = kind;
    error = null;
    try {
      const status = await api.startClient(kind);
      applyStatus(status);
      if (!status.client?.reachable) {
        error = 'Started, but the client did not answer yet. It is checked again every minute.';
      }
    } catch (err) {
      error = errorMessage(err);
    } finally {
      starting = null;
    }
  }
</script>

{#if client && !client.reachable && (canTransmission || canRtorrent)}
  <div class="client-start">
    <p>A client is installed but not running.</p>
    {#if canTransmission}
      <button onclick={() => start('transmission')} disabled={starting !== null}>
        {starting === 'transmission' ? 'Starting…' : 'Start Transmission'}
      </button>
    {/if}
    {#if canRtorrent}
      <button onclick={() => start('rtorrent')} disabled={starting !== null}>
        {starting === 'rtorrent' ? 'Starting…' : 'Start rtorrent'}
      </button>
    {/if}
    {#if client.transmission_service && !client.transmission_opt_in}
      <p class="muted">
        Starting Transmission creates <code>/media/fat/linux/transmission</code>, so it also starts at every
        boot. Remove that directory to undo this.
      </p>
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
  </div>
{/if}

<style>
  .client-start {
    margin: 0.6em 0;
  }

  .client-start button {
    margin-right: 0.4em;
  }

  .error {
    color: var(--danger);
  }
</style>
