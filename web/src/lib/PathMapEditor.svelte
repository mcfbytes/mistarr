<script lang="ts" module>
  import type { PathMapping } from './types';

  /** Drops rows left blank and returns the rest, or a message naming the first bad row. */
  export function cleanPathMap(map: PathMapping[]): { map: PathMapping[] } | { error: string } {
    const rows = map
      .map((m) => ({ remote: m.remote.trim(), local: m.local.trim() }))
      .filter((m) => m.remote !== '' || m.local !== '');
    const bad = rows.findIndex((m) => m.remote === '' || !m.local.startsWith('/'));
    if (bad >= 0) {
      return { error: `Mapping ${bad + 1} needs a remote path and an absolute local path.` };
    }
    return { map: rows };
  }
</script>

<script lang="ts">
  /** Edits a client's remote path map; each row can be removed. */
  let { map = $bindable() }: { map: PathMapping[] } = $props();
</script>

<div class="path-map">
  {#each map as mapping, i (i)}
    <div class="mapping">
      <label>
        Remote path
        <input type="text" placeholder="/downloads" bind:value={mapping.remote} />
      </label>
      <label>
        Local path
        <input type="text" placeholder="/media/fat/mistarr/staging" bind:value={mapping.local} />
      </label>
      <button type="button" onclick={() => (map = map.filter((_, j) => j !== i))}>Remove</button>
    </div>
  {:else}
    <p class="muted">No mappings. The client and mistarr see the same paths.</p>
  {/each}
  <button type="button" onclick={() => (map = [...map, { remote: '', local: '' }])}>Add mapping</button>
</div>

<style>
  .mapping {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em 0.6em;
    align-items: end;
    margin: 0.3em 0;
  }

  .mapping label {
    display: flex;
    flex-direction: column;
    gap: 0.2em;
    font-size: 0.85em;
    min-width: 0;
  }

  .mapping input {
    max-width: 100%;
  }
</style>
