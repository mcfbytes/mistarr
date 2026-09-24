<script lang="ts">
  import { onMount } from 'svelte';
  import { navigate } from '../lib/router.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { getDats, loadDats } from '../lib/stores/dats.svelte';
  import { getStatus, getWizard, loadStatus, loadWizard } from '../lib/stores/status.svelte';
  import { fixtureCores, fixtureDats, fixtureSettings } from '../lib/fixtures';
  import { getSources, loadSources } from '../lib/stores/sources.svelte';
  import { scheduleIncoming, type Watched } from '../lib/stores/incoming.svelte';
  import { addUpload } from '../lib/stores/uploads.svelte';
  import IncomingList from '../lib/IncomingList.svelte';
  import ClientStart from '../lib/ClientStart.svelte';
  import HeldBanner from '../lib/HeldBanner.svelte';
  import PathMapEditor, { cleanPathMap } from '../lib/PathMapEditor.svelte';
  import type { Settings } from '../lib/types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  let step = $state(0);
  const steps = ['Paths', 'DATs', 'Client', 'Sources'];

  let datFileInput = $state<HTMLInputElement>();
  let sourceFileInput = $state<HTMLInputElement>();
  let sourceMagnet = $state('');
  let settings = $state<Settings | null>(null);
  // `null` until a fresh POST /system/cores answer replaces the boot-time result.
  let coresResult = $state<string[] | null>(null);
  let detectingCores = $state(false);
  let coresChecked = false;
  let clientError = $state<string | null>(null);
  let clientSaved = $state(false);

  onMount(() => {
    void loadPlatforms();
    void loadDats();
    void loadStatus();
    void loadWizard();
    void loadSettings();
    void loadSources().catch(() => undefined);
  });

  const platforms = $derived(getPlatforms());
  const detectedCores = $derived(
    coresResult ?? platforms.filter((p) => p.core_present).map((p) => p.id)
  );

  $effect(() => {
    if (step === 0 && !coresChecked) {
      coresChecked = true;
      // Only auto-run when boot detection (already reflected in `platforms`) found nothing.
      if (!platforms.some((p) => p.core_present)) {
        void runCoreDetection();
      }
    }
  });

  async function runCoreDetection(): Promise<void> {
    if (isMock) {
      coresResult = fixtureCores.platforms;
      return;
    }
    detectingCores = true;
    try {
      const result = await api.cores();
      coresResult = result.platforms;
      await loadPlatforms();
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      detectingCores = false;
    }
  }

  async function loadSettings(): Promise<void> {
    settings = isMock ? fixtureSettings : await api.settings();
  }

  const dats = $derived(isMock ? fixtureDats : getDats());
  const status = $derived(getStatus());
  const wizard = $derived(getWizard());
  const sources = $derived(getSources());

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

  // Marks setup as seen so a reload lands on the library, not back here.
  async function finish(): Promise<void> {
    if (!isMock) {
      try {
        await api.wizardDone();
      } catch (err) {
        showToast(errorMessage(err));
      }
    }
    navigate('/');
  }

  function platformName(id: string): string {
    return platforms.find((p) => p.id === id)?.name ?? id;
  }

  // Upload only enqueues the import; the list below follows it to the end.
  async function upload(which: Watched, input: HTMLInputElement | undefined): Promise<void> {
    const files = Array.from(input?.files ?? []);
    if (isMock) {
      return;
    }
    for (const file of files) {
      try {
        const up = which === 'dats' ? await api.uploadDat(file) : await api.uploadSource(file);
        addUpload({ kind: which, file: up.file, jobId: up.job_id });
      } catch (err) {
        showToast(`${file.name}: ${errorMessage(err)}`);
      }
    }
    scheduleIncoming(which);
    if (input) {
      input.value = '';
    }
  }

  async function addSourceMagnet(): Promise<void> {
    const uri = sourceMagnet.trim();
    if (!uri || isMock) {
      return;
    }
    try {
      const up = await api.addMagnet(uri);
      addUpload({ kind: 'sources', file: up.file, jobId: up.job_id });
      scheduleIncoming('sources');
      sourceMagnet = '';
    } catch (err) {
      showToast(errorMessage(err));
    }
  }

  async function saveClientSettings(): Promise<void> {
    if (!settings) {
      return;
    }
    clientSaved = false;
    const cleaned = cleanPathMap(settings.client.remote_path_map);
    if ('error' in cleaned) {
      clientError = cleaned.error;
      return;
    }
    clientError = null;
    const client = { ...settings.client, remote_path_map: cleaned.map };
    try {
      settings = isMock ? { ...settings, client } : await api.putSettings({ client });
      clientSaved = true;
      if (!isMock) {
        await loadStatus();
      }
    } catch (err) {
      clientError = errorMessage(err);
    }
  }
</script>

<HeldBanner />
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
      <button type="button" onclick={runCoreDetection} disabled={detectingCores}>
        {detectingCores ? 'Detecting…' : 'Re-detect'}
      </button>
      <ul>
        {#each detectedCores as id (id)}
          <li>{platformName(id)}</li>
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
      <input
        bind:this={datFileInput}
        type="file"
        accept=".dat,.xml,.zip"
        multiple
        onchange={() => upload('dats', datFileInput)}
      />
      <p class="muted">Waiting in <code>dats/</code>:</p>
      <IncomingList which="dats" />
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
      <ClientStart />
      {#if settings}
        <h3>Remote path map</h3>
        <p class="muted">Only needed when the client sees its downloads under different paths.</p>
        <PathMapEditor bind:map={settings.client.remote_path_map} />
        <p>
          <button class="primary" onclick={saveClientSettings}>Save</button>
          {#if clientSaved}<span class="muted">Saved.</span>{/if}
        </p>
        {#if clientError}<p class="error">{clientError}</p>{/if}
      {/if}
    </section>
  {:else}
    <section class="card">
      <h2>Sources</h2>
      <p>Drop <code>.torrent</code> or <code>.magnet</code> files here, or place them in:</p>
      <p><code>/media/fat/mistarr/sources</code></p>
      <input
        bind:this={sourceFileInput}
        type="file"
        accept=".torrent"
        multiple
        onchange={() => upload('sources', sourceFileInput)}
      />
      <label>
        Or a magnet link
        <input type="text" placeholder="magnet:?xt=..." bind:value={sourceMagnet} />
      </label>
      <button onclick={addSourceMagnet}>Add</button>
      <p class="muted">Waiting in <code>sources/</code>:</p>
      <IncomingList which="sources" />
      <p class="muted">Added sources:</p>
      <ul>
        {#each sources as source (source.id)}
          <li>
            {source.display_name} — {source.platform_id ? platformName(source.platform_id) : source.state}
            {#if source.reason}<span class="muted">({source.reason})</span>{/if}
          </li>
        {:else}
          <li class="muted">None added yet.</li>
        {/each}
      </ul>
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

  .error {
    color: var(--danger);
  }

  li {
    overflow-wrap: anywhere;
  }

  .actions {
    display: flex;
    justify-content: space-between;
    margin-top: 1em;
  }
</style>
