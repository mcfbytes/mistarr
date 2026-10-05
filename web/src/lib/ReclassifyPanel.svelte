<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import { attempt } from './actions';
  import { api, errorMessage } from './api';
  import { describeProgress, jobOutcome, jobStatus } from './status';
  import { followJob, jobs } from './stores/jobs.svelte';
  import { platformName } from './stores/platforms.svelte';
  import { sources } from './stores/sources.svelte';
  import { showToast } from './stores/toast.svelte';
  import ProgressBar from './ProgressBar.svelte';
  import StatusPill from './StatusPill.svelte';
  import type { SourceDetail, SourcePreview } from './types';

  /**
   * The Re-classify and Reset buttons of a source, the platform preview they open and the
   * progress of the binding they queue; `onchange` runs when the source's binding has moved.
   */
  const { source, onchange }: { source: SourceDetail; onchange: () => void } = $props();

  const NONE = '-';

  let panelOpen = $state(false);
  let reclassifyButton = $state<HTMLButtonElement | null>(null);
  let previewController: AbortController | null = null;
  let preview = $state<SourcePreview | null>(null);
  let previewError = $state<string | null>(null);
  let choice = $state('');
  let applying = $state(false);
  let resetting = $state(false);
  let jobId = $state<number | null>(null);
  let stopBinding: (() => void) | null = null;

  const job = $derived(jobId === null ? undefined : jobs.items.find((j) => j.id === jobId));
  const jobView = $derived(job ? describeProgress(job.kind, job.progress) : null);
  const sampled = $derived(preview !== null && preview.sampled < preview.total);
  const chosenMatch = $derived(
    choice && choice !== NONE ? (preview?.platforms.find((p) => p.platform_id === choice)?.matched ?? 0) : null
  );

  onDestroy(() => {
    previewController?.abort();
    stopBinding?.();
  });

  /** Closes Re-classify, stops its preview request and returns focus to its button. */
  async function closePanel(): Promise<void> {
    panelOpen = false;
    previewController?.abort();
    previewController = null;
    await tick();
    reclassifyButton?.focus();
  }

  // The preview is read once per page view; the page is rebuilt for another source.
  async function openPanel(): Promise<void> {
    if (panelOpen) {
      await closePanel();
      return;
    }
    panelOpen = true;
    choice = '';
    if (preview || previewController) {
      return;
    }
    previewError = null;
    const c = new AbortController();
    previewController = c;
    try {
      const got = await api.sourcePreview(source.id, c.signal);
      if (!c.signal.aborted) {
        preview = got;
      }
    } catch (err) {
      if (!c.signal.aborted) {
        previewError = errorMessage(err);
      }
    } finally {
      if (previewController === c) {
        previewController = null;
      }
    }
  }

  function confirmText(): string {
    if (choice === NONE) {
      return 'The source will have no platform and is never bound automatically. Its files keep no matches.';
    }
    const name = platformName(choice);
    const about = sampled ? 'about ' : '';
    return `Binding to ${name} would match ${about}${chosenMatch ?? 0} of ${preview?.total ?? 0} files. Its files are matched again in the background, and later DAT loads keep this choice.`;
  }

  // Once the queued binding of job `id` ends, says how it went and reads the sources again.
  function followBinding(id: number | null): void {
    stopBinding?.();
    jobId = id;
    if (id === null) {
      return;
    }
    stopBinding = followJob(
      (end) => end.id === id,
      (end) => {
        const payload = { source_name: source.display_name };
        const text = jobOutcome({ kind: 'bind_source', state: end.state, progress: end.progress, payload }, platformName);
        jobId = null;
        showToast(text, end.state === 'done' ? 'success' : 'error');
        void sources.load();
        onchange();
      }
    );
  }

  async function apply(): Promise<void> {
    if (!choice || applying) {
      return;
    }
    applying = true;
    const platformId = choice === NONE ? null : choice;
    const updated = await attempt(() => api.updateSource(source.id, { platform_id: platformId }));
    if (updated) {
      followBinding(updated.job_id);
      sources.patch(source.id, { user_binding: updated.user_binding, pending_binding: updated.pending_binding });
      onchange();
      void closePanel();
      showToast(platformId ? `Binding to ${platformName(platformId)} queued.` : 'Setting the source aside queued.', 'info');
    }
    applying = false;
  }

  async function reset(): Promise<void> {
    if (resetting) {
      return;
    }
    resetting = true;
    const updated = await attempt(() => api.updateSource(source.id, { binding: 'automatic' }));
    if (updated) {
      followBinding(updated.job_id);
      sources.patch(source.id, { user_binding: updated.user_binding, pending_binding: updated.pending_binding });
      onchange();
      showToast('Automatic binding queued.', 'info');
      // Reset leaves with the user's binding; focus goes to the control that stays.
      await tick();
      reclassifyButton?.focus();
    }
    resetting = false;
  }
</script>

{#if jobId !== null}
  <div class="job" aria-live="polite">
    {#if job && jobView}
      <ProgressBar label="Binding progress" view={jobView} />
    {:else if job}
      <StatusPill {...jobStatus(job)} />
      {#if job.reason}<span class="muted">{job.reason}</span>{/if}
    {:else}
      <span class="muted">Binding queued.</span>
    {/if}
  </div>
{/if}

<div class="actions">
  <button
    bind:this={reclassifyButton}
    aria-expanded={panelOpen}
    aria-controls="reclassify"
    disabled={source.file_count === 0}
    onclick={() => void openPanel()}>Re-classify…</button
  >
  {#if source.user_binding}
    <button onclick={() => void reset()} aria-disabled={resetting} aria-busy={resetting}
      >{resetting ? 'Resetting…' : 'Reset to automatic'}</button
    >
  {/if}
</div>

{#if panelOpen}
  <div id="reclassify" class="panel" role="region" aria-label="Re-classify">
    {#if previewError}
      <p role="alert">{previewError}</p>
    {:else if !preview}
      <p class="muted" aria-busy="true">Checking how each platform's DAT entries match these files…</p>
    {:else}
      <fieldset>
        <legend>Bind this source to</legend>
        {#if preview.platforms.length === 0}
          <p class="muted">No platform has a DAT loaded.</p>
        {/if}
        {#each preview.platforms as p (p.platform_id)}
          <label class="option">
            <input type="radio" name="bind-to" value={p.platform_id} bind:group={choice} />
            <span>
              {platformName(p.platform_id)}{#if p.platform_id === source.platform_id}&nbsp;<span class="muted">(now)</span>{/if}
              <span class="muted hint"
                >would match {sampled ? 'about ' : ''}{p.matched.toLocaleString()} of {preview.total.toLocaleString()} files</span
              >
            </span>
          </label>
        {/each}
        <label class="option">
          <input type="radio" name="bind-to" value={NONE} bind:group={choice} />
          <span>Not a game set <span class="muted hint">no platform, never bound automatically</span></span>
        </label>
      </fieldset>
      {#if choice}
        <p class="confirm">{confirmText()}</p>
      {/if}
      <div class="actions">
        <button class="primary" onclick={() => void apply()} aria-disabled={!choice || applying} aria-busy={applying}>
          {applying ? 'Applying…' : choice === NONE ? 'Set aside' : choice ? `Bind to ${platformName(choice)}` : 'Bind'}
        </button>
        <button onclick={() => void closePanel()}>Cancel</button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5em;
    margin-top: 0.6em;
  }

  .job {
    margin-top: 0.6em;
  }

  .panel {
    margin-top: 0.8em;
    border-top: 1px solid var(--border);
    padding-top: 0.8em;
  }

  fieldset {
    border: none;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 0.4em;
  }

  legend {
    margin-bottom: 0.4em;
  }

  .option {
    display: flex;
    gap: 0.5em;
    align-items: baseline;
  }

  .hint {
    display: block;
    font-size: 0.85em;
  }

  .confirm {
    margin: 0.8em 0 0;
  }
</style>
