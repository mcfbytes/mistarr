<script lang="ts">
  import { onMount } from 'svelte';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { platformUrl } from '../lib/router.svelte';
  import { api } from '../lib/api';

  onMount(() => {
    void loadPlatforms();
  });

  const platforms = $derived(getPlatforms());
  const present = $derived(platforms.filter((p) => p.core_present));
  const absent = $derived(platforms.filter((p) => !p.core_present));

  const isMock = import.meta.env.VITE_MOCK === '1';

  async function scan(id: string): Promise<void> {
    if (!isMock) {
      await api.scan(id);
    }
  }
</script>

<div class="page">
  <h1>Platforms</h1>
  <div class="grid">
    {#each present as platform (platform.id)}
      <div class="card">
        <h2><a href={platformUrl(platform.id)}>{platform.name}</a></h2>
        <p class="muted">
          {platform.counts.have} have · {platform.counts.wanted} wanted · {platform.counts.unverified} unverified
          of {platform.counts.titles}
        </p>
        <button onclick={() => scan(platform.id)}>Scan</button>
      </div>
    {/each}
  </div>

  {#if absent.length > 0}
    <details>
      <summary>Platforms with no core present ({absent.length})</summary>
      <div class="grid">
        {#each absent as platform (platform.id)}
          <div class="card">
            <h2>{platform.name}</h2>
            <p class="muted">Core not present.</p>
          </div>
        {/each}
      </div>
    </details>
  {/if}
</div>

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 1em;
    margin-top: 1em;
  }

  details {
    margin-top: 1.5em;
  }

  summary {
    cursor: pointer;
    color: var(--fg-dim);
  }
</style>
