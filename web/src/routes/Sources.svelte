<script lang="ts">
  import { onMount } from 'svelte';
  import { getSources, loadSources, patchSource } from '../lib/stores/sources.svelte';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { scheduleIncoming } from '../lib/stores/incoming.svelte';
  import { addUpload } from '../lib/stores/uploads.svelte';
  import IncomingList from '../lib/IncomingList.svelte';
  import type { SeedPolicy } from '../lib/types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  onMount(() => {
    void loadSources();
    void loadPlatforms();
  });

  const sources = $derived(getSources());
  const platforms = $derived(getPlatforms());

  let magnet = $state('');
  let fileInput: HTMLInputElement | undefined;

  // The server may format a ratio as "1.0"; compare the parsed number so
  // the select shows the matching option regardless of formatting.
  function seedSelectValue(policy: string): string {
    const ratio = policy.startsWith('ratio:') ? parseFloat(policy.slice('ratio:'.length)) : null;
    return ratio !== null && Number.isFinite(ratio) ? `ratio:${ratio}` : policy;
  }

  async function bind(id: number, platformId: string): Promise<void> {
    if (!platformId) {
      return;
    }
    const prev = sources.find((s) => s.id === id);
    patchSource(id, { platform_id: platformId, state: 'bound' });
    if (isMock) {
      return;
    }
    try {
      const row = await api.updateSource(id, { platform_id: platformId });
      patchSource(id, row);
    } catch (err) {
      if (prev) {
        patchSource(id, { platform_id: prev.platform_id, state: prev.state });
      }
      showToast(errorMessage(err));
    }
  }

  async function setSeedPolicy(id: number, policy: SeedPolicy): Promise<void> {
    const prev = sources.find((s) => s.id === id);
    patchSource(id, { seed_policy: policy });
    if (isMock) {
      return;
    }
    try {
      const row = await api.updateSource(id, { seed_policy: policy });
      patchSource(id, row);
    } catch (err) {
      if (prev) {
        patchSource(id, { seed_policy: prev.seed_policy });
      }
      showToast(errorMessage(err));
    }
  }

  async function disable(id: number): Promise<void> {
    const prev = sources.find((s) => s.id === id);
    patchSource(id, { state: 'disabled' });
    if (isMock) {
      return;
    }
    try {
      const row = await api.updateSource(id, { state: 'disabled' });
      patchSource(id, row);
    } catch (err) {
      if (prev) {
        patchSource(id, { state: prev.state });
      }
      showToast(errorMessage(err));
    }
  }

  async function remove(id: number): Promise<void> {
    if (isMock) {
      return;
    }
    try {
      await api.deleteSource(id);
      await loadSources();
    } catch (err) {
      showToast(errorMessage(err));
    }
  }

  function platformName(id: string): string {
    return platforms.find((p) => p.id === id)?.name ?? id;
  }

  async function upload(): Promise<void> {
    const files = Array.from(fileInput?.files ?? []);
    if (isMock) {
      return;
    }
    for (const file of files) {
      try {
        const up = await api.uploadSource(file);
        addUpload({ kind: 'sources', file: up.file, jobId: up.job_id });
      } catch (err) {
        showToast(`${file.name}: ${errorMessage(err)}`);
      }
    }
    scheduleIncoming('sources');
    if (fileInput) {
      fileInput.value = '';
    }
  }

  async function addMagnet(): Promise<void> {
    const uri = magnet.trim();
    if (!uri || isMock) {
      return;
    }
    try {
      const up = await api.addMagnet(uri);
      addUpload({ kind: 'sources', file: up.file, jobId: up.job_id });
      scheduleIncoming('sources');
      magnet = '';
    } catch (err) {
      showToast(errorMessage(err));
    }
  }
</script>

<div class="page">
  <h1>Sources</h1>

  <form class="card upload" onsubmit={(e) => e.preventDefault()}>
    <label>
      Add a .torrent file
      <input bind:this={fileInput} type="file" accept=".torrent" multiple onchange={upload} />
    </label>
    <label>
      Or a magnet link
      <input type="text" placeholder="magnet:?xt=..." bind:value={magnet} />
    </label>
    <button class="primary" onclick={addMagnet}>Add</button>
  </form>

  <h2>Waiting in <code>sources/</code></h2>
  <IncomingList which="sources" />

  {#if sources.length === 0}
    <p>No sources yet. Place a .torrent or .magnet file in <code>/media/fat/mistarr/sources</code> or drop one here.</p>
  {:else}
    <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th>Platform</th>
          <th>State</th>
          <th>Files</th>
          <th>Matched</th>
          <th>Seed policy</th>
          <th>Client</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each sources as source (source.id)}
          <tr>
            <td>{source.display_name}</td>
            <td>
              {#if source.platform_id}
                {source.platform_id}
              {:else}
                <select
                  disabled={source.file_count === 0}
                  onchange={(e) => bind(source.id, (e.currentTarget as HTMLSelectElement).value)}
                >
                  <option value="">Choose platform</option>
                  {#each platforms as p (p.id)}
                    <option value={p.id}>{p.name}</option>
                  {/each}
                </select>
                {#if source.suggested_platform_id}
                  <button class="suggest" onclick={() => source.suggested_platform_id && bind(source.id, source.suggested_platform_id)}>
                    Bind to {platformName(source.suggested_platform_id)}
                  </button>
                {/if}
              {/if}
            </td>
            <td>{source.state}{source.reason ? ` — ${source.reason}` : ''}</td>
            <td>{source.file_count}</td>
            <td>{source.matched_count}</td>
            <td>
              <select
                value={seedSelectValue(source.seed_policy)}
                onchange={(e) => setSeedPolicy(source.id, (e.currentTarget as HTMLSelectElement).value as SeedPolicy)}
              >
                <option value="none">None</option>
                <option value="client">Client default</option>
                <option value="ratio:1">Until ratio 1</option>
                <option value="ratio:2">Until ratio 2</option>
              </select>
            </td>
            <td>{source.client_id ? 'in client' : '—'}</td>
            <td class="row-actions">
              {#if source.state !== 'disabled'}
                <button onclick={() => disable(source.id)}>Disable</button>
              {/if}
              <button onclick={() => remove(source.id)}>Delete</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    </div>
  {/if}
</div>

<style>
  .upload {
    display: flex;
    flex-wrap: wrap;
    gap: 0.8em;
    align-items: end;
    margin-bottom: 1em;
  }

  .upload label {
    display: flex;
    flex-direction: column;
    gap: 0.2em;
    font-size: 0.85em;
  }

  .table-wrap {
    overflow-x: auto;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85em;
  }

  th,
  td {
    text-align: left;
    padding: 0.4em;
    border-bottom: 1px solid var(--border);
  }

  .suggest {
    display: block;
    margin-top: 0.3em;
    font-size: 0.9em;
  }

  .row-actions {
    display: flex;
    gap: 0.4em;
  }
</style>
