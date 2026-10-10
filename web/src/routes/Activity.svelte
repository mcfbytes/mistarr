<script lang="ts">
  import { onMount } from 'svelte';
  import { downloads, imports, watchImports } from '../lib/stores/downloads.svelte';
  import { jobs, recent, watchRecent } from '../lib/stores/jobs.svelte';
  import { platformName, platforms } from '../lib/stores/platforms.svelte';
  import { attempt } from '../lib/actions';
  import { api } from '../lib/api';
  import {
    downloadStatus,
    jobOutcome,
    jobStatus,
    runOutcome
  } from '../lib/status';
  import JobRow from '../lib/JobRow.svelte';
  import StatusPill from '../lib/StatusPill.svelte';
  import ProgressBar from '../lib/ProgressBar.svelte';
  import type { Download, Job } from '../lib/types';

  onMount(() => {
    void downloads.load();
    void imports.load();
    void jobs.load();
    // The platform name falls back to its id in the job title; it retries at the next resync.
    void platforms.ensure();
    const stopRecent = watchRecent();
    const stopImports = watchImports();
    return () => {
      stopRecent();
      stopImports();
    };
  });

  async function change(id: number, call: () => Promise<Download>): Promise<void> {
    const row = await attempt(call);
    if (row) {
      downloads.patch(id, row);
    }
  }

  /** The span a folded run covers, in the list's muted time style; one time when it is a second. */
  function timeRange(job: Job): string {
    const first = new Date((job.first_updated_at ?? job.updated_at) * 1000).toLocaleString();
    const last = new Date(job.updated_at * 1000).toLocaleString();
    return first === last ? last : `${first} – ${last}`;
  }
</script>

<div class="page">
  <h1>Activity</h1>

  <h2>Downloads</h2>
  {#if downloads.error}
    <p role="alert">{downloads.error} <button type="button" onclick={() => void downloads.load()}>Retry</button></p>
  {/if}
  {#if downloads.loaded || !downloads.error}
    {#each downloads.items as d (d.id)}
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
            <button onclick={() => change(d.id, () => api.retryDownload(d.id))}>Retry</button>
          {/if}
          {#if ['wanted', 'queued', 'transferring', 'checking'].includes(d.state)}
            <button onclick={() => change(d.id, () => api.cancelDownload(d.id))}>Cancel</button>
          {/if}
        </div>
      </div>
    {:else}
      <p class="muted">No downloads.</p>
    {/each}
  {/if}

  <h2>Jobs</h2>
  {#if jobs.error}
    <p role="alert">{jobs.error} <button type="button" onclick={() => void jobs.load()}>Retry</button></p>
  {/if}
  {#if jobs.loaded || !jobs.error}
    {#each jobs.items as job (job.id)}
      <div class="card row job" data-job={job.id}>
        <JobRow {job} all={jobs.items} />
      </div>
    {:else}
      <p class="muted">No jobs queued or running.</p>
    {/each}
  {/if}

  <h2>Recent</h2>
  <ul class="imports" aria-live="polite" aria-label="Recent jobs">
    {#each recent.items as job (job.id)}
      <li>
        <StatusPill {...jobStatus(job)} />
        {#if (job.count ?? 1) > 1}
          {@const run = runOutcome(job, platformName)}
          <a class:error={job.state === 'failed'} href={run.href}>{run.text}</a>
          <span class="muted">{timeRange(job)}</span>
        {:else}
          <span class:error={job.state === 'failed'}>{jobOutcome(job, platformName)}</span>
          <span class="muted">{new Date(job.updated_at * 1000).toLocaleString()}</span>
        {/if}
      </li>
    {:else}
      <li class="muted">No finished jobs yet.</li>
    {/each}
  </ul>

  <h2>Imports</h2>
  {#if imports.error}
    <p role="alert">{imports.error} <button type="button" onclick={() => void imports.load()}>Retry</button></p>
  {/if}
  {#if imports.loaded || !imports.error}
    <ul class="imports">
      {#each imports.items as entry (entry.id)}
        <li>{entry.action}</li>
      {:else}
        <li class="muted">No imports yet.</li>
      {/each}
    </ul>
  {/if}
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

  .actions {
    display: flex;
    gap: 0.5em;
    margin-top: 0.4em;
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
