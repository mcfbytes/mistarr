<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { api, ApiError, errorMessage } from '../lib/api';
  import { findSource, sources } from '../lib/stores/sources.svelte';
  import { platformName, platforms } from '../lib/stores/platforms.svelte';
  import { showToast } from '../lib/stores/toast.svelte';
  import { optimistic } from '../lib/actions';
  import { sourceStatus } from '../lib/status';
  import { bytesText } from '../lib/format';
  import { copyText } from '../lib/system';
  import { pageUrl } from '../lib/router.svelte';
  import { bindingText, shortHash, transferShare } from '../lib/sourceDetail';
  import StatusPill from '../lib/StatusPill.svelte';
  import ProgressBar from '../lib/ProgressBar.svelte';
  import ReclassifyPanel from '../lib/ReclassifyPanel.svelte';
  import SourceFiles from '../lib/SourceFiles.svelte';
  import SeedPolicySelect from '../lib/SeedPolicySelect.svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import type { SeedPolicy, SourceDetail } from '../lib/types';

  const { sourceId }: { sourceId: number } = $props();

  let detail = $state<SourceDetail | null>(null);
  let missing = $state(false);
  let loadError = $state<string | null>(null);
  let files = $state<{ reload: () => void }>();
  let fullHash = $state(false);

  const status = $derived(getStatus());
  const pausedWhilePlaying = $derived(status?.pause_client_while_playing === true);
  // How the client is held while a core runs, as the list's banner says it.
  const clientHeld = $derived(
    status?.client_hold === 'frozen'
      ? `paused while ${status.corename ?? 'a core'} is running`
      : status?.client_hold === 'uploads'
        ? `uploads paused while ${status.corename ?? 'a core'} is running`
        : null
  );
  const row = $derived(findSource(sourceId));
  // `source.changed` moves the list row; these fields moving means a re-read.
  const rowKey = $derived(
    JSON.stringify([
      row?.state,
      row?.platform_id,
      row?.matched_count,
      row?.user_binding,
      row?.pending_binding,
      row?.file_count
    ])
  );

  onMount(() => {
    // Falls back to the id in platformName(); a miss retries at the next resync.
    void platforms.ensure();
    // Hides the pause-while-playing note until connected; SSE resync fills it.
    if (!getStatus()) {
      void loadStatus().catch(() => undefined);
    }
    if (!findSource(sourceId)) {
      void sources.load();
    }
    void loadDetail();
  });

  let seenKey = untrack(() => rowKey);
  $effect(() => {
    if (rowKey !== seenKey) {
      seenKey = rowKey;
      untrack(refresh);
    }
  });

  async function loadDetail(): Promise<void> {
    try {
      detail = await api.source(sourceId);
      missing = false;
      loadError = null;
    } catch (err) {
      missing = err instanceof ApiError && err.status === 404;
      loadError = missing ? null : errorMessage(err);
    }
  }

  /** Reads the detail and the file table again after the source's binding moved. */
  function refresh(): void {
    void loadDetail();
    files?.reload();
  }

  async function copyHash(hash: string): Promise<void> {
    if (await copyText(hash)) {
      showToast('Infohash copied.', 'success');
    } else {
      fullHash = true;
      showToast('The browser did not allow copying. The whole infohash is shown to copy.');
    }
  }

  async function setSeedPolicy(policy: SeedPolicy): Promise<void> {
    const prev = detail?.seed_policy;
    const set = (value: string): void => {
      sources.patch(sourceId, { seed_policy: value });
      if (detail) {
        detail = { ...detail, seed_policy: value };
      }
    };
    await optimistic({
      apply: () => set(policy),
      revert: () => {
        if (prev !== undefined) {
          set(prev);
        }
      },
      call: () => api.updateSource(sourceId, { seed_policy: policy }),
      commit: (updated) => sources.patch(sourceId, updated)
    });
  }
</script>

<div class="page">
  <p class="back"><a href={pageUrl('sources')}>Sources</a></p>
  {#if missing}
    <h1>Source not found</h1>
    <p class="muted">No source has this id. It may have been removed.</p>
  {:else if loadError}
    <h1>Source</h1>
    <p role="alert">{loadError} <button onclick={() => void loadDetail()}>Retry</button></p>
  {:else if !detail}
    <h1>Source</h1>
    <p class="muted" aria-busy="true">Loading…</p>
  {:else}
    <h1>{detail.display_name}</h1>

    <section class="card head" aria-label="Overview">
      <dl>
        <div><dt>Size</dt><dd>{bytesText(detail.total_size)}</dd></div>
        <div><dt>Files</dt><dd>{detail.file_count.toLocaleString()}</dd></div>
        <div>
          <dt>Infohash</dt>
          <dd class="hash">
            {#if fullHash}
              <input class="full" readonly value={detail.infohash} aria-label="Infohash" onfocus={(e) => e.currentTarget.select()} />
            {:else}
              <code title={detail.infohash}>{shortHash(detail.infohash)}</code>
            {/if}
            <button class="small" onclick={() => void copyHash(detail?.infohash ?? '')} aria-label="Copy infohash">Copy</button>
          </dd>
        </div>
        <div><dt>Added</dt><dd>{new Date(detail.added_at * 1000).toLocaleDateString()}</dd></div>
        <div><dt>File</dt><dd class="wrap">{detail.origin_file}</dd></div>
        <div>
          <dt>Client</dt>
          <dd>
            {#if detail.client_id}
              In the client{#if detail.transfer.files > 0}, {detail.transfer.files} {detail.transfer.files === 1 ? 'file' : 'files'} selected{/if}{#if clientHeld}<span
                  class="held"
                  data-testid="client-held-line"
                >
                  <StatusPill status="paused" label={`Client ${clientHeld}`} /></span
                >{/if}
            {:else}
              Not in the client
            {/if}
          </dd>
        </div>
      </dl>
      {#if detail.transfer.files > 0}
        {@const share = transferShare(detail.transfer)}
        <ProgressBar
          label="Transfer of the selected files"
          view={{
            fraction: share,
            text: `${bytesText(detail.transfer.done)} of ${bytesText(detail.transfer.size)}`
          }}
        />
      {/if}
    </section>

    <section class="card" aria-labelledby="seed-h">
      <h2 id="seed-h">Seed policy</h2>
      <div class="seed">
        <SeedPolicySelect policy={detail.seed_policy} label="Seed policy" onpick={(p: SeedPolicy) => void setSeedPolicy(p)} />
        <span class="muted">How long the client keeps sharing this source's files once they are complete.</span>
      </div>
      {#if pausedWhilePlaying}<p class="muted seed-note">Paused while a core runs</p>{/if}
    </section>

    <section class="card" aria-labelledby="class-h">
      <h2 id="class-h">Classification</h2>
      <div class="state">
        <StatusPill {...sourceStatus(detail.state)} />
        {#if detail.user_binding}<span class="tag" data-testid="overridden">Set by you</span>{/if}
      </div>
      <p>{bindingText(detail, platformName)}</p>
      {#if detail.reason && !detail.user_binding}<p class="muted">{detail.reason}</p>{/if}
      {#if detail.dats.length > 0}
        <p class="muted">
          Matched against
          {#each detail.dats as d, i (d.dat_version_id)}{i > 0 ? '; ' : ' '}{d.dat_name} ({d.version}), {d.matched.toLocaleString()}
            {d.matched === 1 ? 'file' : 'files'}{/each}.
        </p>
      {/if}
      <ul class="counts" aria-label="Files by match">
        <li><strong>{detail.summary.matched.toLocaleString()}</strong> matched</li>
        <li><strong>{detail.summary.candidates.toLocaleString()}</strong> {detail.summary.candidates === 1 ? 'possible match' : 'possible matches'}</li>
        <li><strong>{detail.summary.unmatched.toLocaleString()}</strong> unmatched</li>
        <li><strong>{detail.summary.extra.toLocaleString()}</strong> {detail.summary.extra === 1 ? 'extra' : 'extras'} (text, images, checksums)</li>
        <li><strong>{detail.summary.wanted.toLocaleString()}</strong> wanted</li>
      </ul>

      <ReclassifyPanel source={detail} onchange={refresh} />
    </section>

    <SourceFiles bind:this={files} {sourceId} />
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

  .wrap {
    overflow-wrap: break-word;
  }

  .full {
    font-family: var(--mono);
    font-size: 0.85em;
    width: 100%;
  }

  .hash {
    display: flex;
    align-items: center;
    gap: 0.5em;
  }

  button.small {
    padding: 0.25em 0.7em;
    font-size: 0.85em;
  }

  .seed {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6em;
  }

  .held {
    display: block;
    margin-top: 0.3em;
  }

  .seed-note {
    margin: 0.4em 0 0;
    font-size: 0.85em;
  }

  .state {
    display: flex;
    align-items: center;
    gap: 0.6em;
  }

  .counts {
    list-style: none;
    padding: 0;
    margin: 0.6em 0;
    display: flex;
    flex-wrap: wrap;
    gap: 0.3em 1.2em;
  }
</style>
