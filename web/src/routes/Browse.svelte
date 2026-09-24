<script lang="ts">
  import { onMount } from 'svelte';
  import { findPlatform, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { getGroups, getGroupsTotal, loadTitlesPage, patchGroup } from '../lib/stores/titles.svelte';
  import { titleUrl } from '../lib/router.svelte';
  import { api } from '../lib/api';
  import type { HaveFilter, TitleFilters } from '../lib/types';

  interface Props {
    platformId: string;
  }

  const { platformId }: Props = $props();

  const isMock = import.meta.env.VITE_MOCK === '1';

  let q = $state('');
  let have = $state<HaveFilter>('any');
  let region = $state('');
  let showHidden = $state(false);
  let page = $state(0);
  let loadingMore = $state(false);

  const platform = $derived(findPlatform(platformId));
  const groups = $derived(getGroups());
  const total = $derived(getGroupsTotal());

  function filters(): TitleFilters {
    return { q: q || undefined, have, region: region || undefined, sort: 'name' };
  }

  async function reload(): Promise<void> {
    page = 0;
    await loadTitlesPage(platformId, filters(), 0);
  }

  onMount(() => {
    void loadPlatforms();
    void reload();
  });

  $effect(() => {
    void platformId;
    void reload();
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

  function onScroll(e: Event): void {
    const el = e.currentTarget as HTMLElement;
    if (el.scrollTop + el.clientHeight > el.scrollHeight - 400) {
      void loadMore();
    }
  }

  async function toggleWant(parentId: number, wanted: number): Promise<void> {
    const next = wanted > 0 ? 0 : 1;
    patchGroup(parentId, { wanted: next });
    if (!isMock) {
      if (next > 0) {
        await api.want(parentId);
      } else {
        await api.unwant(parentId);
      }
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

<div class="page" onscroll={onScroll}>
  <h1>{platform?.name ?? platformId}</h1>

  <form class="filters" onsubmit={(e) => e.preventDefault()}>
    <input type="search" placeholder="Search" bind:value={q} oninput={reload} />
    <select bind:value={have} onchange={reload}>
      <option value="any">Any</option>
      <option value="yes">Have</option>
      <option value="no">Missing</option>
    </select>
    <input type="text" placeholder="Region" bind:value={region} oninput={reload} />
    <label>
      <input type="checkbox" bind:checked={showHidden} />
      Show hidden flags
    </label>
  </form>

  <div class="grid">
    {#each groups as group (group.parent_id)}
      <a class="poster" href={titleUrl(group.parent_id)}>
        <img src={group.art.boxart} alt="" loading="lazy" onerror={onArtError} />
        <p class="name">{group.pick_name}</p>
        <p class="muted">{group.have_verified > 0 ? 'Have' : 'Missing'}</p>
        <button
          class:primary={group.wanted > 0}
          onclick={(e) => {
            e.preventDefault();
            void toggleWant(group.parent_id, group.wanted);
          }}
        >
          {group.wanted > 0 ? 'Wanted' : 'Want'}
        </button>
      </a>
    {/each}
  </div>
  {#if groups.length === 0}
    <p class="muted">No titles match.</p>
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

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 1em;
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
