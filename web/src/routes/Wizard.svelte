<script lang="ts">
  import { onMount } from 'svelte';
  import { navigate } from '../lib/router.svelte';
  import { fixtureDats } from '../lib/fixtures';
  import { getPlatforms, loadPlatforms } from '../lib/stores/platforms.svelte';

  const isMock = import.meta.env.VITE_MOCK === '1';

  let step = $state(0);
  const steps = ['Paths', 'DATs', 'Client', 'Sources'];

  onMount(() => {
    void loadPlatforms();
  });

  const platforms = $derived(getPlatforms());
  const detectedCores = $derived(platforms.filter((p) => p.core_present));

  function next(): void {
    if (step < steps.length - 1) {
      step += 1;
    } else {
      navigate('/');
    }
  }

  function skip(): void {
    next();
  }

  function finish(): void {
    navigate('/');
  }
</script>

<div class="page">
  <h1>First run</h1>
  <ol class="steps">
    {#each steps as label, i (label)}
      <li class:current={i === step} class:done={i < step}>{label}</li>
    {/each}
  </ol>

  {#if step === 0}
    <section class="card">
      <h2>Paths</h2>
      <p>Root directory: <code>/media/fat</code></p>
      <p>Games directory: <code>/media/fat/games</code></p>
      <p class="muted">Detected cores:</p>
      <ul>
        {#each detectedCores as p (p.id)}
          <li>{p.name}</li>
        {:else}
          <li class="muted">None detected yet.</li>
        {/each}
      </ul>
    </section>
  {:else if step === 1}
    <section class="card">
      <h2>DATs</h2>
      <p>Drop Logiqx DAT files or zipped DAT packs here, or place them in:</p>
      <p><code>/media/fat/mistarr/dats</code></p>
      <div class="dropzone">Drop files here</div>
      <p class="muted">Loaded DATs:</p>
      <ul>
        {#each (isMock ? fixtureDats : []) as dat (dat.id)}
          <li>{dat.dat_name} — {dat.platform_id ?? 'unbound'}</li>
        {:else}
          <li class="muted">None loaded yet.</li>
        {/each}
      </ul>
    </section>
  {:else if step === 2}
    <section class="card">
      <h2>Client</h2>
      <p>Detection result: <strong>rtorrent not found</strong></p>
      <button class="primary">Start rtorrent</button>
      <h3>Remote path map</h3>
      <label>
        Remote path
        <input type="text" placeholder="/downloads" />
      </label>
      <label>
        Local path
        <input type="text" placeholder="/media/fat/mistarr/staging" />
      </label>
    </section>
  {:else}
    <section class="card">
      <h2>Sources</h2>
      <p>Drop <code>.torrent</code> or <code>.magnet</code> files here, or place them in:</p>
      <p><code>/media/fat/mistarr/sources</code></p>
      <div class="dropzone">Drop files here</div>
      <h3>Seed policy</h3>
      <p>Each source keeps its own seeding setting. Default: <strong>none</strong>.</p>
      <ul>
        <li><strong>none</strong> — no seeding after a transfer completes.</li>
        <li><strong>until ratio</strong> — seed until a chosen ratio, then stop.</li>
        <li><strong>client default</strong> — leave it to the client's own setting.</li>
      </ul>
    </section>
  {/if}

  <div class="actions">
    <button onclick={skip}>Skip</button>
    {#if step === steps.length - 1}
      <button class="primary" onclick={finish}>Finish</button>
    {:else}
      <button class="primary" onclick={next}>Next</button>
    {/if}
  </div>
</div>

<style>
  .steps {
    display: flex;
    gap: 0.5em;
    list-style: none;
    padding: 0;
    flex-wrap: wrap;
  }

  .steps li {
    padding: 0.3em 0.7em;
    border-radius: 999px;
    background: var(--bg-raised);
    color: var(--fg-dim);
    border: 1px solid var(--border);
  }

  .steps li.current {
    color: var(--fg);
    border-color: var(--accent);
  }

  .steps li.done {
    color: var(--ok);
  }

  .dropzone {
    border: 1px dashed var(--border);
    border-radius: var(--radius);
    padding: 2em;
    text-align: center;
    color: var(--fg-dim);
    margin: 0.6em 0;
  }

  label {
    display: block;
    margin: 0.5em 0;
  }

  .actions {
    display: flex;
    justify-content: space-between;
    margin-top: 1em;
  }
</style>
