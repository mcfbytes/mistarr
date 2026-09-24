<script lang="ts">
  import { onMount } from 'svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import { fixtureSettings } from '../lib/fixtures';
  import { api } from '../lib/api';
  import type { Settings } from '../lib/types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  let settings = $state<Settings>({});
  let saved = $state(false);

  onMount(() => {
    void loadStatus();
    void loadSettings();
  });

  async function loadSettings(): Promise<void> {
    settings = isMock ? fixtureSettings : await api.settings();
  }

  const status = $derived(getStatus());

  async function togglePause(): Promise<void> {
    if (isMock) {
      return;
    }
    if (status?.paused) {
      await api.resume();
    } else {
      await api.pause();
    }
    await loadStatus();
  }

  async function save(): Promise<void> {
    if (!isMock) {
      settings = await api.putSettings(settings);
    }
    saved = true;
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

  .log {
    white-space: pre-wrap;
    font-size: 0.85em;
  }
</style>
