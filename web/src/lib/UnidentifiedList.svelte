<script lang="ts">
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
  let started = false;

  function mockPage(offset: number): Paged<UnidentifiedFile> {
    const all = fixtureUnidentified[platformId] ?? [];
    return { items: all.slice(offset, offset + PAGE), total: all.length };
  }

  async function more(): Promise<void> {
    loading = true;
    error = null;
    try {
      const page = isMock ? mockPage(items.length) : await api.unidentified(platformId, items.length, PAGE);
      items = [...items, ...page.items];
      total = page.total;
    } catch (err) {
      error = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  function onToggle(e: Event): void {
    if ((e.currentTarget as HTMLDetailsElement).open && !started) {
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
