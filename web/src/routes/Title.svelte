<script lang="ts">
  import { clearDetail, getDetail, loadTitleDetail, setDetail } from '../lib/stores/titles.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { fixtureTitle } from '../lib/fixtures';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import { findPlatform, loadPlatforms, platformsLoaded } from '../lib/stores/platforms.svelte';
  import { canPlay, launchBlocker } from '../lib/launch';
  import { availabilityLine } from '../lib/availability';
  import { chdMemberLabel } from '../lib/unidentified';

  interface Props {
    titleId: number;
  }

  const { titleId }: Props = $props();

  const isMock = import.meta.env.VITE_MOCK === '1';
  let tab = $state<'boxart' | 'title' | 'snap'>('boxart');
  let busy = $state(false);

  $effect(() => {
    clearDetail();
    void loadTitleDetail(titleId);
  });

  const detail = $derived(getDetail());
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
    if (!platformsLoaded()) {
      void loadPlatforms().catch(() => undefined);
    }
  });

  async function play(variantId: number): Promise<void> {
    busy = true;
    try {
      if (!isMock) {
        await api.launchTitle(variantId);
      }
      showToast('Started on the MiSTer.', 'success');
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      busy = false;
    }
  }

  async function want(variantId: number): Promise<void> {
    busy = true;
    try {
      if (isMock) {
        setDetail({
          ...fixtureTitle(titleId),
          variants: (detail?.variants ?? []).map((v) => (v.id === variantId ? { ...v, wanted: true } : v))
        });
      } else {
        setDetail(await api.want(titleId, variantId));
      }
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      busy = false;
    }
  }

  async function unwant(): Promise<void> {
    busy = true;
    try {
      if (isMock) {
        setDetail({
          ...fixtureTitle(titleId),
          variants: (detail?.variants ?? []).map((v) => ({ ...v, wanted: false }))
        });
      } else {
        setDetail(await api.unwant(titleId));
      }
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      busy = false;
    }
  }

  async function rename(fileId: number): Promise<void> {
    busy = true;
    try {
      if (!isMock) {
        setDetail(await api.rename(titleId, fileId));
      }
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      busy = false;
    }
  }

  function onArtError(e: Event): void {
    (e.currentTarget as HTMLImageElement).style.visibility = 'hidden';
  }
</script>

<div class="page">
  {#if detail}
    <h1>{detail.base_name}</h1>

    {#if detail.art}
      <div class="art">
        <div class="tabs">
          <button class:primary={tab === 'boxart'} onclick={() => (tab = 'boxart')}>Boxart</button>
          <button class:primary={tab === 'title'} onclick={() => (tab = 'title')}>Title</button>
          <button class:primary={tab === 'snap'} onclick={() => (tab = 'snap')}>Snap</button>
        </div>
        <img src={detail.art[tab]} alt="" onerror={onArtError} />
      </div>
    {/if}

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
                {:else}
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
