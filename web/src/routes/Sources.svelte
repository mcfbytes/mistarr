<script lang="ts">
  import { onMount } from 'svelte';
  import { sources } from '../lib/stores/sources.svelte';
  import { platformName, platforms } from '../lib/stores/platforms.svelte';
  import { attempt, optimistic } from '../lib/actions';
  import { api } from '../lib/api';
  import { sourceStatus } from '../lib/status';
  import IncomingList from '../lib/IncomingList.svelte';
  import UploadField from '../lib/UploadField.svelte';
  import UrlField from '../lib/UrlField.svelte';
  import StatusPill from '../lib/StatusPill.svelte';
  import { sourceUrl } from '../lib/router.svelte';
  import { bindingText } from '../lib/sourceDetail';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import ClientHeld from '../lib/ClientHeld.svelte';
  import SeedPolicySelect from '../lib/SeedPolicySelect.svelte';
  import { getStatus } from '../lib/stores/status.svelte';
  import type { SeedPolicy } from '../lib/types';

  onMount(() => {
    void sources.load();
    // The bind dropdown just shows no platforms on a miss; it retries at the next resync.
    void platforms.ensure();
  });

  const pausedWhilePlaying = $derived(getStatus()?.pause_client_while_playing === true);
  let removingId = $state<number | null>(null);

  async function bind(id: number, platformId: string): Promise<void> {
    if (!platformId) {
      return;
    }
    const prev = sources.items.find((s) => s.id === id);
    const pending = { automatic: false, platform_id: platformId };
    await optimistic({
      apply: () => sources.patch(id, { user_binding: true, pending_binding: pending }),
      revert: () => {
        if (prev) {
          sources.patch(id, { user_binding: prev.user_binding, pending_binding: prev.pending_binding });
        }
      },
      // The binding runs as a job; `source.changed` brings the bound row once it ends.
      call: () => api.updateSource(id, { platform_id: platformId }),
      commit: (row) => sources.patch(id, { user_binding: row.user_binding, pending_binding: row.pending_binding })
    });
  }

  async function setSeedPolicy(id: number, policy: SeedPolicy): Promise<void> {
    const prev = sources.items.find((s) => s.id === id);
    await optimistic({
      apply: () => sources.patch(id, { seed_policy: policy }),
      revert: () => {
        if (prev) {
          sources.patch(id, { seed_policy: prev.seed_policy });
        }
      },
      call: () => api.updateSource(id, { seed_policy: policy }),
      commit: (row) => sources.patch(id, row)
    });
  }

  async function disable(id: number): Promise<void> {
    const prev = sources.items.find((s) => s.id === id);
    await optimistic({
      apply: () => sources.patch(id, { state: 'disabled' }),
      revert: () => {
        if (prev) {
          sources.patch(id, { state: prev.state });
        }
      },
      call: () => api.updateSource(id, { state: 'disabled' }),
      commit: (row) => sources.patch(id, row)
    });
  }

  async function remove(id: number): Promise<boolean> {
    removingId = id;
    const done = await attempt(async () => {
      await api.deleteSource(id);
      sources.patch(id, null);
      void sources.load();
      return true;
    });
    removingId = null;
    if (done) {
      // The removed row is gone from the table; land focus on the heading instead of the body.
      document.getElementById('sources-heading')?.focus();
    }
    return done === true;
  }
</script>

<div class="page">
  <h1 id="sources-heading" tabindex="-1">Sources</h1>
  <ClientHeld />

  <div class="card upload">
    <UploadField which="sources" label="Add a .torrent file" accept=".torrent" />
    <UrlField label="Or a magnet link" placeholder="magnet:?xt=..." action="Add" pending="Adding…" note="" />
    <UrlField />
  </div>

  <h2>Waiting in <code>sources/</code></h2>
  <IncomingList which="sources" />

  {#if sources.error}
    <p role="alert">{sources.error} <button type="button" onclick={() => void sources.load()}>Retry</button></p>
  {:else if sources.items.length === 0}
    <p>No sources yet. Place a .torrent or .magnet file in <code>/media/fat/mistarr/sources</code> or drop one here.</p>
  {/if}
  {#if sources.items.length > 0}
    <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th>Platform</th>
          <th>State</th>
          <th>Files</th>
          <th>Matched</th>
          <th>Seed policy</th>
          <th>Client</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each sources.items as source (source.id)}
          <tr>
            <td><a href={sourceUrl(source.id)} class="name">{source.display_name}</a></td>
            <td>
              {#if source.pending_binding}
                <span class="muted">{bindingText(source, platformName)}</span>
              {:else if source.platform_id}
                {source.platform_id}
                {#if source.user_binding}<span class="tag">Set by you</span>{/if}
              {:else}
                {#if source.user_binding}<span class="tag">Set by you</span>{/if}
                <select
                  disabled={source.file_count === 0}
                  onchange={(e) => bind(source.id, e.currentTarget.value)}
                >
                  <option value="">Choose platform</option>
                  {#each platforms.items as p (p.id)}
                    <option value={p.id}>{p.name}</option>
                  {/each}
                </select>
                {#if source.suggested_platform_id}
                  <button class="suggest" onclick={() => source.suggested_platform_id && bind(source.id, source.suggested_platform_id)}>
                    Bind to {platformName(source.suggested_platform_id)}
                  </button>
                {/if}
              {/if}
            </td>
            <td>
              <StatusPill {...sourceStatus(source.state)} />
              {#if source.reason}<span class="muted reason">{source.reason}</span>{/if}
            </td>
            <td>{source.file_count}</td>
            <td>{source.matched_count}</td>
            <td>
              <SeedPolicySelect
                policy={source.seed_policy}
                label={`Seed policy of ${source.display_name}`}
                onpick={(p: SeedPolicy) => setSeedPolicy(source.id, p)}
              />
              {#if pausedWhilePlaying}<span class="muted seed-note">Paused while a core runs</span>{/if}
            </td>
            <td>{source.client_id ? 'in client' : '—'}</td>
            <td>
              <div class="row-actions">
                {#if source.state !== 'disabled'}
                  <button onclick={() => disable(source.id)}>Disable</button>
                {/if}
                <ConfirmButton
                  name={`Remove ${source.display_name}`}
                  confirmLabel="Remove source"
                  keepName={`Keep ${source.display_name}`}
                  groupName={`Remove ${source.display_name}?`}
                  prompt="Removed from mistarr and the client; placed files stay."
                  busy={removingId === source.id}
                  onconfirm={() => remove(source.id)}
                />
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    </div>
  {/if}
</div>

<style>
  .upload {
    display: grid;
    gap: 0.6em;
    margin-bottom: 1em;
  }

  .reason,
  .seed-note {
    display: block;
    margin-top: 0.2em;
  }

  .seed-note {
    font-size: 0.85em;
  }

  .suggest {
    display: block;
    margin-top: 0.3em;
    font-size: 0.9em;
  }

  .name {
    overflow-wrap: break-word;
  }

  .row-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em;
  }
</style>
