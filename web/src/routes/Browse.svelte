<script lang="ts">
  import { onMount } from 'svelte';
  import { findPlatform, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { getGroups, getGroupsTotal, loadTitlesPage, patchGroup } from '../lib/stores/titles.svelte';
  import { titleUrl } from '../lib/router.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { BROWSE_FLAGS, type HaveFilter, type TitleFilters } from '../lib/types';

  interface Props {
    platformId: string;
  }

  const { platformId }: Props = $props();

  const isMock = import.meta.env.VITE_MOCK === '1';

  let q = $state('');
  let have = $state<HaveFilter>('any');
  let wanted = $state<HaveFilter>('any');
  let region = $state('');
  let showHidden = $state(false);
  let requireFlags = $state<string[]>([]);
  let page = $state(0);
  let loadingMore = $state(false);

  const platform = $derived(findPlatform(platformId));
  const groups = $derived(getGroups());
  const total = $derived(getGroupsTotal());

  function filters(): TitleFilters {
    return {
      q: q || undefined,
      have,
      wanted,
      region: region || undefined,
      flags: requireFlags.length > 0 ? requireFlags.join(',') : undefined,
      hidden: showHidden ? 'show' : 'hide',
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
  });

  $effect(() => {
    void platformId;
    void q;
    void have;
    void wanted;
    void region;
    void showHidden;
    void requireFlags;
    page = 0;
    void loadTitlesPage(platformId, filters(), 0);
  });

  async function loadMore(): Promise<void> {
    if (loadingMore || groups.length >= total) {
      return;
    }
    loadingMore = true;
    page += 1;
    await loadTitlesPage(platformId, filters(), page);
    loadingMore = false;
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

  function onArtError(e: Event): void {
    const img = e.currentTarget as HTMLImageElement;
    img.src = placeholderArt;
  }

  const placeholderArt =
    'data:image/svg+xml,' +
    encodeURIComponent(
      '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="280"><rect width="100%" height="100%" fill="#2c303a"/></svg>'
    );
</script>

<div class="page">
  <h1>{platform?.name ?? platformId}</h1>

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
      <input type="checkbox" bind:checked={showHidden} />
      Show hidden
    </label>
    <fieldset class="flags">
      <legend>Require flags</legend>
      {#each BROWSE_FLAGS as flag (flag)}
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

  <div class="grid">
    {#each groups as group (group.parent_id)}
      <a class="poster" href={titleUrl(group.parent_id)}>
        <img src={group.art?.boxart ?? placeholderArt} alt="" loading="lazy" onerror={onArtError} />
        <p class="name">{group.pick_name ?? group.name}</p>
        <p class="muted">{group.have_verified > 0 ? 'Have' : 'Missing'}</p>
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
      </a>
    {/each}
  </div>
  {#if groups.length === 0}
    <p class="muted">No titles match.</p>
  {:else if groups.length < total}
    <div use:sentinel class="sentinel"></div>
  {/if}
</div>

<style>
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

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 1em;
  }

  .sentinel {
    height: 1px;
  }

  .poster {
    display: block;
    text-decoration: none;
    color: var(--fg);
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
