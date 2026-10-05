<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { api, errorMessage } from '../lib/api';
  import { dats, markDatRemoved } from '../lib/stores/dats.svelte';
  import { platformName, platforms } from '../lib/stores/platforms.svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import IncomingList from '../lib/IncomingList.svelte';
  import UploadField from '../lib/UploadField.svelte';
  import UrlField from '../lib/UrlField.svelte';
  import type { DatVersion } from '../lib/types';

  onMount(() => {
    void dats.load();
    // Falls back to the id in boundName(); a miss retries at the next resync.
    void platforms.ensure();
    if (!getStatus()) {
      // Only feeds the dats-dir hint text; a miss just hides it until SSE resync.
      void loadStatus().catch(() => undefined);
    }
  });

  const busy = new SvelteSet<number>();
  let announcement = $state('');

  const datsDir = $derived(getStatus()?.dats_dir ?? null);

  interface Family {
    key: string;
    head: DatVersion;
    older: DatVersion[];
  }

  // One entry per family on a platform: the current version, else the newest, then the rest.
  const families = $derived.by((): Family[] => {
    const byKey: Record<string, DatVersion[]> = {};
    for (const d of dats.items) {
      const key = familyKey(d);
      byKey[key] = [...(byKey[key] ?? []), d];
    }
    const out: Family[] = [];
    for (const [key, rows] of Object.entries(byKey)) {
      const sorted = [...rows].sort((a, b) => b.loaded_at - a.loaded_at || b.id - a.id);
      const head = sorted.find(isCurrent) ?? sorted[0];
      if (head) {
        out.push({ key, head, older: sorted.filter((d) => d !== head) });
      }
    }
    return out.sort(
      (a, b) =>
        Number(isCurrent(b.head)) - Number(isCurrent(a.head)) ||
        boundName(a.head.platform_id).localeCompare(boundName(b.head.platform_id)) ||
        a.head.dat_name.localeCompare(b.head.dat_name)
    );
  });

  /** A family on a platform, stable across the versions that come and go in it. */
  function familyKey(d: DatVersion): string {
    return `${d.platform_id ?? ''}|${d.family || d.dat_name}`;
  }

  function isCurrent(d: DatVersion): boolean {
    return !d.retired && d.superseded_by === null;
  }

  function boundName(id: string | null): string {
    if (!id) {
      return 'Not bound';
    }
    return platformName(id);
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

  async function remove(d: DatVersion): Promise<boolean> {
    busy.add(d.id);
    announcement = `Removing ${label(d)}`;
    try {
      await api.deleteDat(d.id);
      markDatRemoved(d.id);
      announcement = `${label(d)} removed. Files on the card stay where they are.`;
      await tick();
      const key = familyKey(d);
      const headings = document.querySelectorAll<HTMLElement>('h3[data-family]');
      [...headings].find((h) => h.dataset.family === key)?.focus();
      return true;
    } catch (err) {
      announcement = `${label(d)}: ${errorMessage(err)}`;
      return false;
    } finally {
      busy.delete(d.id);
    }
  }
</script>

<div class="page">
  <h1>DATs</h1>

  <div class="card upload">
    <UploadField which="dats" label="Add DAT files" accept=".dat,.xml,.zip" />
    <UrlField />
    <p class="muted">
      Logiqx DATs, No-Intro database exports and zipped DAT packs are accepted.
      {#if datsDir}Files placed in <code>{datsDir}</code> are picked up the same way.{/if}
    </p>
  </div>

  <h2>Waiting in <code>dats/</code></h2>
  <IncomingList which="dats" manage />

  <h2>Loaded <span class="muted count">({dats.total} {dats.total === 1 ? 'version' : 'versions'})</span></h2>
  <p class="live" aria-live="polite">{announcement}</p>
  {#if dats.error}
    <p role="alert">{dats.error} <button type="button" onclick={() => void dats.load()}>Retry</button></p>
  {:else if dats.items.length === 0}
    <p class="muted">No DAT loaded yet.</p>
  {/if}
  {#if families.length > 0}
    <ul class="families" aria-label="Loaded DATs">
      {#each families as f (f.key)}
        {@const d = f.head}
        <li class="card family" class:inactive={!isCurrent(d)}>
          <h3 data-family={f.key} tabindex="-1">{d.dat_name}</h3>
          <dl>
            <div>
              <dt>Platform</dt>
              <dd>
                {boundName(d.platform_id)}{#if !d.platform_id && d.suggested.length > 0}; its
                  family is loaded for {d.suggested.map(platformName).join(', ')}{/if}
              </dd>
            </div>
            <div><dt>Version</dt><dd>{d.version || '—'}</dd></div>
            <div><dt>Games</dt><dd>{d.game_count}</dd></div>
            <div><dt>Loaded</dt><dd>{loadedAt(d)}</dd></div>
            <div><dt>State</dt><dd>{stateOf(d)}</dd></div>
          </dl>
          <p class="muted file">{d.source_file}</p>
          {#if isCurrent(d)}
            {#key d.id}
              <ConfirmButton
                name={`Remove ${label(d)}`}
                confirmLabel="Remove from the catalogue"
                confirmName={`Remove ${label(d)} from the catalogue`}
                keepName={`Keep ${label(d)}`}
                groupName={`Remove ${label(d)}?`}
                prompt="Its games leave the catalogue. Files on the card stay where they are, and a file another loaded DAT lists stays matched."
                busy={busy.has(d.id)}
                onconfirm={() => remove(d)}
              />
            {/key}
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

  .upload p {
    margin: 0;
    font-size: 0.85em;
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
