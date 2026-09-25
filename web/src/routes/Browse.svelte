<script lang="ts">
  import { onMount } from 'svelte';
  import { findPlatform, loadPlatforms } from '../lib/stores/platforms.svelte';
  import {
    getGroups,
    getGroupsError,
    getGroupsTotal,
    isGroupsLoading,
    loadTitlesPage,
    patchGroup
  } from '../lib/stores/titles.svelte';
  import { titleUrl } from '../lib/router.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { fixtureSettings, scenarioBrowseFlags } from '../lib/fixtures';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import { launchBlocker } from '../lib/launch';
  import PlatformArt from '../lib/PlatformArt.svelte';
  import PosterPlaceholder from '../lib/PosterPlaceholder.svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { BROWSE_FLAGS, type HaveFilter, type TitleFilters } from '../lib/types';

  interface Props {
    platformId: string;
  }

  const { platformId }: Props = $props();

  const isMock = import.meta.env.VITE_MOCK === '1';
  const flagChoices: readonly string[] = isMock ? scenarioBrowseFlags() : BROWSE_FLAGS;
  /** Typing pauses this long before the search runs. */
  const SEARCH_DEBOUNCE_MS = 250;

  let q = $state('');
  let search = $state('');
  let have = $state<HaveFilter>('any');
  let wanted = $state<HaveFilter>('any');
  let region = $state('');
  let showHidden = $state(false);
  let requireFlags = $state<string[]>([]);
  let hideList = $state<string[]>([]);
  let page = $state(0);
  let loadingMore = $state(false);

  const platform = $derived(findPlatform(platformId));
  const canStartCore = $derived(platform !== undefined && platform.kind !== 'arcade');
  let statusFailed = $state(false);
  const coreBlocker = $derived(
    launchBlocker(getStatus()?.launch, statusFailed) ??
      (platform && !platform.core_present ? 'No core for this platform is installed.' : null)
  );
  let coreBusy = $state(false);

  async function startCore(): Promise<void> {
    coreBusy = true;
    try {
      if (!isMock) {
        await api.launchCore(platformId);
      }
      showToast('Core started on the MiSTer.', 'success');
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      coreBusy = false;
    }
  }
  const groups = $derived(getGroups());
  const total = $derived(getGroupsTotal());
  const loading = $derived(isGroupsLoading());
  const loadError = $derived(getGroupsError());
  // Requiring a flag the server hides by default would otherwise always
  // yield an empty grid, so force "show hidden" on for that combination.
  const forcedByFlags = $derived(requireFlags.filter((f) => hideList.includes(f)));
  const effectiveHidden = $derived(showHidden || forcedByFlags.length > 0);

  function filters(): TitleFilters {
    return {
      q: search.trim() || undefined,
      have,
      wanted,
      region: region || undefined,
      flags: requireFlags.length > 0 ? requireFlags.join(',') : undefined,
      hidden: effectiveHidden ? 'show' : 'hide',
      sort: 'name'
    };
  }

  function toggleFlag(flag: string): void {
    requireFlags = requireFlags.includes(flag)
      ? requireFlags.filter((f) => f !== flag)
      : [...requireFlags, flag];
  }

  onMount(() => {
    void loadPlatforms();
    void loadHideList();
    if (!getStatus()) {
      void loadStatus().catch(() => {
        statusFailed = true;
      });
    }
  });

  async function loadHideList(): Promise<void> {
    const settings = isMock ? fixtureSettings : await api.settings();
    hideList = settings.prefs.hide;
  }

  $effect(() => {
    const next = q;
    const timer = setTimeout(() => {
      search = next;
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  });

  $effect(() => {
    page = 0;
    void loadTitlesPage(platformId, filters(), 0);
  });

  async function loadMore(): Promise<void> {
    if (loadingMore || loadError || groups.length >= total) {
      return;
    }
    loadingMore = true;
    // The page advances only once it has landed, so a failure never skips rows.
    if (await loadTitlesPage(platformId, filters(), page + 1)) {
      page += 1;
    }
    loadingMore = false;
  }

  function retry(): void {
    page = 0;
    void loadTitlesPage(platformId, filters(), 0);
  }

  function sentinel(node: HTMLElement): { destroy(): void } {
    const observer = new IntersectionObserver((entries) => {
      if (entries[0]?.isIntersecting) {
        void loadMore();
      }
    });
    observer.observe(node);
    return {
      destroy() {
        observer.disconnect();
      }
    };
  }

  async function toggleWant(parentId: number, pickId: number | null, currentlyWanted: number): Promise<void> {
    if (pickId === null) {
      return;
    }
    const next = currentlyWanted > 0 ? 0 : 1;
    patchGroup(parentId, { wanted: next });
    if (isMock) {
      return;
    }
    try {
      if (next > 0) {
        await api.want(pickId);
      } else {
        await api.unwant(pickId);
      }
    } catch (err) {
      patchGroup(parentId, { wanted: currentlyWanted });
      showToast(errorMessage(err));
    }
  }

  /** Groups whose cover failed to load, drawn with the generated poster instead. */
  const missingArt = new SvelteSet<number>();
</script>

<div class="page">
  <div class="head">
    <div class="banner"><PlatformArt id={platformId} kind={platform?.kind} format="wide" /></div>
    <h1>{platform?.name ?? platformId}</h1>
    {#if canStartCore}
      <button disabled={coreBusy || coreBlocker !== null} onclick={startCore}>Start core</button>
    {/if}
  </div>
  {#if canStartCore && coreBlocker}
    <p class="muted reason">Start core is unavailable: {coreBlocker}</p>
  {/if}

  <form class="filters" onsubmit={(e) => e.preventDefault()}>
    <input type="search" placeholder="Search" bind:value={q} />
    <select bind:value={have}>
      <option value="any">Any</option>
      <option value="yes">Have</option>
      <option value="no">Missing</option>
    </select>
    <select bind:value={wanted}>
      <option value="any">Wanted: any</option>
      <option value="yes">Wanted</option>
      <option value="no">Not wanted</option>
    </select>
    <input type="text" placeholder="Region" bind:value={region} />
    <label class="show-hidden">
      <input
        type="checkbox"
        checked={effectiveHidden}
        disabled={forcedByFlags.length > 0}
        onchange={(e) => (showHidden = e.currentTarget.checked)}
      />
      Show hidden
    </label>
    <fieldset class="flags">
      <legend>Require flags</legend>
      {#each flagChoices as flag (flag)}
        <label>
          <input
            type="checkbox"
            checked={requireFlags.includes(flag)}
            onchange={() => toggleFlag(flag)}
          />
          {flag}
        </label>
      {/each}
    </fieldset>
  </form>
  {#if forcedByFlags.length > 0}
    <p class="muted note">
      Showing hidden entries because {forcedByFlags.join(', ')}
      {forcedByFlags.length > 1 ? 'are' : 'is'} hidden by default.
    </p>
  {/if}

  <div class="status">
    <span class="hidden-text" aria-live="polite">{loading ? 'Loading titles' : ''}</span>
    {#if loading}
      <div class="busy" role="progressbar" aria-label="Loading titles" aria-valuetext="Loading"></div>
    {/if}
  </div>
  {#if loadError}
    <p class="error" role="alert">
      Titles could not be loaded: {loadError}
      <button onclick={retry}>Retry</button>
    </p>
  {/if}

  <div class="grid" class:dimmed={loading} aria-busy={loading}>
    {#each groups as group (group.parent_id)}
      <a class="poster" href={titleUrl(group.parent_id)}>
        {#if group.art?.boxart && !missingArt.has(group.parent_id)}
          <img src={group.art.boxart} alt="" loading="lazy" onerror={() => missingArt.add(group.parent_id)} />
        {:else}
          <PosterPlaceholder
            platformId={group.platform_id}
            kind={platform?.kind}
            title={group.base_name}
            name={group.pick_name ?? group.name}
          />
        {/if}
        <p class="name">{group.pick_name ?? group.name}</p>
        <p class="muted status">{group.have_verified > 0 ? 'Have' : 'Missing'}</p>
        {#if !group.bios}
          <button
            class:primary={group.wanted > 0}
            disabled={group.pick_id === null}
            onclick={(e) => {
              e.preventDefault();
              void toggleWant(group.parent_id, group.pick_id, group.wanted);
            }}
          >
            {group.wanted > 0 ? 'Wanted' : 'Want'}
          </button>
        {/if}
      </a>
    {/each}
  </div>
  {#if groups.length === 0 && !loading && !loadError}
    <p class="muted">No titles match.</p>
  {:else if groups.length < total}
    <div use:sentinel class="sentinel"></div>
  {/if}
</div>

<style>
  .head {
    position: relative;
    overflow: hidden;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5em 1em;
    padding: 116px 1em 1em;
    margin-bottom: 0.75em;
    border-radius: var(--radius);
    border: 1px solid var(--border);
  }

  .banner {
    position: absolute;
    inset: 0 0 auto;
    height: 150px;
  }

  /* Text starts where the scrim is at least 85% opaque, which holds AA contrast over any art. */
  .banner::after {
    content: '';
    position: absolute;
    inset: 0;
    background: linear-gradient(
      to bottom,
      transparent 50%,
      color-mix(in srgb, var(--bg) 85%, transparent) 76%,
      var(--bg) 94%
    );
  }

  .head > :not(.banner) {
    position: relative;
  }

  h1 {
    margin: 0;
    font-size: 1.6em;
    line-height: 1.2;
  }

  .reason {
    font-size: 0.9em;
    margin: 0;
  }

  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5em;
    margin: 1em 0;
    align-items: center;
  }

  .show-hidden {
    display: flex;
    align-items: center;
    gap: 0.3em;
  }

  .flags {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5em;
    align-items: center;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.3em 0.6em;
  }

  .flags label {
    display: flex;
    align-items: center;
    gap: 0.2em;
    font-size: 0.85em;
  }

  .note {
    font-size: 0.85em;
    margin: -0.5em 0 1em;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 1em;
    transition: opacity 0.15s;
  }

  .grid.dimmed {
    opacity: 0.5;
  }

  .status {
    min-height: 3px;
    margin: -0.5em 0 0.5em;
  }

  .busy {
    height: 3px;
    border-radius: 2px;
    background: linear-gradient(90deg, transparent, var(--accent), transparent);
    background-size: 40% 100%;
    background-repeat: no-repeat;
    animation: sweep 1s linear infinite;
  }

  @keyframes sweep {
    from {
      background-position: -40% 0;
    }
    to {
      background-position: 140% 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .busy {
      animation: none;
      background-size: 100% 100%;
    }
  }

  .hidden-text {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .error {
    margin: 0.5em 0;
    color: var(--danger);
  }

  .sentinel {
    height: 1px;
  }

  .poster {
    display: flex;
    flex-direction: column;
    text-decoration: none;
    color: var(--fg);
  }

  .poster .status {
    margin-top: auto;
    padding-top: 1em;
  }

  .poster button {
    align-self: flex-start;
  }

  .poster img {
    width: 100%;
    aspect-ratio: 10 / 14;
    object-fit: cover;
    border-radius: var(--radius);
    background: var(--bg-raised);
  }

  .name {
    font-size: 0.9em;
    margin: 0.3em 0 0;
  }
</style>
