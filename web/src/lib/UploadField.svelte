<script lang="ts">
  import { uploadFiles } from './upload';
  import type { Watched } from './stores/incoming.svelte';

  /** A file picker that uploads what is chosen into `dats/` or `sources/`, showing while it sends. */
  let { which, label, accept }: { which: Watched; label: string; accept: string } = $props();

  let input = $state<HTMLInputElement>();
  let sending = $state<string | null>(null);

  async function send(): Promise<void> {
    const names = Array.from(input?.files ?? []).map((f) => f.name);
    if (names.length === 0) {
      return;
    }
    sending = names.length === 1 ? (names[0] ?? '') : `${names.length} files`;
    try {
      await uploadFiles(which, input);
    } finally {
      sending = null;
    }
  }
</script>

<div class="field">
  <label>
    {label}
    <input bind:this={input} type="file" {accept} multiple disabled={sending !== null} aria-busy={sending !== null} onchange={send} />
  </label>
  <p class="sending" role="status">
    {#if sending !== null}<span class="spinner" aria-hidden="true"></span>Uploading {sending}…{/if}
  </p>
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.2em;
    min-width: 0;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 0.3em;
    font-size: 0.85em;
  }

  input {
    max-width: 100%;
  }

  .sending {
    display: flex;
    align-items: center;
    gap: 0.4em;
    min-height: 1.4em;
    margin: 0;
    font-size: 0.8em;
    color: var(--fg-dim);
    overflow-wrap: anywhere;
  }
</style>
