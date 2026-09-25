<script lang="ts">
  import { addMagnet } from './upload';

  /** A magnet link box whose Add button shows while the link is sent, keeping focus and ignoring presses. */
  let magnet = $state('');
  let sending = $state(false);

  async function add(): Promise<void> {
    const uri = magnet.trim();
    if (!uri || sending) {
      return;
    }
    sending = true;
    try {
      if (await addMagnet(uri)) {
        magnet = '';
      }
    } finally {
      sending = false;
    }
  }
</script>

<form class="magnet" onsubmit={(e) => { e.preventDefault(); void add(); }}>
  <label>
    Or a magnet link
    <input type="text" placeholder="magnet:?xt=..." bind:value={magnet} />
  </label>
  <button type="submit" class="primary" aria-disabled={sending} aria-busy={sending}>
    {#if sending}<span class="spinner" aria-hidden="true"></span>Adding…{:else}Add{/if}
  </button>
</form>

<style>
  .magnet {
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
</style>
