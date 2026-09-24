<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { api, errorMessage } from '../lib/api';
  import { getDats, getDatTotal, loadDats, markDatRemoved } from '../lib/stores/dats.svelte';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import { uploadFiles } from '../lib/upload';
  import IncomingList from '../lib/IncomingList.svelte';
  import type { DatVersion } from '../lib/types';

  const isMock = import.meta.env.VITE_MOCK === '1';

  onMount(() => {
    void loadDats().catch(() => undefined);
    void loadPlatforms().catch(() => undefined);
    if (!getStatus()) {
      void loadStatus().catch(() => undefined);
    }
  });

  let fileInput = $state<HTMLInputElement>();
  let confirming = $state<number | null>(null);
  let busy = $state<Set<number>>(new Set());
  let announcement = $state('');

  const platforms = $derived(getPlatforms());
  const dats = $derived(getDats());
  const total = $derived(getDatTotal());
  const datsDir = $derived(getStatus()?.dats_dir ?? null);

  interface Family {
    key: string;
    head: DatVersion;
    older: DatVersion[];
  }

  // One entry per family on a platform: the current version, else the newest, then the rest.
  const families = $derived.by((): Family[] => {
    const byKey = new Map<string, DatVersion[]>();
    for (const d of dats) {
      const key = `${d.platform_id ?? ''}|${d.family || d.dat_name}`;
      byKey.set(key, [...(byKey.get(key) ?? []), d]);
    }
    const out: Family[] = [];
    for (const [key, rows] of byKey) {
      const sorted = [...rows].sort((a, b) => b.loaded_at - a.loaded_at || b.id - a.id);
      const head = sorted.find(isCurrent) ?? sorted[0];
      if (head) {
        out.push({ key, head, older: sorted.filter((d) => d !== head) });
      }
    }
    return out.sort(
      (a, b) =>
        Number(isCurrent(b.head)) - Number(isCurrent(a.head)) ||
        platformName(a.head.platform_id).localeCompare(platformName(b.head.platform_id)) ||
        a.head.dat_name.localeCompare(b.head.dat_name)
    );
  });

  function isCurrent(d: DatVersion): boolean {
    return !d.retired && d.superseded_by === null;
  }

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
    if (isCurrent(d)) {
      return 'Current';
    }
    return d.reason ?? (d.retired ? 'Removed' : 'Replaced by a newer version');
  }

  function label(d: DatVersion): string {
    return d.version ? `${d.dat_name} version ${d.version}` : d.dat_name;
  }

  async function focusButton(id: number, which: 'remove' | 'confirm'): Promise<void> {
    await tick();
    document.querySelector<HTMLButtonElement>(`[data-dat="${id}"][data-action="${which}"]`)?.focus();
  }

  function ask(d: DatVersion): void {
    confirming = d.id;
    void focusButton(d.id, 'confirm');
  }

  function keep(d: DatVersion): void {
    confirming = null;
    void focusButton(d.id, 'remove');
  }

  async function remove(d: DatVersion): Promise<void> {
    confirming = null;
    busy = new Set([...busy, d.id]);
    announcement = `Removing ${label(d)}`;
    try {
      if (!isMock) {
        await api.deleteDat(d.id);
      }
      markDatRemoved(d.id);
      announcement = `${label(d)} removed. Files on the card stay where they are.`;
      await tick();
      document.getElementById(`dat-${d.id}`)?.focus();
    } catch (err) {
      announcement = `${label(d)}: ${errorMessage(err)}`;
      void focusButton(d.id, 'remove');
    } finally {
      busy = new Set([...busy].filter((id) => id !== d.id));
    }
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
      Logiqx DATs, No-Intro database exports and zipped DAT packs are accepted.
      {#if datsDir}Files placed in <code>{datsDir}</code> are picked up the same way.{/if}
    </p>
  </form>

  <h2>Waiting in <code>dats/</code></h2>
  <IncomingList which="dats" manage />

  <h2>Loaded <span class="muted count">({total} {total === 1 ? 'version' : 'versions'})</span></h2>
  <p class="live" aria-live="polite">{announcement}</p>
  {#if dats.length === 0}
    <p class="muted">No DAT loaded yet.</p>
  {:else}
    <ul class="families" aria-label="Loaded DATs">
      {#each families as f (f.key)}
        {@const d = f.head}
        <li class="card family" class:inactive={!isCurrent(d)}>
          <h3 id={`dat-${d.id}`} tabindex="-1">{d.dat_name}</h3>
          <dl>
            <div><dt>Platform</dt><dd>{platformName(d.platform_id)}</dd></div>
            <div><dt>Version</dt><dd>{d.version || '—'}</dd></div>
            <div><dt>Games</dt><dd>{d.game_count}</dd></div>
            <div><dt>Loaded</dt><dd>{loadedAt(d)}</dd></div>
            <div><dt>State</dt><dd>{stateOf(d)}</dd></div>
          </dl>
          <p class="muted file">{d.source_file}</p>
          {#if isCurrent(d)}
            {#if confirming === d.id}
              <div class="confirm" role="group" aria-label={`Remove ${label(d)}?`}>
                <p>
                  Its games leave the catalogue. Files on the card stay where they are, and a file
                  another loaded DAT lists stays matched.
                </p>
                <button
                  type="button"
                  class="danger"
                  data-dat={d.id}
                  data-action="confirm"
                  aria-label={`Remove ${label(d)} from the catalogue`}
                  onclick={() => remove(d)}>Remove from the catalogue</button
                >
                <button type="button" aria-label={`Keep ${label(d)}`} onclick={() => keep(d)}>Keep</button>
              </div>
            {:else}
              <button
                type="button"
                data-dat={d.id}
                data-action="remove"
                aria-label={`Remove ${label(d)}`}
                disabled={busy.has(d.id)}
                onclick={() => ask(d)}>{busy.has(d.id) ? 'Removing…' : 'Remove'}</button
              >
            {/if}
          {/if}
          {#if f.older.length > 0}
            <details>
              <summary>Older versions ({f.older.length})</summary>
              <ul class="older" aria-label={`Older versions of ${d.dat_name}`}>
                {#each f.older as o (o.id)}
                  <li>
                    <span class="name">{o.dat_name}</span>
                    <span>{o.version || '—'}, {o.game_count} games, loaded {loadedAt(o)}</span>
                    <span class="muted">{stateOf(o)}</span>
                  </li>
                {/each}
              </ul>
            </details>
          {/if}
        </li>
      {/each}
    </ul>
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

  .count {
    font-size: 0.7em;
    font-weight: normal;
  }

  .live {
    margin: 0 0 0.5em;
    font-size: 0.85em;
  }

  .families,
  .older {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .family {
    margin-bottom: 0.8em;
    overflow-wrap: anywhere;
  }

  .family.inactive h3 {
    color: var(--fg-dim);
  }

  h3 {
    margin: 0 0 0.4em;
    font-size: 1em;
  }

  dl {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3em 1.2em;
    margin: 0;
    font-size: 0.85em;
  }

  dl div {
    display: flex;
    gap: 0.4em;
  }

  dt {
    color: var(--fg-dim);
  }

  dd {
    margin: 0;
  }

  .file {
    font-size: 0.8em;
    margin: 0.3em 0;
  }

  .confirm {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
    align-items: center;
  }

  .confirm p {
    flex-basis: 100%;
    margin: 0;
    font-size: 0.9em;
  }

  .danger {
    border-color: var(--danger);
    color: var(--danger);
  }

  details {
    margin-top: 0.5em;
    font-size: 0.85em;
  }

  .older li {
    display: flex;
    flex-wrap: wrap;
    gap: 0.2em 0.6em;
    padding: 0.3em 0;
    border-bottom: 1px solid var(--border);
  }

  .older .name {
    font-weight: 600;
  }
</style>
