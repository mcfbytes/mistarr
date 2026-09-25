<script lang="ts">
  import { cancelFetch, fetchToken } from './fetch';
  import { isCancelling } from './stores/jobs.svelte';
  import type { Job } from './types';

  /** A URL fetch's Cancel button, disabled and saying so while the cancel is pending. */
  let { job, label, compact = false }: { job: Pick<Job, 'kind' | 'payload'>; label: string; compact?: boolean } =
    $props();

  const token = $derived(fetchToken(job));
  const pending = $derived(token !== null && isCancelling(token));
</script>

{#if token !== null}
  <button
    class:compact
    aria-label={pending ? `Cancelling ${label}` : `Cancel ${label}`}
    aria-busy={pending}
    disabled={pending}
    onclick={() => cancelFetch(job)}>{pending ? 'Cancelling…' : 'Cancel'}</button
  >
{/if}

<style>
  .compact {
    margin-left: auto;
    padding: 0.15em 0.6em;
    font-size: 0.85em;
  }
</style>
