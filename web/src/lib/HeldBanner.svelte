<script lang="ts">
  import { api, errorMessage } from './api';
  import { applyStatus, getStatus } from './stores/status.svelte';
  import { showToast } from './stores/toast.svelte';

  const isMock = import.meta.env.VITE_MOCK === '1';

  const status = $derived(getStatus());
  const waiting = $derived(status?.paused ? status.waiting : []);
  const why = $derived(
    status?.pause_reason === 'core' ? `while ${status.corename ?? 'a core'} is running` : 'by the user'
  );
  let busy = $state(false);

  async function runNow(): Promise<void> {
    if (isMock) {
      return;
    }
    busy = true;
    try {
      applyStatus(await api.resume());
    } catch (err) {
      showToast(errorMessage(err));
    } finally {
      busy = false;
    }
  }
</script>

{#if waiting.length > 0}
  <div class="held" role="status">
    <span>
      {waiting.length === 1 ? '1 job is' : `${waiting.length} jobs are`} paused {why}:
      {waiting.map((w) => (w.detail ? `${w.kind} (${w.detail})` : w.kind)).join(', ')}.
    </span>
    <button onclick={runNow} disabled={busy}>Run now</button>
  </div>
{/if}

<style>
  .held {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4em 0.8em;
    align-items: center;
    justify-content: center;
    padding: 0.4em var(--gutter);
    border-bottom: 1px solid var(--border);
    background: var(--bg-raised);
    font-size: 0.9em;
    overflow-wrap: anywhere;
  }

  .held button {
    padding: 0.2em 0.7em;
  }
</style>
