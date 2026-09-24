<script lang="ts">
  import { onMount } from 'svelte';
  import { getDownloads, getImports, loadDownloads, loadImports } from '../lib/stores/downloads.svelte';
  import { getJobs, loadJobs } from '../lib/stores/jobs.svelte';

  onMount(() => {
    void loadDownloads();
    void loadImports();
    void loadJobs();
  });

  const downloads = $derived(getDownloads());
  const imports = $derived(getImports());
  const jobs = $derived(getJobs());
</script>

<div class="page">
  <h1>Activity</h1>

  <h2>Downloads</h2>
  {#each downloads as d (d.id)}
    <div class="card row">
      <div>
        <strong>{d.title_name}</strong>
        <span class="muted">{d.state}</span>
        {#if d.error}<span class="error">{d.error}</span>{/if}
      </div>
      <div class="progress"><span style={`width: ${Math.round(d.progress * 100)}%`}></span></div>
    </div>
  {:else}
    <p class="muted">No downloads.</p>
  {/each}

  <h2>Running jobs</h2>
  {#each jobs as job (job.id)}
    <div class="card row">
      <strong>{job.kind}</strong>
      <span class="muted">{job.state}</span>
    </div>
  {:else}
    <p class="muted">No jobs running.</p>
  {/each}

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

  .row > div:first-child {
    display: flex;
    gap: 0.6em;
    margin-bottom: 0.3em;
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
