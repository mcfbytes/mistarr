<script lang="ts">
  import { onMount } from 'svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import { fixtureSettings } from '../lib/fixtures';
  import { api, errorMessage } from '../lib/api';
  import type { Settings } from '../lib/types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  let settings = $state<Settings | null>(null);
  let saved = $state(false);
  let settingsError = $state<string | null>(null);
  let statusError = $state<string | null>(null);

  onMount(() => {
    void loadStatus();
    void loadSettings();
  });

  async function loadSettings(): Promise<void> {
    settings = isMock ? fixtureSettings : await api.settings();
  }

  const status = $derived(getStatus());

  async function togglePause(): Promise<void> {
    statusError = null;
    if (isMock) {
      return;
    }
    try {
      if (status?.paused) {
        await api.resume();
      } else {
        await api.pause();
      }
      await loadStatus();
    } catch (err) {
      statusError = errorMessage(err);
    }
  }

  function csv(xs: string[]): string {
    return xs.join(', ');
  }

  function fromCsv(text: string): string[] {
    return text
      .split(',')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  }

  function addMapping(): void {
    if (!settings) {
      return;
    }
    settings = {
      ...settings,
      client: {
        ...settings.client,
        remote_path_map: [...settings.client.remote_path_map, { remote: '', local: '' }]
      }
    };
  }

  function removeMapping(index: number): void {
    if (!settings) {
      return;
    }
    settings = {
      ...settings,
      client: {
        ...settings.client,
        remote_path_map: settings.client.remote_path_map.filter((_, i) => i !== index)
      }
    };
  }

  async function save(): Promise<void> {
    if (!settings) {
      return;
    }
    settingsError = null;
    saved = false;
    try {
      settings = isMock ? settings : await api.putSettings(settings);
      saved = true;
    } catch (err) {
      settingsError = errorMessage(err);
      return;
    }
    if (!isMock) {
      // A failed refresh leaves the card as it was; SSE brings the next status.
      await loadStatus().catch(() => undefined);
    }
  }
</script>

<div class="page">
  <h1>System</h1>

  {#if status}
    <div class="card">
      <p>Version {status.version}</p>
      <p>Uptime {Math.round(status.uptime_secs / 60)} minutes</p>
      <p>
        Client: {status.client?.kind ?? 'none'} —
        {status.client?.reachable ? 'reachable' : 'unreachable'}
      </p>
      <p>CORENAME: {status.corename ?? 'none'}</p>
      <p>Launching: {status.launch}</p>
      <p>Disk free: {status.disk_free_bytes ? (status.disk_free_bytes / 1_000_000_000).toFixed(1) : '—'} GB</p>
      <p>Memory: {status.rss_bytes ? (status.rss_bytes / 1_000_000).toFixed(0) : '—'} MB</p>
      <p>
        Scheduler: {status.paused ? 'paused' : 'running'}
        {#if status.pause_reason === 'core'}<span class="muted">(held for the running core)</span>{/if}
        <button onclick={togglePause}>{status.paused ? 'Resume' : 'Pause'}</button>
      </p>
      {#if statusError}<p class="error">{statusError}</p>{/if}
    </div>
  {/if}

  {#if settings}
    <h2>Settings</h2>
    <form class="card settings" onsubmit={(e) => e.preventDefault()}>
      <h3>Client</h3>
      <label>
        Kind
        <select bind:value={settings.client.kind}>
          <option value="auto">Auto-detect</option>
          <option value="transmission">Transmission</option>
          <option value="rtorrent">rtorrent</option>
        </select>
      </label>
      <label>
        URL
        <input type="text" placeholder="http://127.0.0.1:9091/transmission/rpc" bind:value={settings.client.url} />
      </label>
      <p class="muted">Remote path map</p>
      {#each settings.client.remote_path_map as mapping, i (i)}
        <div class="mapping">
          <input type="text" placeholder="Remote path" bind:value={mapping.remote} />
          <input type="text" placeholder="Local path" bind:value={mapping.local} />
          <button type="button" onclick={() => removeMapping(i)}>Remove</button>
        </div>
      {/each}
      <button type="button" onclick={addMapping}>Add mapping</button>

      <h3>Limits (kbps, 0 is unlimited)</h3>
      <label>
        Download at menu
        <input type="number" min="0" bind:value={settings.limits.down_kbps_menu} />
      </label>
      <label>
        Download while a core runs
        <input type="number" min="0" bind:value={settings.limits.down_kbps_core} />
      </label>
      <label>
        Upload at menu
        <input type="number" min="0" bind:value={settings.limits.up_kbps_menu} />
      </label>
      <label>
        Upload while a core runs
        <input type="number" min="0" bind:value={settings.limits.up_kbps_core} />
      </label>

      <h3>1G1R preferences</h3>
      <label>
        Region order
        <input
          type="text"
          value={csv(settings.prefs.regions)}
          oninput={(e) => settings && (settings.prefs.regions = fromCsv((e.currentTarget as HTMLInputElement).value))}
        />
      </label>
      <label>
        Language order
        <input
          type="text"
          value={csv(settings.prefs.languages)}
          oninput={(e) =>
            settings && (settings.prefs.languages = fromCsv((e.currentTarget as HTMLInputElement).value))}
        />
      </label>
      <label>
        <input type="checkbox" bind:checked={settings.prefs.prefer_latest_revision} />
        Prefer the highest revision
      </label>
      <label>
        Hidden flags
        <input
          type="text"
          value={csv(settings.prefs.hide)}
          oninput={(e) => settings && (settings.prefs.hide = fromCsv((e.currentTarget as HTMLInputElement).value))}
        />
      </label>

      <h3>Launching</h3>
      <label>
        <input type="checkbox" bind:checked={settings.prefs.launch} />
        Allow starting cores and games from mistarr
      </label>

      <button class="primary" onclick={save}>Save</button>
      {#if saved}<span class="muted">Saved.</span>{/if}
      {#if settingsError}<p class="error">{settingsError}</p>{/if}
    </form>
  {/if}
</div>

<style>
  label {
    display: block;
    margin: 0.4em 0;
  }

  .settings h3 {
    margin-top: 1em;
  }

  .mapping {
    display: flex;
    gap: 0.4em;
    margin: 0.3em 0;
  }

  .error {
    color: var(--danger);
  }
</style>
