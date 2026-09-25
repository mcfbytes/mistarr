<script lang="ts">
  import { addFromUrl } from './fetch';

  /**
   * A link box that fetches one DAT, DAT pack or torrent, or places a magnet link. It keeps
   * no history: the browser is asked not to remember or suggest, and the box empties once sent.
   */
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
    Add from a URL
    <input
      type="text"
      inputmode="url"
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
      placeholder="https://example.invalid/…"
      bind:value={link}
    />
  </label>
  <button type="submit" class="primary" aria-disabled={sending} aria-busy={sending}>
    {#if sending}<span class="spinner" aria-hidden="true"></span>Sending…{:else}Fetch{/if}
  </button>
  <p class="note">A DAT, a zipped DAT pack, a .torrent file or a magnet link. It is fetched once and not remembered.</p>
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
