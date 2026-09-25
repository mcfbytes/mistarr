<script lang="ts">
  import { onMount } from 'svelte';
  import { getDownloads, getImports, loadDownloads, loadImports, patchDownload } from '../lib/stores/downloads.svelte';
  import { getJobs, getRecentJobs, jobOutcome, loadJobs, watchRecent } from '../lib/stores/jobs.svelte';
  import { findPlatform, loadPlatforms } from '../lib/stores/platforms.svelte';
  import { api, errorMessage } from '../lib/api';
  import { showToast } from '../lib/stores/toast.svelte';
  import { describeProgress, downloadStatus, fetchSubject, jobDetail, jobHref, jobStatus, kindLabel } from '../lib/status';
  import FetchCancel from '../lib/FetchCancel.svelte';
  import StatusPill from '../lib/StatusPill.svelte';
  import ProgressBar from '../lib/ProgressBar.svelte';
  import type { Job } from '../lib/types';

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

  function jobTitle(job: Job): string {
    const pid = job.payload.platform_id;
    const detail =
      job.kind === 'url_fetch'
        ? fetchSubject(job, jobs)
        : typeof pid === 'string'
          ? platformName(pid)
          : jobDetail(job.payload);
    return detail ? `${kindLabel(job.kind)}: ${detail}` : kindLabel(job.kind);
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
        <StatusPill {...downloadStatus(d.state)} />
        {#if d.error}<span class="error">{d.error}</span>{/if}
      </div>
      <ProgressBar view={{ fraction: d.progress, text: '' }} label={`${d.title_name} transfer`} />
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
    {@const view = job.state === 'running' ? describeProgress(job.kind, job.progress) : null}
    <div class="card row job" data-job={job.id}>
      <div class="head">
        <StatusPill {...jobStatus(job)} />
        <a href={jobHref(job)}><strong>{jobTitle(job)}</strong></a>
        <span class="muted">{job.lane} lane</span>
        <FetchCancel {job} label={jobTitle(job)} />
      </div>
      {#if job.state === 'running'}
        <ProgressBar view={view ?? { fraction: null, text: 'Starting' }} label={`${jobTitle(job)} progress`} />
      {/if}
      {#if job.reason}<p class="muted why">{job.reason}</p>{/if}
    </div>
  {:else}
    <p class="muted">No jobs queued or running.</p>
  {/each}

  <h2>Recent</h2>
  <ul class="imports" aria-live="polite">
    {#each recent as job (job.id)}
      <li>
        <StatusPill {...jobStatus(job)} />
        <span class:error={job.state === 'failed'}>{jobOutcome(job, platformName)}</span>
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
    align-items: baseline;
    gap: 0.6em;
    margin-bottom: 0.4em;
    overflow-wrap: anywhere;
  }

  .why {
    margin: 0.4em 0 0;
    font-size: 0.85em;
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
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.3em 0.6em;
    padding: 0.35em 0;
    border-bottom: 1px solid var(--border);
    overflow-wrap: anywhere;
  }
</style>
