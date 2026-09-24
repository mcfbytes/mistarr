<script lang="ts">
  import { onMount } from 'svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import { fixtureSettings } from '../lib/fixtures';
  import { api, errorMessage } from '../lib/api';
  import type { Settings } from '../lib/types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  type SettingType = 'string' | 'number' | 'boolean';

  let settings = $state<Settings>({});
  let settingTypes = $state<Record<string, SettingType>>({});
  let saved = $state(false);
  let settingsError = $state<string | null>(null);
  let statusError = $state<string | null>(null);

  onMount(() => {
    void loadStatus();
    void loadSettings();
  });

  async function loadSettings(): Promise<void> {
    const loaded = isMock ? fixtureSettings : await api.settings();
    settings = loaded;
    settingTypes = Object.fromEntries(
      Object.entries(loaded).map(([key, value]) => [key, typeof value as SettingType])
    );
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

  function convert(key: string, raw: string | number | boolean): string | number | boolean {
    const type = settingTypes[key] ?? 'string';
    if (type === 'number') {
      return Number(raw);
    }
    if (type === 'boolean') {
      return raw === true || raw === 'true';
    }
    return String(raw);
  }

  async function save(): Promise<void> {
    settingsError = null;
    saved = false;
    const payload: Settings = {};
    for (const [key, value] of Object.entries(settings)) {
      payload[key] = convert(key, value);
    }
    try {
      settings = isMock ? payload : await api.putSettings(payload);
      saved = true;
    } catch (err) {
      settingsError = errorMessage(err);
    }
  }
</script>

<div class="page">
  <h1>System</h1>

  {#if status}
    <div class="card">
      <p>Version {status.version}</p>
      <p>Uptime {Math.round(status.uptime / 60)} minutes</p>
      <p>Client: {status.client_kind} — {status.client_reachable ? 'reachable' : 'unreachable'}</p>
      <p>CORENAME: {status.corename ?? 'none'}</p>
      <p>Disk free: {(status.disk_free / 1_000_000_000).toFixed(1)} GB</p>
      <p>Memory: {(status.rss / 1_000_000).toFixed(0)} MB</p>
      <p>
        Scheduler: {status.paused ? 'paused' : 'running'}
        <button onclick={togglePause}>{status.paused ? 'Resume' : 'Pause'}</button>
      </p>
      {#if statusError}<p class="error">{statusError}</p>{/if}
    </div>
  {/if}

  <h2>Settings</h2>
  <form class="card" onsubmit={(e) => e.preventDefault()}>
    {#each Object.entries(settings) as [key, value] (key)}
      <label>
        {key}
        <input
          type="text"
          value={String(value)}
          oninput={(e) => (settings = { ...settings, [key]: (e.currentTarget as HTMLInputElement).value })}
        />
      </label>
    {/each}
    <button class="primary" onclick={save}>Save</button>
    {#if saved}<span class="muted">Saved.</span>{/if}
    {#if settingsError}<p class="error">{settingsError}</p>{/if}
  </form>

  <h2>Log tail</h2>
  <pre class="card log">server started
scan: nes complete
scheduler: idle</pre>
</div>

<style>
  label {
    display: block;
    margin: 0.4em 0;
  }

  .error {
    color: var(--danger);
  }

  .log {
    white-space: pre-wrap;
    font-size: 0.85em;
  }
</style>
