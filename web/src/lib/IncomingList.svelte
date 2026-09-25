<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { api, errorMessage } from './api';
  import { getIncoming, loadIncoming, patchIncoming, scheduleIncoming, type Watched } from './stores/incoming.svelte';
  import { addUpload, dismissUpload, getUploads } from './stores/uploads.svelte';
  import { getFinishedJob, getJobs } from './stores/jobs.svelte';
  import { showToast } from './stores/toast.svelte';
  import { describeProgress, incomingStatus, jobStatus, type Shown } from './status';
  import StatusPill from './StatusPill.svelte';
  import ProgressBar from './ProgressBar.svelte';
  import type { IncomingFile } from './types';

  /**
   * Lists the files waiting in `dats/` or `sources/` and this session's uploads; with
   * `manage`, a rejected DAT can be retried or deleted.
   */
  let { which, manage = false }: { which: Watched; manage?: boolean } = $props();

  const isMock = import.meta.env.VITE_MOCK === '1';
  let confirming = $state<string | null>(null);
  let busy = $state<Set<string>>(new Set());
  let announcement = $state('');
  let list = $state<HTMLUListElement>();

  onMount(() => {
    void loadIncoming(which).catch(() => undefined);
  });

  const files = $derived(getIncoming(which));
  const pendingNames = $derived(new Set(files.map((f) => f.file)));
  const uploads = $derived(getUploads(which).filter((u) => !pendingNames.has(u.file)));
  const canManage = $derived(manage && which === 'dats');

  function gamesText(p: Record<string, unknown> | null): string {
    return typeof p?.games === 'number' ? `, ${p.games} games` : '';
  }

  /** A running file's progress: the job store's live value, else the list's. */
  function progressOf(f: IncomingFile): Record<string, unknown> | null {
    const job = f.job_id === null ? undefined : getJobs().find((j) => j.id === f.job_id);
    return job?.progress ?? f.progress;
  }

  function uploadOutcome(jobId: number | null, reason: string | null, stale: boolean): { shown: Shown; text: string } {
    const done = jobId === null ? undefined : getFinishedJob(jobId);
    if (done?.state === 'done' && typeof done.progress?.rejected === 'string') {
      return { shown: { status: 'failed', label: 'Rejected' }, text: done.progress.rejected };
    }
    if (done?.state === 'done') {
      const text = which === 'dats' ? `Loaded${gamesText(done.progress)}` : 'Added';
      return { shown: { status: 'done', label: which === 'dats' ? 'Loaded' : 'Added' }, text };
    }
    if (done?.state === 'failed') {
      return { shown: { status: 'failed', label: 'Failed' }, text: 'See Activity for the reason' };
    }
    const open = jobId === null ? undefined : getJobs().find((j) => j.id === jobId);
    if (open) {
      return { shown: jobStatus(open), text: open.reason ?? '' };
    }
    if (stale) {
      return { shown: { status: 'done', label: 'Finished' }, text: 'The lists below show the result' };
    }
    return { shown: { status: 'waiting', label: 'Received' }, text: reason ?? 'Waiting for the import to start' };
  }

  function setBusy(file: string, on: boolean): void {
    busy = new Set(on ? [...busy, file] : [...busy].filter((f) => f !== file));
  }

  // Focuses the button `action` of the row for `file` once the list has re-rendered.
  async function focusOn(file: string, action: string): Promise<void> {
    await tick();
    const rows = list?.querySelectorAll<HTMLButtonElement>(`button[data-action="${action}"]`) ?? [];
    const target = [...rows].find((b) => b.dataset.file === file);
    target?.focus();
  }

  async function focusList(): Promise<void> {
    await tick();
    list?.focus();
  }

  function ask(f: IncomingFile): void {
    confirming = f.file;
    void focusOn(f.file, 'confirm');
  }

  function keep(f: IncomingFile): void {
    confirming = null;
    void focusOn(f.file, 'delete');
  }

  // Moves the file back into dats/; the list shows it waiting until the next read.
  async function retry(f: IncomingFile): Promise<void> {
    setBusy(f.file, true);
    try {
      if (isMock) {
        patchIncoming(which, f.file, { ...f, state: 'waiting', reason: 'Queued.', job_id: 99 });
      } else {
        const up = await api.retryRejectedDat(f.file);
        addUpload({ kind: which, file: up.file, jobId: up.job_id, reason: up.reason });
        patchIncoming(which, f.file, null);
        patchIncoming(which, up.file, up);
        scheduleIncoming(which);
      }
      announcement = `${f.file} queued to load again.`;
      await focusList();
    } catch (err) {
      showToast(`${f.file}: ${errorMessage(err)}`);
      announcement = `${f.file}: ${errorMessage(err)}`;
    } finally {
      setBusy(f.file, false);
    }
  }

  async function remove(f: IncomingFile): Promise<void> {
    confirming = null;
    setBusy(f.file, true);
    try {
      if (!isMock) {
        await api.deleteRejectedDat(f.file);
      }
      patchIncoming(which, f.file, null);
      announcement = `${f.file} deleted.`;
      await focusList();
    } catch (err) {
      showToast(`${f.file}: ${errorMessage(err)}`);
      announcement = `${f.file}: ${errorMessage(err)}`;
      void focusOn(f.file, 'delete');
    } finally {
      setBusy(f.file, false);
    }
  }
</script>

{#if canManage}<p class="live" aria-live="polite">{announcement}</p>{/if}
<ul
  class="incoming"
  aria-label={which === 'dats' ? 'Files in dats' : 'Files in sources'}
  tabindex="-1"
  bind:this={list}
>
  {#each files as f (f.file)}
    {@const shown = incomingStatus(f)}
    <li class:rejected={f.state === 'rejected'}>
      <span class="name">{f.file}</span>
      <StatusPill {...shown} />
      {#if f.state === 'rejected'}
        {#if f.reason}<p class="reason">{f.reason}</p>{/if}
        {#if canManage}
          <span class="actions">
            <button
              type="button"
              data-file={f.file}
              data-action="retry"
              aria-label={`Retry ${f.file}`}
              disabled={busy.has(f.file)}
              onclick={() => retry(f)}>Retry</button
            >
            {#if confirming === f.file}
              <button
                type="button"
                class="danger"
                data-file={f.file}
                data-action="confirm"
                aria-label={`Delete the file ${f.file}`}
                disabled={busy.has(f.file)}
                onclick={() => remove(f)}>Delete the file</button
              >
              <button type="button" aria-label={`Keep ${f.file}`} onclick={() => keep(f)}>Keep</button>
            {:else}
              <button
                type="button"
                data-file={f.file}
                data-action="delete"
                aria-label={`Delete ${f.file}`}
                disabled={busy.has(f.file)}
                onclick={() => ask(f)}>Delete</button
              >
            {/if}
          </span>
        {/if}
      {:else if f.state === 'importing'}
        {@const view = describeProgress(which === 'dats' ? 'dat_import' : 'source_import', progressOf(f))}
        <div class="progress-row">
          <ProgressBar view={view ?? { fraction: null, text: '' }} label={`${f.file} import progress`} />
        </div>
      {:else if f.reason}
        <span class="muted why">{f.reason}</span>
      {/if}
    </li>
  {/each}
  {#each uploads as u (u.file)}
    {@const outcome = uploadOutcome(u.jobId, u.reason, u.stale ?? false)}
    <li>
      <span class="name">{u.file}</span>
      <StatusPill {...outcome.shown} />
      {#if outcome.text}<span class="muted why">{outcome.text}</span>{/if}
      <button type="button" class="link" aria-label={`Dismiss ${u.file}`} onclick={() => dismissUpload(which, u.file)}
        >Dismiss</button
      >
    </li>
  {/each}
  {#if files.length === 0 && uploads.length === 0}
    <li class="muted">No files waiting.</li>
  {/if}
</ul>

<style>
  .live {
    margin: 0;
    font-size: 0.85em;
  }

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

  .why {
    font-size: 0.9em;
  }

  .progress-row {
    flex-basis: 100%;
    max-width: 32rem;
  }

  .link {
    padding: 0 0.4em;
    font-size: 0.85em;
  }
</style>
