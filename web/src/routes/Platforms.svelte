<script lang="ts">
  import { onMount } from 'svelte';
  import { getPlatforms, loadPlatforms, patchPlatform } from '../lib/stores/platforms.svelte';
  import { platformUrl } from '../lib/router.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import SetupHints from '../lib/SetupHints.svelte';

  onMount(() => {
    void loadPlatforms();
  });

  const platforms = $derived(getPlatforms());
  const present = $derived(platforms.filter((p) => p.core_present && p.enabled));
  const absent = $derived(platforms.filter((p) => !p.core_present));
  const disabled = $derived(platforms.filter((p) => p.core_present && !p.enabled));

  const isMock = import.meta.env.VITE_MOCK === '1';
  let scanning = $state<Record<string, boolean>>({});

  async function scan(id: string): Promise<void> {
    scanning = { ...scanning, [id]: true };
    try {
      if (!isMock) {
        await api.scan(id);
      }
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      scanning = { ...scanning, [id]: false };
    }
  }

  async function setEnabled(id: string, enabled: boolean): Promise<void> {
    patchPlatform(id, { enabled });
    if (isMock) {
      return;
    }
    try {
      await api.setPlatform(id, enabled);
    } catch (err) {
      patchPlatform(id, { enabled: !enabled });
      showToast(errorMessage(err));
    }
  }
</script>

<div class="page">
  <h1>Platforms</h1>
  <SetupHints />
  <div class="grid">
    {#each present as platform (platform.id)}
      <div class="card">
        <h2><a href={platformUrl(platform.id)}>{platform.name}</a></h2>
        <p class="muted">
          {platform.counts.have} have · {platform.counts.wanted} wanted · {platform.counts.unverified} unverified
          of {platform.counts.titles}
        </p>
        <div class="actions">
          <button onclick={() => scan(platform.id)} disabled={scanning[platform.id]}>Scan</button>
          <button onclick={() => setEnabled(platform.id, false)}>Disable</button>
        </div>
      </div>
    {/each}
  </div>

  {#if disabled.length > 0}
    <details>
      <summary>Disabled platforms ({disabled.length})</summary>
      <div class="grid">
        {#each disabled as platform (platform.id)}
          <div class="card">
            <h2>{platform.name}</h2>
            <button onclick={() => setEnabled(platform.id, true)}>Enable</button>
          </div>
        {/each}
      </div>
    </details>
  {/if}

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

  .actions {
    display: flex;
    gap: 0.5em;
  }

  details {
    margin-top: 1.5em;
  }

  summary {
    cursor: pointer;
    color: var(--fg-dim);
  }
</style>
