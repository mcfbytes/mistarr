<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { api, ApiError, errorMessage } from '../lib/api';
  import { findSource, loadSources, patchSource } from '../lib/stores/sources.svelte';
  import { findPlatform, loadPlatforms, platformsLoaded } from '../lib/stores/platforms.svelte';
  import { getFinishedJob, getJobs, jobOutcome, runMockJob } from '../lib/stores/jobs.svelte';
  import { showToast } from '../lib/stores/toast.svelte';
  import { fixtureSources, mockSourceDetail, mockSourceFilesPage, mockSourcePreview } from '../lib/fixtures';
  import { describeProgress, jobStatus, sourceStatus } from '../lib/status';
  import { confidenceLabel } from '../lib/availability';
  import { titleUrl } from '../lib/router.svelte';
  import {
    bindingText,
    downloadText,
    formatSize,
    kindText,
    shortHash,
    transferShare,
    unmatchedText
  } from '../lib/sourceDetail';
  import StatusPill from '../lib/StatusPill.svelte';
  import ProgressBar from '../lib/ProgressBar.svelte';
  import SeedPolicySelect from '../lib/SeedPolicySelect.svelte';
  import type { SeedPolicy, SourceDetail, SourceFile, SourceFileFilter, SourcePreview } from '../lib/types';

  const { sourceId }: { sourceId: number } = $props();

  const isMock = import.meta.env.VITE_MOCK === '1';
  const PAGE = 50;
  const NONE = '-';
  const FILTERS: { value: SourceFileFilter | undefined; label: string }[] = [
    { value: undefined, label: 'All' },
    { value: 'matched', label: 'Matched' },
    { value: 'unmatched', label: 'Unmatched' },
    { value: 'wanted', label: 'Wanted' }
  ];

  let detail = $state<SourceDetail | null>(null);
  let missing = $state(false);
  let loadError = $state<string | null>(null);

  let filter = $state<SourceFileFilter | undefined>(undefined);
  let query = $state('');
  let applied = $state('');
  let offset = $state(0);
  let files = $state<SourceFile[]>([]);
  let total = $state(0);
  let filesLoading = $state(false);
  let filesError = $state<string | null>(null);
  let reloadTick = $state(0);
  let controller: AbortController | null = null;
  let searchTimer: ReturnType<typeof setTimeout> | null = null;

  let panelOpen = $state(false);
  let reclassifyButton = $state<HTMLButtonElement | null>(null);
  let previewController: AbortController | null = null;
  let preview = $state<SourcePreview | null>(null);
  let previewError = $state<string | null>(null);
  let choice = $state('');
  let applying = $state(false);
  let resetting = $state(false);
  let jobId = $state<number | null>(null);
  let mockJobs = 0;

  const row = $derived(findSource(sourceId));
  const source = $derived(detail ?? null);
  const job = $derived(jobId === null ? undefined : getJobs().find((j) => j.id === jobId));
  const jobView = $derived(job ? describeProgress(job.kind, job.progress) : null);

  function platformName(id: string): string {
    return findPlatform(id)?.name ?? id;
  }

  $effect(() => {
    if (!platformsLoaded()) {
      void loadPlatforms().catch(() => undefined);
    }
    if (!row) {
      void loadSources().catch(() => undefined);
    }
  });

  async function loadDetail(): Promise<void> {
    if (isMock) {
      const r = findSource(sourceId);
      missing = !r;
      detail = r ? mockSourceDetail(r) : null;
      return;
    }
    try {
      detail = await api.source(sourceId);
      missing = false;
      loadError = null;
    } catch (err) {
      missing = err instanceof ApiError && err.status === 404;
      loadError = missing ? null : errorMessage(err);
    }
  }

  // The list row changes on `source.changed`; its binding and counts moving means a re-read.
  const rowKey = $derived(
    JSON.stringify([
      sourceId,
      row?.state,
      row?.platform_id,
      row?.matched_count,
      row?.user_binding,
      row?.pending_binding,
      row?.file_count
    ])
  );
  $effect(() => {
    if (rowKey) {
      untrack(() => {
        void loadDetail();
        reloadTick += 1;
      });
    }
  });

  interface PageAsk {
    filter: SourceFileFilter | undefined;
    q: string | undefined;
    limit: number;
    offset: number;
  }

  function currentAsk(): PageAsk {
    return { filter, q: applied || undefined, limit: PAGE, offset };
  }

  async function loadFiles(opts: PageAsk): Promise<void> {
    controller?.abort();
    const c = new AbortController();
    controller = c;
    filesLoading = true;
    try {
      const r = findSource(sourceId);
      const page = isMock
        ? r
          ? mockSourceFilesPage(r, opts)
          : { items: [], total: 0 }
        : await api.sourceFiles(sourceId, opts, c.signal);
      if (c.signal.aborted) {
        return;
      }
      files = page.items;
      total = page.total;
      filesError = null;
    } catch (err) {
      if (!c.signal.aborted) {
        filesError = errorMessage(err);
      }
    } finally {
      if (controller === c) {
        filesLoading = false;
      }
    }
  }

  $effect(() => {
    const ask = currentAsk();
    if (reloadTick >= 0) {
      untrack(() => void loadFiles(ask));
    }
  });

  onDestroy(() => {
    controller?.abort();
    previewController?.abort();
    if (searchTimer) {
      clearTimeout(searchTimer);
    }
  });

  function setFilter(value: SourceFileFilter | undefined): void {
    filter = value;
    offset = 0;
  }

  function onSearch(value: string): void {
    query = value;
    if (searchTimer) {
      clearTimeout(searchTimer);
    }
    searchTimer = setTimeout(() => {
      applied = query.trim();
      offset = 0;
    }, 250);
  }

  let fullHash = $state(false);

  async function copyHash(hash: string): Promise<void> {
    // Plain http on the LAN has no clipboard API; the whole infohash is shown to copy by hand.
    if (!window.isSecureContext) {
      fullHash = true;
      showToast('This page cannot copy over plain http. The whole infohash is shown to copy.', 'info');
      return;
    }
    try {
      await navigator.clipboard.writeText(hash);
      showToast('Infohash copied.', 'success');
    } catch {
      fullHash = true;
      showToast('The browser did not allow copying. The whole infohash is shown to copy.');
    }
  }

  async function setSeedPolicy(policy: SeedPolicy): Promise<void> {
    const prev = detail?.seed_policy;
    patchSource(sourceId, { seed_policy: policy });
    if (detail) {
      detail = { ...detail, seed_policy: policy };
    }
    if (isMock) {
      return;
    }
    try {
      const updated = await api.updateSource(sourceId, { seed_policy: policy });
      patchSource(sourceId, updated);
    } catch (err) {
      if (prev !== undefined) {
        patchSource(sourceId, { seed_policy: prev });
        if (detail) {
          detail = { ...detail, seed_policy: prev };
        }
      }
      showToast(errorMessage(err));
    }
  }

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
      const r = findSource(sourceId);
      const got = isMock ? (r ? mockSourcePreview(r) : null) : await api.sourcePreview(sourceId, c.signal);
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

  const sampled = $derived(preview !== null && preview.sampled < preview.total);

  const chosenMatch = $derived(
    choice && choice !== NONE ? (preview?.platforms.find((p) => p.platform_id === choice)?.matched ?? 0) : null
  );

  function confirmText(): string {
    if (choice === NONE) {
      return 'The source will have no platform and is never bound automatically. Its files keep no matches.';
    }
    const name = platformName(choice);
    const about = sampled ? 'about ' : '';
    return `Binding to ${name} would match ${about}${chosenMatch ?? 0} of ${preview?.total ?? 0} files. Its files are matched again in the background, and later DAT loads keep this choice.`;
  }

  /** Mock mode: runs a binding job, then shows the source as the server would leave it. */
  function mockBinding(platformId: string | null, automatic: boolean, matched: number): void {
    mockJobs += 1;
    const id = 9000 + mockJobs;
    const name = detail?.display_name ?? '';
    const payload: Record<string, unknown> = { source_id: sourceId, source_name: name };
    jobId = id;
    const fileCount = detail?.file_count ?? 0;
    const job = {
      id,
      kind: 'bind_source',
      lane: 'background' as const,
      payload,
      state: 'running' as const,
      progress: { phase: 'binding', source_id: sourceId },
      reason: null,
      created_at: 0,
      updated_at: 0
    };
    runMockJob(job, { source_id: sourceId, platform_id: platformId, matched, total: fileCount }, 2500, () => {
      patchSource(sourceId, {
        pending_binding: null,
        platform_id: platformId,
        state: platformId ? 'bound' : 'unbound',
        matched_count: matched,
        bind_score: platformId && fileCount > 0 ? matched / fileCount : null,
        reason: platformId ? null : automatic ? detail?.reason ?? null : 'Marked as not a game set. It is not bound automatically.'
      });
    });
  }

  async function apply(): Promise<void> {
    if (!choice || applying) {
      return;
    }
    applying = true;
    const platformId = choice === NONE ? null : choice;
    try {
      if (isMock) {
        patchSource(sourceId, { user_binding: true, pending_binding: { automatic: false, platform_id: platformId } });
        mockBinding(platformId, false, chosenMatch ?? 0);
      } else {
        const updated = await api.updateSource(sourceId, { platform_id: platformId });
        jobId = updated.job_id;
        patchSource(sourceId, { user_binding: updated.user_binding, pending_binding: updated.pending_binding });
      }
      void closePanel();
      showToast(platformId ? `Binding to ${platformName(platformId)} queued.` : 'Setting the source aside queued.', 'info');
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      applying = false;
    }
  }

  async function reset(): Promise<void> {
    if (resetting) {
      return;
    }
    resetting = true;
    try {
      if (isMock) {
        const original = fixtureSources.find((s) => s.id === sourceId);
        patchSource(sourceId, { user_binding: false, pending_binding: { automatic: true, platform_id: null } });
        mockBinding(original?.platform_id ?? null, true, original?.matched_count ?? 0);
      } else {
        const updated = await api.updateSource(sourceId, { binding: 'automatic' });
        jobId = updated.job_id;
        patchSource(sourceId, { user_binding: updated.user_binding, pending_binding: updated.pending_binding });
      }
      showToast('Automatic binding queued.', 'info');
      // Reset leaves with the user's binding; focus goes to the control that stays.
      await tick();
      reclassifyButton?.focus();
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      resetting = false;
    }
  }

  // Once the queued binding finishes, say how it went and read the source again.
  $effect(() => {
    if (jobId === null) {
      return;
    }
    const done = getFinishedJob(jobId);
    if (!done) {
      return;
    }
    const text = jobOutcome({ kind: 'bind_source', state: done.state, progress: done.progress, payload: { source_name: detail?.display_name } }, platformName);
    untrack(() => {
      jobId = null;
      showToast(text, done.state === 'done' ? 'success' : 'error');
      if (!isMock) {
        void loadSources().catch(() => undefined);
      }
    });
  });

  const firstShown = $derived(total === 0 ? 0 : offset + 1);
  const lastShown = $derived(Math.min(offset + PAGE, total));
</script>

<div class="page">
  <p class="back"><a href="#/sources">Sources</a></p>
  {#if missing}
    <h1>Source not found</h1>
    <p class="muted">No source has this id. It may have been removed.</p>
  {:else if loadError}
    <h1>Source</h1>
    <p role="alert">{loadError} <button onclick={() => void loadDetail()}>Retry</button></p>
  {:else if !source}
    <h1>Source</h1>
    <p class="muted" aria-busy="true">Loading…</p>
  {:else}
    <h1>{source.display_name}</h1>

    <section class="card head" aria-label="Overview">
      <dl>
        <div><dt>Size</dt><dd>{formatSize(source.total_size)}</dd></div>
        <div><dt>Files</dt><dd>{source.file_count.toLocaleString()}</dd></div>
        <div>
          <dt>Infohash</dt>
          <dd class="hash">
            {#if fullHash}
              <input class="full" readonly value={source.infohash} aria-label="Infohash" onfocus={(e) => e.currentTarget.select()} />
            {:else}
              <code title={source.infohash}>{shortHash(source.infohash)}</code>
            {/if}
            <button class="small" onclick={() => void copyHash(source.infohash)} aria-label="Copy infohash">Copy</button>
          </dd>
        </div>
        <div><dt>Added</dt><dd>{new Date(source.added_at * 1000).toLocaleDateString()}</dd></div>
        <div><dt>File</dt><dd class="wrap">{source.origin_file}</dd></div>
        <div>
          <dt>Client</dt>
          <dd>
            {#if source.client_id}
              In the client{#if source.transfer.files > 0}, {source.transfer.files} {source.transfer.files === 1 ? 'file' : 'files'} selected{/if}
            {:else}
              Not in the client
            {/if}
          </dd>
        </div>
      </dl>
      {#if source.transfer.files > 0}
        {@const share = transferShare(source.transfer)}
        <ProgressBar
          label="Transfer of the selected files"
          view={{
            fraction: share,
            text: `${formatSize(source.transfer.done)} of ${formatSize(source.transfer.size)}`
          }}
        />
      {/if}
    </section>

    <section class="card" aria-labelledby="seed-h">
      <h2 id="seed-h">Seed policy</h2>
      <div class="seed">
        <SeedPolicySelect policy={source.seed_policy} label="Seed policy" onpick={(p: SeedPolicy) => void setSeedPolicy(p)} />
        <span class="muted">How long the client keeps sharing this source's files once they are complete.</span>
      </div>
    </section>

    <section class="card" aria-labelledby="class-h">
      <h2 id="class-h">Classification</h2>
      <div class="state">
        <StatusPill {...sourceStatus(source.state)} />
        {#if source.user_binding}<span class="tag" data-testid="overridden">Set by you</span>{/if}
      </div>
      <p>{bindingText(source, platformName)}</p>
      {#if source.reason && !source.user_binding}<p class="muted">{source.reason}</p>{/if}
      {#if source.dats.length > 0}
        <p class="muted">
          Matched against
          {#each source.dats as d, i (d.dat_version_id)}{i > 0 ? '; ' : ' '}{d.dat_name} ({d.version}), {d.matched.toLocaleString()}
            {d.matched === 1 ? 'file' : 'files'}{/each}.
        </p>
      {/if}
      <ul class="counts" aria-label="Files by match">
        <li><strong>{source.summary.matched.toLocaleString()}</strong> matched</li>
        <li><strong>{source.summary.candidates.toLocaleString()}</strong> {source.summary.candidates === 1 ? 'possible match' : 'possible matches'}</li>
        <li><strong>{source.summary.unmatched.toLocaleString()}</strong> unmatched</li>
        <li><strong>{source.summary.extra.toLocaleString()}</strong> {source.summary.extra === 1 ? 'extra' : 'extras'} (text, images, checksums)</li>
        <li><strong>{source.summary.wanted.toLocaleString()}</strong> wanted</li>
      </ul>

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
          <button
            onclick={() => void reset()}
            aria-disabled={resetting}
            aria-busy={resetting}>{resetting ? 'Resetting…' : 'Reset to automatic'}</button
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
              <button
                class="primary"
                onclick={() => void apply()}
                aria-disabled={!choice || applying}
                aria-busy={applying}
              >
                {applying ? 'Applying…' : choice === NONE ? 'Set aside' : choice ? `Bind to ${platformName(choice)}` : 'Bind'}
              </button>
              <button onclick={() => void closePanel()}>Cancel</button>
            </div>
          {/if}
        </div>
      {/if}
    </section>

    <section class="card" aria-labelledby="files-h">
      <h2 id="files-h">Files</h2>
      <div class="tools">
        <div class="chips" role="group" aria-label="Show files">
          {#each FILTERS as f (f.label)}
            <button class="chip" aria-pressed={filter === f.value} onclick={() => setFilter(f.value)}>{f.label}</button>
          {/each}
        </div>
        <input
          type="search"
          placeholder="Search paths"
          aria-label="Search paths"
          value={query}
          oninput={(e) => onSearch(e.currentTarget.value)}
        />
      </div>
      {#if filesError}
        <p role="alert">{filesError} <button onclick={() => void loadFiles(currentAsk())}>Retry</button></p>
      {/if}
      <p class="muted" aria-live="polite">
        {#if total === 0 && !filesLoading}
          No files to show.
        {:else}
          Files {firstShown.toLocaleString()}–{lastShown.toLocaleString()} of {total.toLocaleString()}
        {/if}
      </p>
      <table class:busy={filesLoading} aria-busy={filesLoading}>
        <thead>
          <tr>
            <th>Path</th>
            <th>Size</th>
            <th>Kind</th>
            <th>DAT entry</th>
            <th>Transfer</th>
          </tr>
        </thead>
        <tbody>
          {#each files as f (f.file_index)}
            <tr>
              <td class="path">{f.path}</td>
              <td class="num">{formatSize(f.size)}</td>
              <td>{kindText(f.kind)}</td>
              <td>
                {#if f.rom_name && f.title_id !== null}
                  <a href={titleUrl(f.title_id)}>{f.rom_name}</a>
                  {#if f.confidence}<span class="muted">({confidenceLabel(f.confidence)})</span>{/if}
                {:else if f.candidates[0]}
                  {@const c = f.candidates[0]}
                  Possibly <a href={titleUrl(c.title_id)}>{c.rom_name}</a>
                  <span class="muted">({confidenceLabel(c.confidence)})</span>
                {:else if f.unmatched}
                  <span class="muted">{unmatchedText(f.unmatched)}</span>
                {/if}
              </td>
              <td>{f.download ? downloadText(f.download) : ''}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      <div class="pager">
        <button disabled={offset === 0} onclick={() => (offset = Math.max(0, offset - PAGE))}>Previous</button>
        <button disabled={offset + PAGE >= total} onclick={() => (offset += PAGE)}>Next</button>
      </div>
    </section>
  {/if}
</div>

<style>
  .back {
    margin: 0 0 0.5em;
  }

  .back a::before {
    content: '‹ ';
  }

  h1 {
    overflow-wrap: break-word;
  }

  section {
    margin-bottom: 1em;
  }

  h2 {
    font-size: 1.05em;
    margin: 0 0 0.6em;
  }

  dl {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(12em, 1fr));
    gap: 0.6em 1.2em;
    margin: 0 0 0.6em;
  }

  dt {
    font-size: 0.8em;
    color: var(--fg-dim);
  }

  dd {
    margin: 0.1em 0 0;
  }

  .wrap,
  .path {
    overflow-wrap: break-word;
  }

  .full {
    font-family: monospace;
    font-size: 0.85em;
    width: 100%;
  }

  .hash {
    display: flex;
    align-items: center;
    gap: 0.5em;
  }

  button.small,
  .chip {
    padding: 0.25em 0.7em;
    font-size: 0.85em;
  }

  .seed {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6em;
  }

  .state {
    display: flex;
    align-items: center;
    gap: 0.6em;
  }

  .tag {
    font-size: 0.8em;
    border: 1px solid var(--accent);
    color: var(--accent);
    border-radius: 999px;
    padding: 0.05em 0.6em;
  }

  .counts {
    list-style: none;
    padding: 0;
    margin: 0.6em 0;
    display: flex;
    flex-wrap: wrap;
    gap: 0.3em 1.2em;
  }

  .actions,
  .pager {
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

  .tools {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6em;
    align-items: center;
    justify-content: space-between;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
  }

  .chip[aria-pressed='true'] {
    background: var(--accent);
    border-color: var(--accent);
    color: #08101f;
  }

  input[type='search'] {
    flex: 1 1 12em;
    min-width: 0;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85em;
    table-layout: fixed;
  }

  table.busy {
    opacity: 0.6;
  }

  th,
  td {
    text-align: left;
    padding: 0.4em;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
    overflow-wrap: break-word;
  }

  th:nth-child(1) {
    width: 34%;
  }

  th:nth-child(2) {
    width: 10%;
  }

  th:nth-child(3) {
    width: 12%;
  }

  th:nth-child(5) {
    width: 14%;
  }

  .num {
    white-space: nowrap;
  }

  @media (max-width: 600px) {
    thead {
      display: none;
    }

    table,
    tbody,
    tr,
    td {
      display: block;
    }

    tr {
      border-bottom: 1px solid var(--border);
      padding: 0.4em 0;
    }

    td {
      border: none;
      padding: 0.1em 0;
    }

    td:empty {
      display: none;
    }
  }
</style>
