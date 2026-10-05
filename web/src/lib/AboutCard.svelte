<script lang="ts">
  import { getStatus } from './stores/status.svelte';
  import { showToast } from './stores/toast.svelte';
  import StatusPill from './StatusPill.svelte';
  import { copyText, diagnosticsText, releaseNotesUrl } from './system';

  /** The build's version and commit, release notes and a copyable diagnostics summary. */
  let manualCopy = $state<string | null>(null);

  const status = $derived(getStatus());
  const notesUrl = $derived(status ? releaseNotesUrl(status) : null);

  async function copyVersion(): Promise<void> {
    if (!status) {
      return;
    }
    const ok = await copyText(status.version);
    showToast(ok ? `Version ${status.version} copied.` : 'The browser did not allow copying.', ok ? 'success' : 'error');
  }

  async function copyDiagnostics(): Promise<void> {
    if (!status) {
      return;
    }
    const text = diagnosticsText(status);
    if (await copyText(text)) {
      manualCopy = null;
      showToast('Diagnostics copied.', 'success');
    } else {
      manualCopy = text;
    }
  }
</script>

{#if status}
  <section class="card about" aria-labelledby="about-h">
    <h2 id="about-h">About</h2>
    <dl>
      <div>
        <dt>Version</dt>
        <dd>
          <code class="version">{status.version}</code>
          <button class="small" aria-label="Copy version" onclick={copyVersion}>Copy</button>
          {#if status.release}
            <StatusPill status="done" label="Release" />
          {:else}
            <StatusPill status="queued" label="Development build" />
          {/if}
        </dd>
      </div>
      <div>
        <dt>Commit</dt>
        <dd><code>{status.commit ?? 'unknown'}</code></dd>
      </div>
      {#if notesUrl}
        <div>
          <dt>Release notes</dt>
          <dd><a href={notesUrl} target="_blank" rel="noopener noreferrer">v{status.version} on GitHub</a></dd>
        </div>
      {/if}
    </dl>
    <div class="diag">
      <button onclick={copyDiagnostics} aria-describedby="diag-help">Copy diagnostics</button>
      <p id="diag-help" class="help">
        Plain text for a bug report: versions, states and sizes, like <code>mistarr doctor</code>, and your
        browser. It holds no paths, addresses or file names.
      </p>
    </div>
    {#if manualCopy}
      <label class="manual">
        The browser did not allow copying. Select this text and copy it.
        <textarea readonly rows="8">{manualCopy}</textarea>
      </label>
    {/if}
  </section>
{/if}

<style>
  h2 {
    font-size: 1.15rem;
    margin: 0 0 0.6rem;
  }

  code {
    font-family: var(--mono);
    font-size: 0.9em;
  }

  button.small {
    padding: 0.25em 0.7em;
    font-size: 0.85rem;
  }

  .about {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem 2rem;
    align-items: flex-start;
    margin-bottom: 1.5rem;
  }

  .about h2 {
    flex-basis: 100%;
  }

  dl {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 0.45rem 1rem;
    margin: 0;
    flex: 1 1 20rem;
    min-width: 0;
  }

  dl > div {
    display: contents;
  }

  dt {
    color: var(--fg-dim);
    font-size: 0.9rem;
    padding-top: 0.15em;
  }

  dd {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem 0.6rem;
    min-width: 0;
  }

  .version {
    font-size: 1rem;
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .diag {
    flex: 1 1 16rem;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.4rem;
  }

  .manual {
    flex-basis: 100%;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.9rem;
  }

  .manual textarea {
    font: 0.8rem var(--mono);
    width: 100%;
    background: var(--bg);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.5em;
  }
</style>
