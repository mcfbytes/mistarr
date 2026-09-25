<script lang="ts">
  import { untrack } from 'svelte';
  import { api, errorMessage } from './api';
  import { fixtureUnidentified } from './fixtures';
  import { reasonText } from './unidentified';
  import type { Paged, UnidentifiedFile } from './types';

  interface Props {
    platformId: string;
    count: number;
  }

  const { platformId, count }: Props = $props();

  const PAGE = 50;
  const isMock = import.meta.env.VITE_MOCK === '1';
  let items = $state<UnidentifiedFile[]>([]);
  let total = $state(0);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let open = $state(false);
  let started = false;
  // Bumped when the list is reset, so a page fetched for the old list is dropped.
  let generation = 0;
  let shownFor = untrack(() => count);

  // A scan or decode changed the count: the pages read so far are stale.
  $effect(() => {
    const now = count;
    untrack(() => {
      if (now === shownFor) {
        return;
      }
      shownFor = now;
      generation += 1;
      items = [];
      total = 0;
      loading = false;
      started = open;
      if (open) {
        void more();
      }
    });
  });

  function mockPage(offset: number): Paged<UnidentifiedFile> {
    const all = fixtureUnidentified[platformId] ?? [];
    return { items: all.slice(offset, offset + PAGE), total: all.length };
  }

  async function more(): Promise<void> {
    const mine = generation;
    loading = true;
    error = null;
    try {
      const page = isMock ? mockPage(items.length) : await api.unidentified(platformId, items.length, PAGE);
      if (mine !== generation) {
        return;
      }
      items = [...items, ...page.items];
      total = page.total;
    } catch (err) {
      if (mine === generation) {
        error = errorMessage(err);
      }
    } finally {
      if (mine === generation) {
        loading = false;
      }
    }
  }

  function onToggle(e: Event): void {
    open = (e.currentTarget as HTMLDetailsElement).open;
    if (open && !started) {
      started = true;
      void more();
    }
  }
</script>

<details ontoggle={onToggle}>
  <summary>{count} not identified</summary>
  <p class="muted" aria-live="polite">{loading ? 'Loading…' : `${items.length} of ${total} shown`}</p>
  <ul>
    {#each items as file (file.rel_path)}
      <li>
        <span class="path">{file.rel_path}</span>
        <span class="muted">{reasonText(file.reason)}</span>
      </li>
    {/each}
  </ul>
  {#if items.length < total && !loading}
    <button onclick={more}>Show more</button>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
</details>

<style>
  details {
    margin: 0 0 0.6em;
  }

  summary {
    cursor: pointer;
    color: var(--fg-dim);
  }

  ul {
    list-style: none;
    padding: 0;
    margin: 0.3em 0;
  }

  li {
    padding: 0.3em 0;
    border-bottom: 1px solid var(--border);
  }

  .path {
    display: block;
    overflow-wrap: anywhere;
  }

  .error {
    color: var(--danger);
  }
</style>
