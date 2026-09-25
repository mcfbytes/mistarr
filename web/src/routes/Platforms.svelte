<script lang="ts">
  import { onMount } from 'svelte';
  import { findPlatform, getPlatforms, loadPlatforms, patchPlatform } from '../lib/stores/platforms.svelte';
  import { getFinishedJob, jobOutcome } from '../lib/stores/jobs.svelte';
  import { platformUrl } from '../lib/router.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import SetupHints from '../lib/SetupHints.svelte';
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

  // Scans this page queued, by job id, until their outcome is shown.
  let pending = $state<Record<number, string>>({});

  function platformName(id: string): string {
    return findPlatform(id)?.name ?? id;
  }

  $effect(() => {
    for (const [key, platformId] of Object.entries(pending)) {
      const done = getFinishedJob(Number(key));
      if (done) {
        showToast(jobOutcome({ ...done, payload: { platform_id: platformId } }, platformName));
        pending = Object.fromEntries(Object.entries(pending).filter(([k]) => k !== key));
      }
    }
  });

  async function scan(id: string): Promise<void> {
    scanning = { ...scanning, [id]: true };
    try {
      if (!isMock) {
        const queued = await api.scan(id);
        const jobId = queued.job_id ?? queued.arcade_job_id;
        if (jobId != null) {
          pending = { ...pending, [jobId]: id };
        }
      }
      showToast(`Scan of ${platformName(id)} queued`);
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
