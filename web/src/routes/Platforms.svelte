<script lang="ts">
  import { onMount } from 'svelte';
  import { getPlatforms, loadPlatforms, patchPlatform } from '../lib/stores/platforms.svelte';
  import { platformUrl } from '../lib/router.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import SetupHints from '../lib/SetupHints.svelte';
  import PlatformArt from '../lib/PlatformArt.svelte';
  import type { PlatformCounts } from '../lib/types';

  onMount(() => {
    void loadPlatforms();
  });

  const platforms = $derived(getPlatforms());
  const present = $derived(platforms.filter((p) => p.core_present && p.enabled));
  const absent = $derived(platforms.filter((p) => !p.core_present));
  const disabled = $derived(platforms.filter((p) => p.core_present && !p.enabled));

  const isMock = import.meta.env.VITE_MOCK === '1';
  let scanning = $state<Record<string, boolean>>({});

  /** "N have · M wanted · T titles", plus any nonzero extra clause. */
  function summarize(counts: PlatformCounts): string {
    const parts = [`${counts.have} have`, `${counts.wanted} wanted`, `${counts.titles} titles`];
    if (counts.unmatched_files > 0) {
      parts.push(`${counts.unmatched_files} unmatched files`);
    }
    if (counts.failing_check > 0) {
      parts.push(`${counts.failing_check} failing check`);
    }
    if (counts.partial > 0) {
      parts.push(`${counts.partial} partial`);
    }
    return parts.join(' · ');
  }

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
      <div class="card art-card">
        <div class="banner"><PlatformArt id={platform.id} kind={platform.kind} /></div>
        <h2><a href={platformUrl(platform.id)}>{platform.name}</a></h2>
        <p class="muted">{summarize(platform.counts)}</p>
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
          <div class="card art-card">
            <div class="banner"><PlatformArt id={platform.id} kind={platform.kind} muted /></div>
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
          <div class="card art-card">
            <div class="banner"><PlatformArt id={platform.id} kind={platform.kind} muted /></div>
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

  .art-card {
    position: relative;
    overflow: hidden;
    padding-top: 100px;
  }

  .art-card > :not(.banner) {
    position: relative;
  }

  .banner {
    position: absolute;
    inset: 0 0 auto;
    height: 136px;
  }

  /* Text starts where the scrim is at least 85% opaque, which holds AA contrast over any art. */
  .banner::after {
    content: '';
    position: absolute;
    inset: 0;
    background: linear-gradient(
      to bottom,
      transparent 35%,
      color-mix(in srgb, var(--bg-raised) 85%, transparent) 72%,
      var(--bg-raised) 94%
    );
  }

  h2 {
    margin: 0 0 0.4em;
    font-size: 1.15em;
    line-height: 1.25;
  }

  h2 a {
    color: var(--fg);
    text-decoration: none;
  }

  h2 a:hover,
  h2 a:focus-visible {
    text-decoration: underline;
    text-decoration-color: var(--accent);
    text-underline-offset: 3px;
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
