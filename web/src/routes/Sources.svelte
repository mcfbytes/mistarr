<script lang="ts">
  import { onMount } from 'svelte';
  import { getSources, loadSources, patchSource } from '../lib/stores/sources.svelte';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { api } from '../lib/api';

  const isMock = import.meta.env.VITE_MOCK === '1';

  onMount(() => {
    void loadSources();
    void loadPlatforms();
  });

  const sources = $derived(getSources());
  const platforms = $derived(getPlatforms());

  async function bind(id: number, platformId: string): Promise<void> {
    patchSource(id, { platform_id: platformId, state: 'bound' });
    if (!isMock) {
      await api.updateSource(id, { platform_id: platformId });
    }
  }

  async function disable(id: number): Promise<void> {
    patchSource(id, { state: 'disabled' });
    if (!isMock) {
      await api.updateSource(id, { state: 'disabled' });
    }
  }
</script>

<div class="page">
  <h1>Sources</h1>

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
                <select onchange={(e) => bind(source.id, (e.currentTarget as HTMLSelectElement).value)}>
                  <option value="">Choose platform</option>
                  {#each platforms as p (p.id)}
                    <option value={p.id}>{p.name}</option>
                  {/each}
                </select>
              {/if}
            </td>
            <td>{source.state}</td>
            <td>{source.file_count}</td>
            <td>{source.matched_count}</td>
            <td>{source.seed_policy}</td>
            <td>{source.client_status ?? '—'}</td>
            <td>
              {#if source.state !== 'disabled'}
                <button onclick={() => disable(source.id)}>Disable</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    </div>
  {/if}
</div>

<style>
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
</style>
