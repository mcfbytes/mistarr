<script lang="ts">
  import { onMount } from 'svelte';
  import { getDats, loadDats } from '../lib/stores/dats.svelte';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { uploadFiles } from '../lib/upload';
  import IncomingList from '../lib/IncomingList.svelte';
  import type { DatVersion } from '../lib/types';

  onMount(() => {
    void loadDats().catch(() => undefined);
    void loadPlatforms().catch(() => undefined);
  });

  let fileInput = $state<HTMLInputElement>();

  const platforms = $derived(getPlatforms());
  const dats = $derived(getDats());
  const current = $derived(
    dats
      .filter((d) => !d.retired && d.superseded_by === null)
      .sort((a, b) => platformName(a.platform_id).localeCompare(platformName(b.platform_id)))
  );
  const older = $derived(dats.filter((d) => d.retired || d.superseded_by !== null));

  function platformName(id: string | null): string {
    if (!id) {
      return 'Not bound';
    }
    return platforms.find((p) => p.id === id)?.name ?? id;
  }

  function loadedAt(d: DatVersion): string {
    return new Date(d.loaded_at * 1000).toLocaleString();
  }

  function stateOf(d: DatVersion): string {
    if (d.retired) {
      return 'Retired';
    }
    return d.superseded_by === null ? 'Current' : 'Replaced by a newer version';
  }
</script>

<div class="page">
  <h1>DATs</h1>

  <form class="card upload" onsubmit={(e) => e.preventDefault()}>
    <label>
      Add DAT files
      <input
        bind:this={fileInput}
        type="file"
        accept=".dat,.xml,.zip"
        multiple
        onchange={() => uploadFiles('dats', fileInput)}
      />
    </label>
    <p class="muted">
      Logiqx DATs, No-Intro database exports and zipped DAT packs are accepted. Files placed in
      <code>/media/fat/mistarr/dats</code> are picked up the same way.
    </p>
  </form>

  <h2>Waiting in <code>dats/</code></h2>
  <IncomingList which="dats" manage />

  <h2>Loaded</h2>
  {#if dats.length === 0}
    <p class="muted">No DAT loaded yet.</p>
  {:else}
    {#snippet versions(rows: DatVersion[], label: string)}
      <div class="table-wrap">
        <table aria-label={label}>
          <thead>
            <tr>
              <th>Name</th>
              <th>Platform</th>
              <th>Version</th>
              <th>Games</th>
              <th>Loaded</th>
              <th>State</th>
            </tr>
          </thead>
          <tbody>
            {#each rows as d (d.id)}
              <tr>
                <td class="dat-name">{d.dat_name}<span class="muted file">{d.source_file}</span></td>
                <td>{platformName(d.platform_id)}</td>
                <td>{d.version || '—'}</td>
                <td>{d.game_count}</td>
                <td>{loadedAt(d)}</td>
                <td>{stateOf(d)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/snippet}
    {@render versions(current, 'Loaded DATs')}
    {#if older.length > 0}
      <details>
        <summary>Older versions ({older.length})</summary>
        {@render versions(older, 'Older DAT versions')}
      </details>
    {/if}
  {/if}
</div>

<style>
  .upload {
    display: flex;
    flex-direction: column;
    gap: 0.5em;
    margin-bottom: 1em;
  }

  .upload label {
    display: flex;
    flex-direction: column;
    gap: 0.3em;
  }

  .upload p {
    margin: 0;
    font-size: 0.85em;
  }

  input[type='file'] {
    max-width: 100%;
  }

  code {
    overflow-wrap: anywhere;
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
    vertical-align: top;
  }

  .dat-name {
    overflow-wrap: anywhere;
    min-width: 10em;
  }

  .file {
    display: block;
    font-size: 0.9em;
  }

  details {
    margin-top: 1em;
  }
</style>
