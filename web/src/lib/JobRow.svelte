<script lang="ts">
  import { platformName } from './stores/platforms.svelte';
  import { describeProgress, jobHref, jobStatus, jobTitle } from './status';
  import FetchCancel from './FetchCancel.svelte';
  import StatusPill from './StatusPill.svelte';
  import ProgressBar from './ProgressBar.svelte';
  import type { Job } from './types';

  /**
   * One job's status, linked title, Cancel for a URL fetch, progress while it runs and its
   * reason; `all` numbers unnamed fetches and `compact` is the panel's tighter form.
   */
  let {
    job,
    all,
    compact = false,
    onnavigate
  }: { job: Job; all: readonly Job[]; compact?: boolean; onnavigate?: () => void } = $props();

  const title = $derived(jobTitle(job, all, platformName));
  const view = $derived(
    job.state === 'running' ? (describeProgress(job.kind, job.progress) ?? { fraction: null, text: 'Starting' }) : null
  );
</script>

<div class="head" class:compact>
  <StatusPill {...jobStatus(job)} />
  <a href={jobHref(job)} onclick={onnavigate}>{#if compact}{title}{:else}<strong>{title}</strong>{/if}</a>
  {#if !compact}<span class="muted">{job.lane} lane</span>{/if}
  <FetchCancel {job} label={title} {compact} />
</div>
{#if view}
  <ProgressBar {view} label={`${title} progress`} {compact} />
{/if}
{#if job.reason}<p class="why" class:muted={!compact}>{job.reason}</p>{/if}

<style>
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.6em;
    margin-bottom: 0.4em;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .head.compact {
    flex-wrap: nowrap;
    gap: 0.5em;
    margin-bottom: 0.3em;
  }

  .compact a {
    min-width: 0;
    color: var(--fg);
  }

  .why {
    margin: 0.4em 0 0;
    font-size: 0.85em;
    overflow-wrap: anywhere;
  }

  .why:not(.muted) {
    margin: 0;
    font-size: 0.8rem;
    color: var(--fg-dim);
  }
</style>
