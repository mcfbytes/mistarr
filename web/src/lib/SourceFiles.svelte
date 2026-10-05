<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { debounce, DELAY_MS } from './coalesce';
  import { api, errorMessage } from './api';
  import { confidenceLabel } from './availability';
  import { bytesText } from './format';
  import { titleUrl } from './router.svelte';
  import { downloadText, kindText, unmatchedText } from './sourceDetail';
  import type { SourceFile, SourceFileFilter } from './types';

  /**
   * A source's file table with its filter chips, search and pager. Every change of filter,
   * page or search loads the page again; `reload` loads it for a change made elsewhere.
   */
  const { sourceId }: { sourceId: number } = $props();

  const PAGE = 50;
  const FILTERS: { value: SourceFileFilter | undefined; label: string }[] = [
    { value: undefined, label: 'All' },
    { value: 'matched', label: 'Matched' },
    { value: 'unmatched', label: 'Unmatched' },
    { value: 'wanted', label: 'Wanted' }
  ];

  let filter = $state<SourceFileFilter | undefined>(undefined);
  let query = $state('');
  let applied = '';
  let offset = $state(0);
  let files = $state<SourceFile[]>([]);
  let total = $state(0);
  let filesLoading = $state(false);
  let filesError = $state<string | null>(null);
  let controller: AbortController | null = null;

  const firstShown = $derived(total === 0 ? 0 : offset + 1);
  const lastShown = $derived(Math.min(offset + PAGE, total));

  async function loadFiles(): Promise<void> {
    controller?.abort();
    const c = new AbortController();
    controller = c;
    filesLoading = true;
    try {
      const page = await api.sourceFiles(sourceId, { filter, q: applied || undefined, limit: PAGE, offset }, c.signal);
      if (c.signal.aborted) {
        return;
      }
      files = page.items;
      total = page.total;
      filesError = null;
    } catch (err) {
      if (!c.signal.aborted) {
        filesError = errorMessage(err);
      }
    } finally {
      if (controller === c) {
        filesLoading = false;
      }
    }
  }

  /** Reads the current page again. */
  export function reload(): void {
    void loadFiles();
  }

  onMount(reload);

  onDestroy(() => {
    controller?.abort();
    applySearch.cancel();
  });

  function setFilter(value: SourceFileFilter | undefined): void {
    filter = value;
    offset = 0;
    void loadFiles();
  }

  function setOffset(next: number): void {
    offset = next;
    void loadFiles();
  }

  const applySearch = debounce((value: string) => {
    applied = value.trim();
    offset = 0;
    void loadFiles();
  }, DELAY_MS.search);

  function onSearch(value: string): void {
    query = value;
    applySearch(value);
  }
</script>

<section class="card" aria-labelledby="files-h">
  <h2 id="files-h">Files</h2>
  <div class="tools">
    <div class="chips" role="group" aria-label="Show files">
      {#each FILTERS as f (f.label)}
        <button class="chip" aria-pressed={filter === f.value} onclick={() => setFilter(f.value)}>{f.label}</button>
      {/each}
    </div>
    <input
      type="search"
      placeholder="Search paths"
      aria-label="Search paths"
      value={query}
      oninput={(e) => onSearch(e.currentTarget.value)}
    />
  </div>
  {#if filesError}
    <p role="alert">{filesError} <button onclick={reload}>Retry</button></p>
  {/if}
  <p class="muted" aria-live="polite">
    {#if total === 0 && !filesLoading}
      No files to show.
    {:else}
      Files {firstShown.toLocaleString()}–{lastShown.toLocaleString()} of {total.toLocaleString()}
    {/if}
  </p>
  <table class:busy={filesLoading} aria-busy={filesLoading}>
    <thead>
      <tr>
        <th>Path</th>
        <th>Size</th>
        <th>Kind</th>
        <th>DAT entry</th>
        <th>Transfer</th>
      </tr>
    </thead>
    <tbody>
      {#each files as f (f.file_index)}
        <tr>
          <td class="path">{f.path}</td>
          <td class="num">{bytesText(f.size)}</td>
          <td>{kindText(f.kind)}</td>
          <td>
            {#if f.rom_name && f.title_id !== null}
              <a href={titleUrl(f.title_id)}>{f.rom_name}</a>
              {#if f.confidence}<span class="muted">({confidenceLabel(f.confidence)})</span>{/if}
            {:else if f.candidates[0]}
              {@const c = f.candidates[0]}
              Possibly <a href={titleUrl(c.title_id)}>{c.rom_name}</a>
              <span class="muted">({confidenceLabel(c.confidence)})</span>
            {:else if f.unmatched}
              <span class="muted">{unmatchedText(f.unmatched)}</span>
            {/if}
          </td>
          <td>{f.download ? downloadText(f.download) : ''}</td>
        </tr>
      {/each}
    </tbody>
  </table>
  <div class="pager">
    <button disabled={offset === 0} onclick={() => setOffset(Math.max(0, offset - PAGE))}>Previous</button>
    <button disabled={offset + PAGE >= total} onclick={() => setOffset(offset + PAGE)}>Next</button>
  </div>
</section>

<style>
  section {
    margin-bottom: 1em;
  }

  h2 {
    font-size: 1.05em;
    margin: 0 0 0.6em;
  }

  .path {
    overflow-wrap: break-word;
  }

  .chip {
    padding: 0.25em 0.7em;
    font-size: 0.85em;
  }

  .pager {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5em;
    margin-top: 0.6em;
  }

  .tools {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6em;
    align-items: center;
    justify-content: space-between;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
  }

  .chip[aria-pressed='true'] {
    background: var(--accent);
    border-color: var(--accent);
    color: #08101f;
  }

  input[type='search'] {
    flex: 1 1 12em;
    min-width: 0;
  }

  table {
    table-layout: fixed;
  }

  table.busy {
    opacity: 0.6;
  }

  th,
  td {
    overflow-wrap: break-word;
  }

  th:nth-child(1) {
    width: 34%;
  }

  th:nth-child(2) {
    width: 10%;
  }

  th:nth-child(3) {
    width: 12%;
  }

  th:nth-child(5) {
    width: 14%;
  }

  .num {
    white-space: nowrap;
  }

  @media (max-width: 600px) {
    thead {
      display: none;
    }

    table,
    tbody,
    tr,
    td {
      display: block;
    }

    tr {
      border-bottom: 1px solid var(--border);
      padding: 0.4em 0;
    }

    td {
      border: none;
      padding: 0.1em 0;
    }

    td:empty {
      display: none;
    }
  }
</style>
