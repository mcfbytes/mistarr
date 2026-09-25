<script lang="ts">
  import { onMount } from 'svelte';
  import { getSources, loadSources, patchSource } from '../lib/stores/sources.svelte';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { sourceStatus } from '../lib/status';
  import IncomingList from '../lib/IncomingList.svelte';
  import UploadField from '../lib/UploadField.svelte';
  import MagnetField from '../lib/MagnetField.svelte';
  import StatusPill from '../lib/StatusPill.svelte';
  import { sourceUrl } from '../lib/router.svelte';
  import { bindingText } from '../lib/sourceDetail';
  import type { SeedPolicy } from '../lib/types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  onMount(() => {
    void loadSources();
    void loadPlatforms();
  });

  const sources = $derived(getSources());
  const platforms = $derived(getPlatforms());

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
    if (isMock) {
      patchSource(id, { platform_id: platformId, state: 'bound', user_binding: true });
      return;
    }
    const pending = { automatic: false, platform_id: platformId };
    patchSource(id, { user_binding: true, pending_binding: pending });
    try {
      // The binding runs as a job; `source.changed` brings the bound row once it ends.
      const row = await api.updateSource(id, { platform_id: platformId });
      patchSource(id, { user_binding: row.user_binding, pending_binding: row.pending_binding });
    } catch (err) {
      if (prev) {
        patchSource(id, { user_binding: prev.user_binding, pending_binding: prev.pending_binding });
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
</script>

<div class="page">
  <h1>Sources</h1>

  <div class="card upload">
    <UploadField which="sources" label="Add a .torrent file" accept=".torrent" />
    <MagnetField />
  </div>

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
            <td><a href={sourceUrl(source.id)} class="name">{source.display_name}</a></td>
            <td>
              {#if source.pending_binding}
                <span class="muted">{bindingText(source, platformName)}</span>
              {:else if source.platform_id}
                {source.platform_id}
                {#if source.user_binding}<span class="tag">Set by you</span>{/if}
              {:else}
                {#if source.user_binding}<span class="tag">Set by you</span>{/if}
                <select
                  disabled={source.file_count === 0}
                  onchange={(e) => bind(source.id, e.currentTarget.value)}
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
            <td>
              <StatusPill {...sourceStatus(source.state)} />
              {#if source.reason}<span class="muted reason">{source.reason}</span>{/if}
            </td>
            <td>{source.file_count}</td>
            <td>{source.matched_count}</td>
            <td>
              <select
                value={seedSelectValue(source.seed_policy)}
                onchange={(e) => setSeedPolicy(source.id, e.currentTarget.value as SeedPolicy)}
              >
                <option value="none">None</option>
                <option value="client">Client default</option>
                <option value="ratio:1">Until ratio 1</option>
                <option value="ratio:2">Until ratio 2</option>
              </select>
            </td>
            <td>{source.client_id ? 'in client' : '—'}</td>
            <td>
              <div class="row-actions">
                {#if source.state !== 'disabled'}
                  <button onclick={() => disable(source.id)}>Disable</button>
                {/if}
                <button onclick={() => remove(source.id)}>Delete</button>
              </div>
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
    display: grid;
    gap: 0.6em;
    margin-bottom: 1em;
  }

  .reason {
    display: block;
    margin-top: 0.2em;
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

  .name {
    overflow-wrap: break-word;
  }

  .tag {
    display: inline-block;
    font-size: 0.85em;
    border: 1px solid var(--accent);
    color: var(--accent);
    border-radius: 999px;
    padding: 0 0.5em;
    white-space: nowrap;
  }

  .row-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
  }
</style>
