<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorMessage } from './api';
  import { getIncoming, loadIncoming, patchIncoming, scheduleIncoming, type Watched } from './stores/incoming.svelte';
  import { addUpload, dismissUpload, getUploads } from './stores/uploads.svelte';
  import { getFinishedJob, getJobs } from './stores/jobs.svelte';
  import { showToast } from './stores/toast.svelte';
  import type { IncomingFile } from './types';

  /**
   * Lists the files waiting in `dats/` or `sources/` and this session's uploads; with
   * `manage`, a rejected DAT can be retried or deleted.
   */
  let { which, manage = false }: { which: Watched; manage?: boolean } = $props();

  const isMock = import.meta.env.VITE_MOCK === '1';
  let confirming = $state<string | null>(null);
  let busy = $state<string | null>(null);

  onMount(() => {
    void loadIncoming(which).catch(() => undefined);
  });

  const files = $derived(getIncoming(which));
  const pendingNames = $derived(new Set(files.map((f) => f.file)));
  const uploads = $derived(getUploads(which).filter((u) => !pendingNames.has(u.file)));
  const canManage = $derived(manage && which === 'dats');

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

  function uploadOutcome(jobId: number, stale: boolean): { text: string; kind: string } {
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
    if (stale) {
      return { text: 'Finished; the lists below show the result', kind: 'muted' };
    }
    return { text: 'Uploaded, waiting for the import to start', kind: 'muted' };
  }

  // Moves the file back into dats/; the list shows it waiting until the next read.
  async function retry(f: IncomingFile): Promise<void> {
    busy = f.file;
    try {
      if (isMock) {
        patchIncoming(which, f.file, { ...f, state: 'waiting', reason: 'Queued.' });
        return;
      }
      const up = await api.retryRejectedDat(f.file);
      addUpload({ kind: which, file: up.file, jobId: up.job_id });
      patchIncoming(which, f.file, { ...f, file: up.file, state: 'waiting', reason: 'Queued.', job_id: up.job_id });
      scheduleIncoming(which);
    } catch (err) {
      showToast(`${f.file}: ${errorMessage(err)}`);
    } finally {
      busy = null;
    }
  }

  async function remove(f: IncomingFile): Promise<void> {
    confirming = null;
    busy = f.file;
    try {
      if (!isMock) {
        await api.deleteRejectedDat(f.file);
      }
      patchIncoming(which, f.file, null);
    } catch (err) {
      showToast(`${f.file}: ${errorMessage(err)}`);
    } finally {
      busy = null;
    }
  }
</script>

<ul class="incoming" aria-label={which === 'dats' ? 'Files in dats' : 'Files in sources'}>
  {#each files as f (f.file)}
    <li class:rejected={f.state === 'rejected'}>
      <span class="name">{f.file}</span>
      {#if f.state === 'rejected'}
        <span class="error state">Rejected</span>
        {#if f.reason}<p class="reason">{f.reason}</p>{/if}
        {#if canManage}
          <span class="actions">
            <button type="button" disabled={busy === f.file} onclick={() => retry(f)}>Retry</button>
            {#if confirming === f.file}
              <button type="button" class="danger" disabled={busy === f.file} onclick={() => remove(f)}>
                Delete the file
              </button>
              <button type="button" onclick={() => (confirming = null)}>Keep</button>
            {:else}
              <button type="button" disabled={busy === f.file} onclick={() => (confirming = f.file)}>Delete</button>
            {/if}
          </span>
        {/if}
      {:else}
        <span class="muted">
          {stateText(f)}{f.reason ? `: ${f.reason}` : ''}
          {#if f.state === 'importing' && f.progress}({progressText(f.progress)}){/if}
        </span>
      {/if}
    </li>
  {/each}
  {#each uploads as u (u.jobId)}
    {@const outcome = uploadOutcome(u.jobId, u.stale ?? false)}
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
    align-items: baseline;
    gap: 0.2em 0.6em;
    padding: 0.3em 0;
    border-bottom: 1px solid var(--border);
    overflow-wrap: anywhere;
  }

  .name {
    font-weight: 600;
  }

  .state {
    font-weight: 600;
  }

  .reason {
    flex-basis: 100%;
    margin: 0.2em 0;
    padding: 0.4em 0.6em;
    border-left: 3px solid var(--danger);
    background: var(--bg);
    color: var(--danger);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
  }

  .actions button {
    padding: 0.3em 0.7em;
    font-size: 0.9em;
  }

  .danger {
    border-color: var(--danger);
    color: var(--danger);
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
