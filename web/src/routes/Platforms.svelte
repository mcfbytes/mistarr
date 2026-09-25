<script lang="ts">
  import { onMount } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { findPlatform, getPlatforms, loadPlatforms, patchPlatform } from '../lib/stores/platforms.svelte';
  import { getJobs, trackScan } from '../lib/stores/jobs.svelte';
  import { describeProgress, jobStatus } from '../lib/status';
  import StatusPill from '../lib/StatusPill.svelte';
  import ProgressBar from '../lib/ProgressBar.svelte';
  import { platformUrl } from '../lib/router.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import SetupHints from '../lib/SetupHints.svelte';
  import PlatformArt from '../lib/PlatformArt.svelte';
  import type { Job, PlatformCounts } from '../lib/types';

  onMount(() => {
    void loadPlatforms();
  });

  const platforms = $derived(getPlatforms());
  const present = $derived(platforms.filter((p) => p.core_present && p.enabled));
  const absent = $derived(platforms.filter((p) => !p.core_present));
  const disabled = $derived(platforms.filter((p) => p.core_present && !p.enabled));

  const isMock = import.meta.env.VITE_MOCK === '1';
  const scanning = new SvelteSet<string>();

  // The open scan of each platform, shown on its card.
  const scans: Record<string, Job | undefined> = $derived(
    Object.fromEntries(
      getJobs()
        .filter((j) => j.kind === 'scan' && typeof j.payload.platform_id === 'string')
        .map((j) => [j.payload.platform_id as string, j])
    )
  );

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

  function platformName(id: string): string {
    return findPlatform(id)?.name ?? id;
  }

  // The button keeps focus while the request is out, so a second press is ignored here.
  async function scan(id: string): Promise<void> {
    if (scanning.has(id)) {
      return;
    }
    scanning.add(id);
    try {
      const queued = isMock ? await new Promise<null>((r) => setTimeout(() => r(null), 400)) : await api.scan(id);
      showToast(`Scan of ${platformName(id)} queued`, 'info');
      const jobId = queued?.job_id ?? queued?.arcade_job_id;
      if (jobId != null) {
        trackScan(jobId, id);
      }
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      scanning.delete(id);
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
      {@const job = scans[platform.id]}
      <div class="card art-card">
        <div class="banner"><PlatformArt id={platform.id} kind={platform.kind} /></div>
        <h2><a href={platformUrl(platform.id)}>{platform.name}</a></h2>
        <p class="muted">{summarize(platform.counts)}</p>
        {#if job}
          <div class="scan-state">
            <StatusPill {...jobStatus(job)} label={`Scan ${jobStatus(job).label.toLowerCase()}`} />
            {#if job.state === 'running'}
              <ProgressBar
                view={describeProgress('scan', job.progress) ?? { fraction: null, text: '' }}
                label={`Scan of ${platform.name}`}
                compact
              />
            {:else if job.reason}
              <span class="muted why">{job.reason}</span>
            {/if}
          </div>
        {/if}
        <div class="actions">
          <button onclick={() => scan(platform.id)} aria-disabled={scanning.has(platform.id)} aria-busy={scanning.has(platform.id)}>
            {#if scanning.has(platform.id)}<span class="spinner" aria-hidden="true"></span>Queuing…{:else}Scan{/if}
          </button>
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
    padding-top: 116px;
  }

  .art-card > :not(.banner) {
    position: relative;
  }

  .banner {
    position: absolute;
    inset: 0 0 auto;
    height: 150px;
  }

  /* Text starts where the scrim is at least 85% opaque, which holds AA contrast over any art. */
  .banner::after {
    content: '';
    position: absolute;
    inset: 0;
    background: linear-gradient(
      to bottom,
      transparent 50%,
      color-mix(in srgb, var(--bg-raised) 85%, transparent) 76%,
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

  h2 a:hover {
    text-decoration: underline;
  }

  .actions {
    display: flex;
    gap: 0.5em;
  }

  .actions button {
    display: inline-flex;
    align-items: center;
    gap: 0.4em;
  }

  .scan-state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.3em;
    margin-bottom: 0.6em;
  }

  .scan-state :global(.wrap) {
    align-self: stretch;
  }

  .why {
    font-size: 0.8rem;
  }

  details {
    margin-top: 1.5em;
  }

  summary {
    cursor: pointer;
    color: var(--fg-dim);
  }
</style>
