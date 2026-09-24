<script lang="ts">
  import { onMount } from 'svelte';
  import { getIncoming, loadIncoming, type Watched } from './stores/incoming.svelte';
  import { dismissUpload, getUploads } from './stores/uploads.svelte';
  import { getFinishedJob, getJobs } from './stores/jobs.svelte';
  import type { IncomingFile } from './types';

  /** Lists the files waiting in `dats/` or `sources/` and this session's uploads. */
  let { which }: { which: Watched } = $props();

  onMount(() => {
    void loadIncoming(which).catch(() => undefined);
  });

  const files = $derived(getIncoming(which));
  const pendingNames = $derived(new Set(files.map((f) => f.file)));
  const uploads = $derived(getUploads(which).filter((u) => !pendingNames.has(u.file)));

  function progressText(p: Record<string, unknown> | null): string {
    if (!p) {
      return '';
    }
    const parts: string[] = [];
    if (typeof p.members === 'number' && typeof p.done === 'number') {
      parts.push(`${p.done} of ${p.members} files`);
    }
    if (typeof p.games === 'number') {
      parts.push(`${p.games} games`);
    }
    return parts.join(', ');
  }

  function stateText(f: IncomingFile): string {
    if (f.state === 'importing') {
      return 'Importing';
    }
    return f.state === 'rejected' ? 'Rejected' : 'Waiting';
  }

  function uploadOutcome(jobId: number): { text: string; kind: string } {
    const done = getFinishedJob(jobId);
    if (done?.state === 'done') {
      const games = progressText(done.progress);
      return { text: which === 'dats' ? `Loaded${games ? `, ${games}` : ''}` : 'Added', kind: 'ok' };
    }
    if (done?.state === 'failed') {
      return { text: 'Failed; see Activity for the reason', kind: 'error' };
    }
    const running = getJobs().find((j) => j.id === jobId);
    if (running) {
      return { text: running.reason ?? `Import ${running.state}`, kind: 'muted' };
    }
    return { text: 'Uploaded, waiting for the import to start', kind: 'muted' };
  }
</script>

<ul class="incoming" aria-label={which === 'dats' ? 'Files in dats' : 'Files in sources'}>
  {#each files as f (f.file)}
    <li>
      <span class="name">{f.file}</span>
      <span class={f.state === 'rejected' ? 'error' : 'muted'}>
        {stateText(f)}{f.reason ? `: ${f.reason}` : ''}
        {#if f.state === 'importing' && f.progress}({progressText(f.progress)}){/if}
      </span>
    </li>
  {/each}
  {#each uploads as u (u.jobId)}
    {@const outcome = uploadOutcome(u.jobId)}
    <li>
      <span class="name">{u.file}</span>
      <span class={outcome.kind}>{outcome.text}</span>
      <button type="button" class="link" onclick={() => dismissUpload(u.jobId)}>Dismiss</button>
    </li>
  {/each}
  {#if files.length === 0 && uploads.length === 0}
    <li class="muted">No files waiting.</li>
  {/if}
</ul>

<style>
  .incoming {
    list-style: none;
    padding: 0;
    margin: 0.4em 0;
  }

  .incoming li {
    display: flex;
    flex-wrap: wrap;
    gap: 0.2em 0.6em;
    padding: 0.3em 0;
    border-bottom: 1px solid var(--border);
    overflow-wrap: anywhere;
  }

  .name {
    font-weight: 600;
  }

  .error {
    color: var(--danger);
  }

  .ok {
    color: var(--ok);
  }

  .link {
    padding: 0 0.4em;
    font-size: 0.85em;
  }
</style>
