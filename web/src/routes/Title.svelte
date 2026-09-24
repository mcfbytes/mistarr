<script lang="ts">
  import { onMount } from 'svelte';
  import { getDetail, loadTitleDetail } from '../lib/stores/titles.svelte';
  import { api } from '../lib/api';

  interface Props {
    titleId: number;
  }

  const { titleId }: Props = $props();

  const isMock = import.meta.env.VITE_MOCK === '1';
  let tab = $state<'boxart' | 'title' | 'snap'>('boxart');

  onMount(() => {
    void loadTitleDetail(titleId);
  });

  $effect(() => {
    void loadTitleDetail(titleId);
  });

  const detail = $derived(getDetail());

  async function want(variantId: number): Promise<void> {
    if (!isMock) {
      await api.want(titleId, variantId);
    }
  }

  async function unwant(): Promise<void> {
    if (!isMock) {
      await api.unwant(titleId);
    }
  }

  async function rename(fileId: number): Promise<void> {
    if (!isMock) {
      await api.rename(titleId, fileId);
    }
  }

  function onArtError(e: Event): void {
    (e.currentTarget as HTMLImageElement).style.visibility = 'hidden';
  }
</script>

<div class="page">
  {#if detail}
    <h1>{detail.base_name}</h1>

    <div class="art">
      <div class="tabs">
        <button class:primary={tab === 'boxart'} onclick={() => (tab = 'boxart')}>Boxart</button>
        <button class:primary={tab === 'title'} onclick={() => (tab = 'title')}>Title</button>
        <button class:primary={tab === 'snap'} onclick={() => (tab = 'snap')}>Snap</button>
      </div>
      <img src={detail.art[tab]} alt="" onerror={onArtError} />
    </div>

    <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th>Region</th>
          <th>Revision</th>
          <th>Flags</th>
          <th>File state</th>
          <th>Sources</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each detail.variants as variant (variant.id)}
          <tr>
            <td>{variant.name}{variant.is_1g1r_pick ? ' (pick)' : ''}</td>
            <td>{variant.regions.join(', ')}</td>
            <td>{variant.revision ?? '—'}</td>
            <td>{variant.flags.join(', ') || '—'}</td>
            <td>
              {#each variant.roms as rom (rom.id)}
                <div>{rom.file_state ?? 'missing'}</div>
              {/each}
            </td>
            <td>{variant.torrent_files_available} available</td>
            <td>
              {#if variant.wanted}
                <button onclick={unwant}>Unwant</button>
              {:else}
                <button class="primary" onclick={() => want(variant.id)}>Want</button>
              {/if}
              {#each variant.roms as rom (rom.id)}
                {#if rom.file_state === 'misnamed' && rom.file_id}
                  <button onclick={() => rename(rom.file_id as number)}>Rename</button>
                {/if}
              {/each}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    </div>
  {:else}
    <p class="muted">Loading…</p>
  {/if}
</div>

<style>
  .table-wrap {
    overflow-x: auto;
  }

  .art img {
    max-width: 240px;
    border-radius: var(--radius);
    display: block;
    margin-top: 0.5em;
  }

  .tabs {
    display: flex;
    gap: 0.4em;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    margin-top: 1em;
    font-size: 0.9em;
  }

  th,
  td {
    text-align: left;
    padding: 0.4em;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
</style>
