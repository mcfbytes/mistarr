<script lang="ts">
  import { onMount } from 'svelte';
  import { navigate } from '../lib/router.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { getDats, loadDats } from '../lib/stores/dats.svelte';
  import { getStatus, getWizard, loadStatus, loadWizard } from '../lib/stores/status.svelte';
  import { fixtureDats, fixtureSettings } from '../lib/fixtures';
  import type { Settings } from '../lib/types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  let step = $state(0);
  const steps = ['Paths', 'DATs', 'Client', 'Sources'];

  let datFileInput = $state<HTMLInputElement>();
  let sourceFileInput = $state<HTMLInputElement>();
  let sourceMagnet = $state('');
  let settings = $state<Settings | null>(null);

  onMount(() => {
    void loadPlatforms();
    void loadDats();
    void loadStatus();
    void loadWizard();
    void loadSettings();
  });

  async function loadSettings(): Promise<void> {
    settings = isMock ? fixtureSettings : await api.settings();
  }

  const platforms = $derived(getPlatforms());
  const detectedCores = $derived(platforms.filter((p) => p.core_present));
  const dats = $derived(isMock ? fixtureDats : getDats());
  const status = $derived(getStatus());
  const wizard = $derived(getWizard());

  function next(): void {
    if (step < steps.length - 1) {
      step += 1;
    } else {
      finish();
    }
  }

  function skip(): void {
    next();
  }

  function finish(): void {
    navigate('/');
  }

  async function uploadDat(): Promise<void> {
    const file = datFileInput?.files?.[0];
    if (!file || isMock) {
      return;
    }
    try {
      await api.uploadDat(file);
      await loadDats();
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      if (datFileInput) {
        datFileInput.value = '';
      }
    }
  }

  async function uploadSource(): Promise<void> {
    const file = sourceFileInput?.files?.[0];
    if (!file || isMock) {
      return;
    }
    try {
      // Upload only enqueues the import; the wizard store updates itself
      // from the source.changed event once it lands.
      await api.uploadSource(file);
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      if (sourceFileInput) {
        sourceFileInput.value = '';
      }
    }
  }

  async function addSourceMagnet(): Promise<void> {
    const uri = sourceMagnet.trim();
    if (!uri || isMock) {
      return;
    }
    try {
      await api.addMagnet(uri);
      sourceMagnet = '';
    } catch (err) {
      showToast(errorMessage(err));
    }
  }

  async function saveClientSettings(): Promise<void> {
    if (!settings || isMock) {
      return;
    }
    try {
      settings = await api.putSettings({ client: settings.client });
      await loadStatus();
    } catch (err) {
      showToast(errorMessage(err));
    }
  }
</script>

<div class="page">
  <h1>First run</h1>
  <ol class="steps">
    {#each steps as label, i (label)}
      <li class:current={i === step} class:done={i < step}>{label}</li>
    {/each}
  </ol>

  {#if step === 0}
    <section class="card">
      <h2>Paths</h2>
      <p>Root directory: <code>/media/fat</code></p>
      <p>Games directory: <code>/media/fat/games</code></p>
      <p class="muted">Detected cores:</p>
      <ul>
        {#each detectedCores as p (p.id)}
          <li>{p.name}</li>
        {:else}
          <li class="muted">None detected yet.</li>
        {/each}
      </ul>
    </section>
  {:else if step === 1}
    <section class="card">
      <h2>DATs</h2>
      <p>Drop Logiqx DAT files or zipped DAT packs here, or place them in:</p>
      <p><code>/media/fat/mistarr/dats</code></p>
      <input bind:this={datFileInput} type="file" accept=".dat,.xml,.zip" onchange={uploadDat} />
      <p class="muted">Loaded DATs:</p>
      <ul>
        {#each dats as dat (dat.id)}
          <li>{dat.dat_name} — {dat.platform_id ?? 'unbound'}</li>
        {:else}
          <li class="muted">None loaded yet.</li>
        {/each}
      </ul>
    </section>
  {:else if step === 2}
    <section class="card">
      <h2>Client</h2>
      {#if status?.client}
        <p>
          Detection result: <strong>{status.client.kind ?? 'none found'}</strong>
          {status.client.reachable ? '(reachable)' : '(unreachable)'}
        </p>
      {:else}
        <p>Detection result: <strong>not run yet</strong></p>
      {/if}
      {#if settings}
        <h3>Remote path map</h3>
        {#each settings.client.remote_path_map as mapping, i (i)}
          <div class="mapping">
            <label>
              Remote path
              <input type="text" placeholder="/downloads" bind:value={mapping.remote} />
            </label>
            <label>
              Local path
              <input type="text" placeholder="/media/fat/mistarr/staging" bind:value={mapping.local} />
            </label>
          </div>
        {/each}
        <button
          type="button"
          onclick={() =>
            settings &&
            (settings.client.remote_path_map = [...settings.client.remote_path_map, { remote: '', local: '' }])}
        >
          Add mapping
        </button>
        <button class="primary" onclick={saveClientSettings}>Save</button>
      {/if}
    </section>
  {:else}
    <section class="card">
      <h2>Sources</h2>
      <p>Drop <code>.torrent</code> or <code>.magnet</code> files here, or place them in:</p>
      <p><code>/media/fat/mistarr/sources</code></p>
      <input bind:this={sourceFileInput} type="file" accept=".torrent" onchange={uploadSource} />
      <label>
        Or a magnet link
        <input type="text" placeholder="magnet:?xt=..." bind:value={sourceMagnet} />
      </label>
      <button onclick={addSourceMagnet}>Add</button>
      <h3>Seed policy</h3>
      <p>Each source keeps its own seeding setting. Default: <strong>none</strong>.</p>
      <ul>
        <li><strong>none</strong> — no seeding after a transfer completes.</li>
        <li><strong>until ratio</strong> — seed until a chosen ratio, then stop.</li>
        <li><strong>client default</strong> — leave it to the client's own setting.</li>
      </ul>
      {#if wizard?.sources}
        <p class="muted">At least one source is loaded.</p>
      {/if}
    </section>
  {/if}

  <div class="actions">
    <button onclick={skip}>Skip</button>
    {#if step === steps.length - 1}
      <button class="primary" onclick={finish}>Finish</button>
    {:else}
      <button class="primary" onclick={next}>Next</button>
    {/if}
  </div>
</div>

<style>
  .steps {
    display: flex;
    gap: 0.5em;
    list-style: none;
    padding: 0;
    flex-wrap: wrap;
  }

  .steps li {
    padding: 0.3em 0.7em;
    border-radius: 999px;
    background: var(--bg-raised);
    color: var(--fg-dim);
    border: 1px solid var(--border);
  }

  .steps li.current {
    color: var(--fg);
    border-color: var(--accent);
  }

  .steps li.done {
    color: var(--ok);
  }

  label {
    display: block;
    margin: 0.5em 0;
  }

  .mapping {
    display: flex;
    gap: 0.6em;
  }

  .actions {
    display: flex;
    justify-content: space-between;
    margin-top: 1em;
  }
</style>
