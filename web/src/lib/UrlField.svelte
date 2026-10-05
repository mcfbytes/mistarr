<script lang="ts">
  import { addFromUrl } from './fetch';

  /**
   * A link box that fetches one DAT, DAT pack or torrent, or places a magnet link. It keeps
   * no history: the browser is asked not to remember or suggest, and the box empties once sent.
   * The props reword it for a magnet-only box; an empty `note` leaves the note out.
   */
  let {
    label = 'Add from a URL',
    placeholder = 'https://example.invalid/…',
    action = 'Fetch',
    pending = 'Sending…',
    note = 'A DAT, a zipped DAT pack, a .torrent file or a magnet link. It is fetched once and not remembered.'
  }: { label?: string; placeholder?: string; action?: string; pending?: string; note?: string } = $props();

  let link = $state('');
  let sending = $state(false);

  async function go(): Promise<void> {
    const text = link.trim();
    if (!text || sending) {
      return;
    }
    sending = true;
    try {
      if (await addFromUrl(text)) {
        link = '';
      }
    } finally {
      sending = false;
    }
  }
</script>

<form class="url" autocomplete="off" onsubmit={(e) => { e.preventDefault(); void go(); }}>
  <label>
    {label}
    <input
      type="text"
      inputmode="url"
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
      {placeholder}
      bind:value={link}
    />
  </label>
  <button type="submit" class="primary" aria-disabled={sending} aria-busy={sending}>
    {#if sending}<span class="spinner" aria-hidden="true"></span>{pending}{:else}{action}{/if}
  </button>
  {#if note}<p class="note">{note}</p>{/if}
</form>

<style>
  .url {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6em;
    align-items: end;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 0.2em;
    font-size: 0.85em;
    flex: 1 1 14em;
    min-width: 0;
  }

  button {
    display: inline-flex;
    align-items: center;
    gap: 0.4em;
    min-width: 6.5em;
    justify-content: center;
  }

  .note {
    flex-basis: 100%;
    margin: 0;
    font-size: 0.8em;
    color: var(--fg-dim);
  }
</style>
