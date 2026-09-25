<script lang="ts">
  import { onMount } from 'svelte';
  import { getDownloads, getImports, loadDownloads, loadImports, patchDownload } from '../lib/stores/downloads.svelte';
  import { getJobs, getRecentJobs, jobOutcome, loadJobs, watchRecent } from '../lib/stores/jobs.svelte';
  import { findPlatform, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';

  const isMock = import.meta.env.VITE_MOCK === '1';

  onMount(() => {
    void loadDownloads();
    void loadImports();
    void loadJobs();
    void loadPlatforms().catch(() => undefined);
    return watchRecent();
  });

  const downloads = $derived(getDownloads());
  const imports = $derived(getImports());
  const jobs = $derived(getJobs());
  const recent = $derived(getRecentJobs());

  function platformName(id: string): string {
    return findPlatform(id)?.name ?? id;
  }

  async function retry(id: number): Promise<void> {
    if (isMock) {
      patchDownload(id, { state: 'queued', error: null });
      return;
    }
    try {
      const row = await api.retryDownload(id);
      patchDownload(id, row);
    } catch (err) {
      showToast(errorMessage(err));
    }
  }

  async function cancel(id: number): Promise<void> {
    if (isMock) {
      patchDownload(id, { state: 'cancelled' });
      return;
    }
    try {
      const row = await api.cancelDownload(id);
      patchDownload(id, row);
    } catch (err) {
      showToast(errorMessage(err));
    }
  }
</script>

<div class="page">
  <h1>Activity</h1>

  <h2>Downloads</h2>
  {#each downloads as d (d.id)}
    <div class="card row">
      <div class="head">
        <strong>{d.title_name}</strong>
        <span class="muted">{d.rom_name}</span>
        <span class="muted">{d.state}</span>
        {#if d.error}<span class="error">{d.error}</span>{/if}
      </div>
      <div class="progress"><span style={`width: ${Math.round(d.progress * 100)}%`}></span></div>
      <div class="actions">
        {#if d.state === 'failed'}
          <button onclick={() => retry(d.id)}>Retry</button>
        {/if}
        {#if ['wanted', 'queued', 'transferring', 'checking'].includes(d.state)}
          <button onclick={() => cancel(d.id)}>Cancel</button>
        {/if}
      </div>
    </div>
  {:else}
    <p class="muted">No downloads.</p>
  {/each}

  <h2>Jobs</h2>
  {#each jobs as job (job.id)}
    <div class="card row head">
      <strong>{job.kind}</strong>
      <span class="muted">{job.state}, {job.lane} lane</span>
      {#if job.reason}<span class="muted">{job.reason}</span>{/if}
    </div>
  {:else}
    <p class="muted">No jobs queued or running.</p>
  {/each}

  <h2>Recent</h2>
  <ul class="imports" aria-live="polite">
    {#each recent as job (job.id)}
      <li class:error={job.state === 'failed'}>
        {jobOutcome(job, platformName)}
        <span class="muted">{new Date(job.updated_at * 1000).toLocaleString()}</span>
      </li>
    {:else}
      <li class="muted">No finished jobs yet.</li>
    {/each}
  </ul>

  <h2>Imports</h2>
  <ul class="imports">
    {#each imports as entry (entry.id)}
      <li>{entry.action}</li>
    {:else}
      <li class="muted">No imports yet.</li>
    {/each}
  </ul>
</div>

<style>
  .row {
    margin-bottom: 0.6em;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6em;
    margin-bottom: 0.3em;
  }

  .actions {
    display: flex;
    gap: 0.5em;
    margin-top: 0.4em;
  }

  .error {
    color: var(--danger);
  }

  .imports {
    list-style: none;
    padding: 0;
  }

  .imports li {
    padding: 0.3em 0;
    border-bottom: 1px solid var(--border);
  }
</style>
