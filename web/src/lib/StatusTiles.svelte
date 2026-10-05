<script lang="ts">
  import { applyStatus, getStatus } from './stores/status.svelte';
  import { api, errorMessage } from './api';
  import ClientHeld from './ClientHeld.svelte';
  import ClientStart from './ClientStart.svelte';
  import Meter from './Meter.svelte';
  import StatusPill from './StatusPill.svelte';
  import { bytesText } from './format';
  import { uptimeText, usedFraction } from './system';

  /** The board's status as tiles: MiSTer, download client, scheduler, memory, storage and uptime. */
  const LAUNCH_TEXT = { ready: 'Ready', disabled: 'Off in settings', unavailable: 'Unavailable here' };

  let statusError = $state<string | null>(null);

  const status = $derived(getStatus());
  const memUsed = $derived(status ? usedFraction(status.mem_available_bytes, status.mem_total_bytes) : null);
  const diskUsed = $derived(status ? usedFraction(status.disk_free_bytes, status.disk_total_bytes) : null);
  const since = $derived(
    status
      ? new Date(Math.floor((Date.now() - status.uptime_secs * 1000) / 60_000) * 60_000).toLocaleString(undefined, {
          dateStyle: 'medium',
          timeStyle: 'short'
        })
      : ''
  );

  async function togglePause(): Promise<void> {
    statusError = null;
    try {
      applyStatus(await (status?.paused ? api.resume() : api.pause()));
    } catch (err) {
      statusError = errorMessage(err);
    }
  }
</script>

{#if status}
  <section aria-labelledby="status-h">
    <h2 id="status-h" class="visually-hidden">Status</h2>
    <ul class="tiles" aria-label="Status">
      <li class="tile">
        <h3>MiSTer</h3>
        <p class="big">
          {#if status.corename === null}Not detected{:else if status.corename === 'MENU'}At the menu{:else}{status.corename}{/if}
        </p>
        <p class="sub">
          {#if status.corename !== null && status.corename !== 'MENU'}Core running.{/if}
          Launching: {LAUNCH_TEXT[status.launch]}
        </p>
        <ClientHeld pillOnly />
      </li>

      <li class="tile">
        <div class="tile-head">
          <h3>Download client</h3>
          {#if !status.client?.kind}
            <StatusPill status="queued" label="Not detected" />
          {:else if status.client.reachable}
            <StatusPill status="done" label="Reachable" />
          {:else}
            <StatusPill status="failed" label="Not reachable" />
          {/if}
        </div>
        <p class="big">
          {status.client?.kind ?? 'None'}
          {#if status.client?.version}<span class="dim">{status.client.version}</span>{/if}
        </p>
        {#if status.client?.url}<p class="sub mono">{status.client.url}</p>{/if}
        <ClientHeld />
        <ClientStart />
      </li>

      <li class="tile">
        <div class="tile-head">
          <h3>Scheduler</h3>
          {#if !status.paused}
            <StatusPill status="running" label="Running" />
          {:else if status.pause_reason === 'core'}
            <StatusPill status="paused" label="Held for the core" />
          {:else}
            <StatusPill status="paused" label="Paused" />
          {/if}
        </div>
        <p class="big">
          {status.waiting.length === 0
            ? 'Nothing waiting'
            : `${status.waiting.length} ${status.waiting.length === 1 ? 'job' : 'jobs'} waiting`}
        </p>
        <div class="tile-action">
          <button onclick={togglePause}>
            {status.paused ? (status.pause_reason === 'core' ? 'Run now' : 'Resume') : 'Pause'}
          </button>
        </div>
        {#if statusError}<p class="error sub">{statusError}</p>{/if}
      </li>

      <li class="tile">
        <h3>Memory</h3>
        <p class="big">{bytesText(status.rss_bytes)} <span class="dim">used by mistarr</span></p>
        {#if memUsed !== null}
          <Meter
            fraction={memUsed}
            label="Board memory in use"
            text={`${bytesText(status.mem_available_bytes)} available of ${bytesText(status.mem_total_bytes)}`}
          />
          <p class="sub">{bytesText(status.mem_available_bytes)} available of {bytesText(status.mem_total_bytes)}</p>
        {/if}
      </li>

      <li class="tile">
        <h3>Storage</h3>
        <p class="big">{bytesText(status.disk_free_bytes)} <span class="dim">free</span></p>
        {#if diskUsed !== null}
          <Meter
            fraction={diskUsed}
            warnAt={0.9}
            label="Storage in use"
            text={`${bytesText(status.disk_free_bytes)} free of ${bytesText(status.disk_total_bytes)}`}
          />
          <p class="sub">of {bytesText(status.disk_total_bytes)}, where the data directory is</p>
        {/if}
      </li>

      <li class="tile">
        <h3>Uptime</h3>
        <p class="big">{uptimeText(status.uptime_secs)}</p>
        <p class="sub">Since {since}</p>
      </li>
    </ul>
  </section>
{/if}

<style>
  .tiles {
    list-style: none;
    margin: 0 0 1rem;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.6rem;
  }

  @media (min-width: 720px) {
    .tiles {
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 0.75rem;
    }
  }

  .tile {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    min-width: 0;
    padding: 0.75rem 0.85rem;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .tile-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.3rem 0.5rem;
  }

  .tile h3 {
    margin: 0;
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--fg-dim);
  }

  .big {
    margin: 0;
    font-size: 1.2rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }

  .dim {
    font-size: 0.8rem;
    font-weight: 400;
    color: var(--fg-dim);
  }

  .sub {
    margin: 0;
    font-size: 0.82rem;
    color: var(--fg-dim);
    overflow-wrap: anywhere;
  }

  .mono {
    font-family: var(--mono);
    font-size: 0.78rem;
  }

  .tile-action {
    margin-top: auto;
  }

  .tile-action button {
    padding: 0.25em 0.7em;
    font-size: 0.85rem;
  }

  .tile :global(.client-held) {
    margin: 0;
  }

  .tile :global(.client-held .pill) {
    white-space: normal;
  }

  .tile :global(.client-start) {
    font-size: 0.85rem;
  }
</style>
