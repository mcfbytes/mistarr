<script lang="ts">
  import { onMount } from 'svelte';
  import {
    clearDetail,
    getDetail,
    getDetailLoadError,
    isDetailMissing,
    loadTitleDetail,
    setDetail
  } from '../lib/stores/titles.svelte';
  import { attempt } from '../lib/actions';
  import { api } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import { findPlatform, platforms } from '../lib/stores/platforms.svelte';
  import { canPlay, launchBlocker } from '../lib/launch';
  import { availabilityLine } from '../lib/availability';
  import { chdMemberLabel } from '../lib/unidentified';
  import PosterPlaceholder from '../lib/PosterPlaceholder.svelte';
  import { SvelteSet } from 'svelte/reactivity';

  interface Props {
    titleId: number;
  }

  const { titleId }: Props = $props();

  let tab = $state<'boxart' | 'title' | 'snap'>('boxart');
  let busy = $state(false);
  /** Art tabs whose image failed to load; a missing boxart shows the generated poster. */
  const missingArt = new SvelteSet<string>();

  onMount(() => {
    clearDetail();
    void loadTitleDetail(titleId);
  });

  const detail = $derived(getDetail());
  const titleMissing = $derived(isDetailMissing());
  const titleLoadError = $derived(getDetailLoadError());
  const pickName = $derived(detail?.variants.find((v) => v.id === detail.pick_variant_id)?.name ?? null);
  const platform = $derived(detail ? findPlatform(detail.platform_id) : undefined);
  let statusFailed = $state(false);
  const playableIds = $derived(
    new Set(
      (detail?.variants ?? [])
        .filter((v) => canPlay(v, detail?.platform_id ?? '', platform?.kind))
        .map((v) => v.id)
    )
  );
  const playBlocker = $derived(
    launchBlocker(getStatus()?.launch, statusFailed) ??
      (platform && !platform.core_present ? 'No core for this platform is installed.' : null)
  );

  $effect(() => {
    if (!getStatus()) {
      void loadStatus().catch(() => {
        statusFailed = true;
      });
    }
    // A miss leaves platform kind/core-missing unknown until the next resync.
    void platforms.ensure();
  });

  // Disables the buttons while `call` is out; a failure is toasted.
  async function working(call: () => Promise<void>): Promise<void> {
    busy = true;
    await attempt(call);
    busy = false;
  }

  const play = (variantId: number): Promise<void> =>
    working(async () => {
      await api.launchTitle(variantId);
      showToast('Started on the MiSTer.', 'success');
    });
  const want = (variantId: number): Promise<void> => working(async () => setDetail(await api.want(titleId, variantId)));
  const unwant = (): Promise<void> => working(async () => setDetail(await api.unwant(titleId)));
  const rename = (fileId: number): Promise<void> => working(async () => setDetail(await api.rename(titleId, fileId)));

</script>

<div class="page">
  {#if detail}
    <h1>{detail.base_name}</h1>

    <div class="art">
      {#if detail.art}
        <div class="tabs">
          <button class:primary={tab === 'boxart'} onclick={() => (tab = 'boxart')}>Boxart</button>
          <button class:primary={tab === 'title'} onclick={() => (tab = 'title')}>Title</button>
          <button class:primary={tab === 'snap'} onclick={() => (tab = 'snap')}>Snap</button>
        </div>
      {/if}
      {#if detail.art && !missingArt.has(tab)}
        <img src={detail.art[tab]} alt="" onerror={() => missingArt.add(tab)} />
      {:else if tab === 'boxart'}
        <div class="poster">
          <PosterPlaceholder
            platformId={detail.platform_id}
            kind={platform?.kind}
            title={detail.base_name}
            name={pickName}
          />
        </div>
      {/if}
    </div>

    {#if playableIds.size > 0 && playBlocker}
      <p class="muted reason">Play is unavailable: {playBlocker}</p>
    {/if}

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
          <tr class:retired={variant.retired}>
            <td>{variant.name}{variant.is_1g1r_pick ? ' (pick)' : ''}</td>
            <td>{variant.regions.join(', ')}</td>
            <td>{variant.revision ?? '—'}</td>
            <td>{variant.flags.join(', ') || '—'}</td>
            <td>
              {#each variant.roms as rom (rom.id)}
                {@const member = rom.file_path ? chdMemberLabel(rom.file_path) : null}
                <div>{rom.file_state ?? 'missing'}{#if member}<span class="muted"> ({member})</span>{/if}</div>
              {/each}
            </td>
            <td class="sources">
              {#each variant.availability as found (`${found.source_id}:${found.file_index}:${found.rom_id}`)}
                <div>{availabilityLine(found)}</div>
              {:else}
                <div class="muted">None available</div>
              {/each}
            </td>
            <td>
              {#if playableIds.has(variant.id)}
                <button class="primary" disabled={busy || playBlocker !== null} onclick={() => play(variant.id)}>
                  Play
                </button>
              {/if}
              {#if !variant.retired}
                {#if variant.wanted}
                  <button disabled={busy} onclick={unwant}>Unwant</button>
                {:else if !variant.flags.includes('bios')}
                  <button class="primary" disabled={busy} onclick={() => want(variant.id)}>Want</button>
                {/if}
              {/if}
              {#each variant.roms as rom (rom.id)}
                {#if rom.file_state === 'misnamed' && rom.file_id}
                  <button disabled={busy} onclick={() => rename(rom.file_id as number)}>Rename</button>
                {/if}
              {/each}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    </div>
  {:else if titleMissing}
    <h1>Title not found</h1>
    <p class="muted">No title has this id. It may have been removed.</p>
  {:else if titleLoadError}
    <h1>Title</h1>
    <p role="alert">{titleLoadError} <button onclick={() => void loadTitleDetail(titleId)}>Retry</button></p>
  {:else}
    <p class="muted" aria-busy="true">Loading…</p>
  {/if}
</div>

<style>
  .poster {
    width: 180px;
    margin-top: 0.5em;
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
    margin-top: 1em;
    font-size: 0.9em;
  }

  td.sources div {
    overflow-wrap: anywhere;
  }

  tr.retired {
    opacity: 0.6;
  }

  .reason {
    font-size: 0.9em;
    margin: 0.8em 0 0;
  }
</style>
