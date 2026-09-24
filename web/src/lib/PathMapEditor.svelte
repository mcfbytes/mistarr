<script lang="ts">
  import type { PathMapping } from './types';

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
