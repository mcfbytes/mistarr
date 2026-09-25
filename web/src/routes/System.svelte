<script lang="ts">
  import { onMount } from 'svelte';
  import { getStatus, loadStatus } from '../lib/stores/status.svelte';
  import { showToast } from '../lib/stores/toast.svelte';
  import { fixtureSettings, recordMockSave } from '../lib/fixtures';
  import { api, errorMessage } from '../lib/api';
  import ClientStart from '../lib/ClientStart.svelte';
  import ClientHeld from '../lib/ClientHeld.svelte';
  import PathMapEditor from '../lib/PathMapEditor.svelte';
  import StatusPill from '../lib/StatusPill.svelte';
  import Meter from '../lib/Meter.svelte';
  import { cleanPathMap } from '../lib/pathmap';
  import { setLeaveGuard } from '../lib/router.svelte';
  import { speedText } from '../lib/unidentified';
  import {
    bytesText,
    copyText,
    diagnosticsText,
    releaseNotesUrl,
    uptimeText,
    usedFraction
  } from '../lib/system';
  import type { LimitsSettings, Settings } from '../lib/types';

  type LimitKey = keyof LimitsSettings;

  const isMock = import.meta.env.VITE_MOCK === '1';

  /** The settings sections, in page order, for the section list. */
  const SECTIONS = [
    { id: 'set-client', label: 'Download client' },
    { id: 'set-limits', label: 'Transfers and limits' },
    { id: 'set-titles', label: 'Title choice' },
    { id: 'set-launch', label: 'Launching' },
    { id: 'set-scan', label: 'Scanning' }
  ] as const;

  const LIMIT_ROWS: { id: string; label: string; fields: { key: LimitKey; label: string }[] }[] = [
    {
      id: 'limits-menu',
      label: 'At the menu',
      fields: [
        { key: 'down_kbps_menu', label: 'Download' },
        { key: 'up_kbps_menu', label: 'Upload' }
      ]
    },
    {
      id: 'limits-core',
      label: 'While a core runs',
      fields: [
        { key: 'down_kbps_core', label: 'Download' },
        { key: 'up_kbps_core', label: 'Upload' }
      ]
    }
  ];
  const LIMIT_ERROR = 'Each speed limit needs a whole number of kB/s, 0 or more.';
  const LAUNCH_TEXT = { ready: 'Ready', disabled: 'Off in settings', unavailable: 'Unavailable here' };

  let settings = $state<Settings | null>(null);
  let baseline = $state('');
  let saving = $state(false);
  let settingsError = $state<string | null>(null);
  let statusError = $state<string | null>(null);
  let manualCopy = $state<string | null>(null);
  let current = $state<string>(SECTIONS[0].id);
  let jumpedAt = 0;
  let invalidLimits = $state<LimitKey[]>([]);
  let limitsKey = $state(0);
  let barHeight = $state(0);

  const status = $derived(getStatus());
  const dirty = $derived(settings !== null && JSON.stringify(settings) !== baseline);
  const showBar = $derived(dirty || invalidLimits.length > 0 || settingsError !== null);
  const notesUrl = $derived(status ? releaseNotesUrl(status) : null);
  const memUsed = $derived(status ? usedFraction(status.mem_available_bytes, status.mem_total_bytes) : null);
  const diskUsed = $derived(status ? usedFraction(status.disk_free_bytes, status.disk_total_bytes) : null);
  const since = $derived(
    status
      ? new Date(Math.floor((Date.now() - status.uptime_secs * 1000) / 60_000) * 60_000).toLocaleString(undefined, {
          dateStyle: 'medium',
          timeStyle: 'short'
        })
      : ''
  );

  onMount(() => {
    void loadStatus();
    void loadSettings();
  });

  function adopt(next: Settings): void {
    baseline = JSON.stringify(next);
    settings = JSON.parse(baseline) as Settings;
  }

  async function loadSettings(): Promise<void> {
    adopt(isMock ? fixtureSettings : await api.settings());
  }

  // Unsaved settings survive neither a reload nor leaving the screen without a question.
  $effect(() => {
    if (!dirty && invalidLimits.length === 0) {
      return;
    }
    const onUnload = (e: BeforeUnloadEvent): void => {
      e.preventDefault();
    };
    window.addEventListener('beforeunload', onUnload);
    setLeaveGuard(() => window.confirm('Leave without saving the changed settings?'));
    return () => {
      window.removeEventListener('beforeunload', onUnload);
      setLeaveGuard(null);
    };
  });

  // Lifts the toasts above the save bar while it shows.
  $effect(() => {
    if (!showBar || barHeight === 0) {
      return;
    }
    const root = document.documentElement;
    root.style.setProperty('--toast-lift', `${barHeight + 12}px`);
    return () => {
      root.style.removeProperty('--toast-lift');
    };
  });

  // Marks the section being read in the section list.
  $effect(() => {
    if (!settings || typeof IntersectionObserver === 'undefined') {
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        const top = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0];
        if (top && Date.now() - jumpedAt > 1000) {
          current = top.target.id;
        }
      },
      { rootMargin: '0px 0px -65% 0px' }
    );
    for (const s of SECTIONS) {
      const el = document.getElementById(s.id);
      if (el) {
        observer.observe(el);
      }
    }
    return () => {
      observer.disconnect();
    };
  });

  function jump(id: string): void {
    const heading = document.getElementById(`${id}-h`);
    if (!heading) {
      return;
    }
    const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    heading.scrollIntoView({ block: 'start', behavior: reduce ? 'auto' : 'smooth' });
    heading.focus({ preventScroll: true });
    current = id;
    jumpedAt = Date.now();
  }

  async function togglePause(): Promise<void> {
    statusError = null;
    if (isMock) {
      return;
    }
    try {
      if (status?.paused) {
        await api.resume();
      } else {
        await api.pause();
      }
      await loadStatus();
    } catch (err) {
      statusError = errorMessage(err);
    }
  }

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

  function csv(xs: string[]): string {
    return xs.join(', ');
  }

  function fromCsv(text: string): string[] {
    return text
      .split(',')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  }

  /** Takes a limit only when it is a whole number of kB/s; otherwise marks the field. */
  function setLimit(key: LimitKey, input: HTMLInputElement): void {
    const text = input.value.trim();
    const n = Number(text);
    const ok = text !== '' && Number.isInteger(n) && n >= 0 && n <= 4_294_967_295;
    invalidLimits = ok ? invalidLimits.filter((k) => k !== key) : [...new Set([...invalidLimits, key])];
    if (ok && settings) {
      settings.limits[key] = n;
    }
    if (invalidLimits.length === 0 && settingsError === LIMIT_ERROR) {
      settingsError = null;
    }
  }

  function discard(): void {
    settingsError = null;
    invalidLimits = [];
    limitsKey += 1;
    settings = JSON.parse(baseline) as Settings;
  }

  async function save(): Promise<void> {
    if (!settings) {
      return;
    }
    settingsError = null;
    if (invalidLimits.length > 0) {
      settingsError = LIMIT_ERROR;
      return;
    }
    const cleaned = cleanPathMap(settings.client.remote_path_map);
    if ('error' in cleaned) {
      settingsError = cleaned.error;
      return;
    }
    const next = { ...settings, client: { ...settings.client, remote_path_map: cleaned.map } };
    saving = true;
    try {
      if (isMock) {
        recordMockSave(next);
      }
      adopt(isMock ? next : await api.putSettings(next));
      showToast('Settings saved.', 'success');
    } catch (err) {
      settingsError = errorMessage(err);
      return;
    } finally {
      saving = false;
    }
    if (!isMock) {
      // A failed refresh leaves the tiles as they were; SSE brings the next status.
      await loadStatus().catch(() => undefined);
    }
  }
</script>

<div class="page system">
  <h1>System</h1>

  {#if status}
    <section aria-labelledby="status-h">
      <h2 id="status-h" class="visually-hidden">Status</h2>
      <ul class="tiles" aria-label="Status">
        <li class="tile">
          <h3>MiSTer</h3>
          <p class="big">
            {#if status.corename === null}Not detected{:else if status.corename === 'MENU'}At the menu{:else}{status.corename}{/if}
          </p>
          <p class="sub">
            {#if status.corename !== null && status.corename !== 'MENU'}Core running.{/if}
            Launching: {LAUNCH_TEXT[status.launch]}
          </p>
          <ClientHeld pillOnly />
        </li>

        <li class="tile">
          <div class="tile-head">
            <h3>Download client</h3>
            {#if !status.client?.kind}
              <StatusPill status="queued" label="Not detected" />
            {:else if status.client.reachable}
              <StatusPill status="done" label="Reachable" />
            {:else}
              <StatusPill status="failed" label="Not reachable" />
            {/if}
          </div>
          <p class="big">
            {status.client?.kind ?? 'None'}
            {#if status.client?.version}<span class="dim">{status.client.version}</span>{/if}
          </p>
          {#if status.client?.url}<p class="sub mono">{status.client.url}</p>{/if}
          <ClientHeld />
          <ClientStart />
        </li>

        <li class="tile">
          <div class="tile-head">
            <h3>Scheduler</h3>
            {#if !status.paused}
              <StatusPill status="running" label="Running" />
            {:else if status.pause_reason === 'core'}
              <StatusPill status="paused" label="Held for the core" />
            {:else}
              <StatusPill status="paused" label="Paused" />
            {/if}
          </div>
          <p class="big">
            {status.waiting.length === 0
              ? 'Nothing waiting'
              : `${status.waiting.length} ${status.waiting.length === 1 ? 'job' : 'jobs'} waiting`}
          </p>
          <div class="tile-action">
            <button onclick={togglePause}>
              {status.paused ? (status.pause_reason === 'core' ? 'Run now' : 'Resume') : 'Pause'}
            </button>
          </div>
          {#if statusError}<p class="error sub">{statusError}</p>{/if}
        </li>

        <li class="tile">
          <h3>Memory</h3>
          <p class="big">{bytesText(status.rss_bytes)} <span class="dim">used by mistarr</span></p>
          {#if memUsed !== null}
            <Meter
              fraction={memUsed}
              label="Board memory in use"
              text={`${bytesText(status.mem_available_bytes)} available of ${bytesText(status.mem_total_bytes)}`}
            />
            <p class="sub">{bytesText(status.mem_available_bytes)} available of {bytesText(status.mem_total_bytes)}</p>
          {/if}
        </li>

        <li class="tile">
          <h3>Storage</h3>
          <p class="big">{bytesText(status.disk_free_bytes)} <span class="dim">free</span></p>
          {#if diskUsed !== null}
            <Meter
              fraction={diskUsed}
              warnAt={0.9}
              label="Storage in use"
              text={`${bytesText(status.disk_free_bytes)} free of ${bytesText(status.disk_total_bytes)}`}
            />
            <p class="sub">of {bytesText(status.disk_total_bytes)}, where the data directory is</p>
          {/if}
        </li>

        <li class="tile">
          <h3>Uptime</h3>
          <p class="big">{uptimeText(status.uptime_secs)}</p>
          <p class="sub">Since {since}</p>
        </li>
      </ul>
    </section>

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

  {#if settings}
    <section class="settings" aria-labelledby="settings-h">
      <h2 id="settings-h">Settings</h2>
      <p class="help intro">
        These apply as soon as they are saved, with no restart. Settings found only in <code>mistarr.toml</code>
        apply at the next start.
      </p>
      <div class="layout">
        <nav class="sections" aria-label="Settings sections">
          <ul>
            {#each SECTIONS as s (s.id)}
              <li>
                <button type="button" aria-current={current === s.id ? 'true' : undefined} onclick={() => jump(s.id)}>
                  {s.label}
                </button>
              </li>
            {/each}
          </ul>
        </nav>

        <form onsubmit={(e) => { e.preventDefault(); void save(); }}>
          <section id="set-client" class="card group" aria-labelledby="set-client-h">
            <h3 id="set-client-h" tabindex="-1">Download client</h3>
            <p class="help lead">Saving a change here looks for the client again.</p>
            <div class="field">
              <label for="client-kind">Client</label>
              <select id="client-kind" bind:value={settings.client.kind}>
                <option value="auto">Auto-detect</option>
                <option value="transmission">Transmission</option>
                <option value="rtorrent">rtorrent</option>
              </select>
            </div>
            <div class="field">
              <label for="client-url">Address</label>
              <input
                id="client-url"
                class="wide"
                type="text"
                placeholder="http://127.0.0.1:9091/transmission/rpc"
                aria-describedby="client-url-help"
                bind:value={settings.client.url}
              />
              <p id="client-url-help" class="help">Empty tries the usual local addresses.</p>
            </div>
            <div class="field">
              <span class="label" id="path-map-label">Path map</span>
              <div role="group" aria-labelledby="path-map-label" aria-describedby="path-map-help">
                <PathMapEditor bind:map={settings.client.remote_path_map} />
              </div>
              <p id="path-map-help" class="help">
                For a client that sees the files under other paths, such as one on another machine.
              </p>
            </div>
          </section>

          <section id="set-limits" class="card group" aria-labelledby="set-limits-h">
            <h3 id="set-limits-h" tabindex="-1">Transfers and limits</h3>
            <p class="help lead">Speed limits the client applies, in kB/s.</p>
            <p class="help lead">0 keeps the client's own limit. Any other value only ever lowers it.</p>
            {#key limitsKey}
              {#each LIMIT_ROWS as row (row.id)}
                <div class="field">
                  <span class="label" id={row.id}>{row.label}</span>
                  <div class="pair" role="group" aria-labelledby={row.id}>
                    {#each row.fields as f (f.key)}
                      <label>
                        <span>{f.label}</span>
                        <span class="unit">
                          <input
                            type="number"
                            min="0"
                            step="1"
                            inputmode="numeric"
                            value={settings.limits[f.key]}
                            aria-invalid={invalidLimits.includes(f.key) ? 'true' : undefined}
                            aria-describedby={invalidLimits.includes(f.key) ? 'limits-error' : undefined}
                            oninput={(e) => {
                              setLimit(f.key, e.currentTarget);
                            }}
                          />
                          <span aria-hidden="true">kB/s</span>
                        </span>
                      </label>
                    {/each}
                  </div>
                </div>
              {/each}
            {/key}
            {#if invalidLimits.length > 0}
              <p id="limits-error" class="error field-error">{LIMIT_ERROR}</p>
            {/if}
            <div class="field check">
              <label>
                <input
                  type="checkbox"
                  bind:checked={settings.transfer.pause_client_while_playing}
                  aria-describedby="client-pause-help"
                />
                Pause the download client while a core runs
              </label>
              <p id="client-pause-help" class="help">
                Frees the board for the game. Transfers resume at the menu, and each source's seed policy applies
                again. A client on another machine only stops uploading.
              </p>
            </div>
          </section>

          <section id="set-titles" class="card group" aria-labelledby="set-titles-h">
            <h3 id="set-titles-h" tabindex="-1">Title choice</h3>
            <p class="help lead">
              How one entry is picked for each title (1G1R). Saving a change here picks again.
            </p>
            <div class="field">
              <label for="pref-regions">Region order</label>
              <input
                id="pref-regions"
                class="wide"
                type="text"
                aria-describedby="regions-help"
                value={csv(settings.prefs.regions)}
                oninput={(e) => settings && (settings.prefs.regions = fromCsv(e.currentTarget.value))}
              />
              <p id="regions-help" class="help">Comma-separated, first preferred.</p>
            </div>
            <div class="field">
              <label for="pref-languages">Language order</label>
              <input
                id="pref-languages"
                class="wide"
                type="text"
                aria-describedby="languages-help"
                value={csv(settings.prefs.languages)}
                oninput={(e) => settings && (settings.prefs.languages = fromCsv(e.currentTarget.value))}
              />
              <p id="languages-help" class="help">Comma-separated, first preferred.</p>
            </div>
            <div class="field">
              <label for="pref-hide">Hidden flags</label>
              <input
                id="pref-hide"
                class="wide"
                type="text"
                aria-describedby="hide-help"
                value={csv(settings.prefs.hide)}
                oninput={(e) => settings && (settings.prefs.hide = fromCsv(e.currentTarget.value))}
              />
              <p id="hide-help" class="help">Comma-separated DAT flags whose entries are left out.</p>
            </div>
            <div class="field check">
              <label>
                <input type="checkbox" bind:checked={settings.prefs.prefer_latest_revision} />
                Prefer the highest revision
              </label>
            </div>
          </section>

          <section id="set-launch" class="card group" aria-labelledby="set-launch-h">
            <h3 id="set-launch-h" tabindex="-1">Launching</h3>
            <div class="field check">
              <label>
                <input type="checkbox" bind:checked={settings.prefs.launch} aria-describedby="launch-help" />
                Allow starting cores and games from mistarr
              </label>
              <p id="launch-help" class="help">
                Shows Start buttons, which ask MiSTer Main to load a core or a file.
              </p>
            </div>
          </section>

          <section id="set-scan" class="card group" aria-labelledby="set-scan-h">
            <h3 id="set-scan-h" tabindex="-1">Scanning</h3>
            <p class="help lead">Disc images</p>
            <div class="field check">
              <label>
                <input type="checkbox" bind:checked={settings.scan.chd_tracks} aria-describedby="chd-help" />
                Identify CHD images by their tracks <span class="tag">Slow</span>
              </label>
              <p id="chd-help" class="help">
                Decodes each CHD image once to hash its tracks, so it can be matched against a DAT. On the DE10-Nano
                this takes minutes per disc; it pauses while a core runs, and results are kept.
              </p>
              <p class="help">{speedText(status?.chd_decode_bytes_per_sec)}</p>
            </div>
          </section>

          {#if showBar}
            <div class="savebar" role="region" aria-label="Unsaved changes" bind:clientHeight={barHeight}>
              {#if settingsError}
                <p class="error" role="alert">{settingsError}</p>
              {:else}
                <p>Unsaved changes</p>
              {/if}
              <div class="actions">
                <button type="button" onclick={discard} disabled={saving}>Discard</button>
                <button type="submit" class="primary" disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
              </div>
            </div>
          {/if}
        </form>
      </div>
    </section>
  {/if}
</div>

<style>
  h1 {
    margin: 0.2em 0 0.6em;
  }

  h2 {
    font-size: 1.15rem;
    margin: 0 0 0.6rem;
  }

  code {
    font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
    font-size: 0.9em;
  }

  .tiles {
    list-style: none;
    margin: 0 0 1rem;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.6rem;
  }

  @media (min-width: 720px) {
    .tiles {
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 0.75rem;
    }
  }

  .tile {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    min-width: 0;
    padding: 0.75rem 0.85rem;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .tile-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.3rem 0.5rem;
  }

  .tile h3 {
    margin: 0;
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--fg-dim);
  }

  .big {
    margin: 0;
    font-size: 1.2rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }

  .dim {
    font-size: 0.8rem;
    font-weight: 400;
    color: var(--fg-dim);
  }

  .sub {
    margin: 0;
    font-size: 0.82rem;
    color: var(--fg-dim);
    overflow-wrap: anywhere;
  }

  .mono {
    font-family: ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
    font-size: 0.78rem;
  }

  .tile-action {
    margin-top: auto;
  }

  .tile-action button,
  button.small {
    padding: 0.25em 0.7em;
    font-size: 0.85rem;
  }

  .tile :global(.client-held) {
    margin: 0;
  }

  .tile :global(.client-held .pill) {
    white-space: normal;
  }

  .tile :global(.client-start) {
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
    font: 0.8rem ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
    width: 100%;
    background: var(--bg);
    color: var(--fg);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.5em;
  }

  .help {
    margin: 0;
    font-size: 0.85rem;
    color: var(--fg-dim);
    max-width: 42em;
  }

  .intro {
    margin-bottom: 0.9rem;
  }

  .layout {
    display: grid;
    gap: 0.8rem;
  }

  .sections ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
  }

  .sections button {
    padding: 0.3em 0.75em;
    font-size: 0.9rem;
    color: var(--fg-dim);
    background: transparent;
    border-color: var(--border);
    border-radius: 999px;
  }

  .sections button[aria-current='true'] {
    color: var(--fg);
    background: var(--bg-raised);
    border-color: var(--accent);
  }

  @media (min-width: 760px) {
    .layout {
      grid-template-columns: 11rem minmax(0, 1fr);
      align-items: start;
      gap: 1.25rem;
    }

    .sections {
      position: sticky;
      top: 1rem;
    }

    .sections ul {
      flex-direction: column;
      gap: 0.15rem;
    }

    .sections button {
      width: 100%;
      text-align: left;
      border-color: transparent;
      border-radius: var(--radius);
    }

    .sections button[aria-current='true'] {
      border-color: var(--border);
      box-shadow: inset 3px 0 0 var(--accent);
    }
  }

  form {
    min-width: 0;
  }

  .group {
    margin-bottom: 0.9rem;
    scroll-margin-top: 1rem;
  }

  .group h3 {
    margin: 0 0 0.2rem;
    font-size: 1.05rem;
  }

  .group h3:focus {
    outline: none;
  }

  .group h3:focus-visible {
    outline: 2px solid var(--accent);
  }

  .lead {
    margin-bottom: 0.4rem;
  }

  .field {
    display: grid;
    gap: 0.35rem 1rem;
    padding: 0.7rem 0;
    border-top: 1px solid var(--border);
    min-width: 0;
  }

  .field:first-of-type {
    border-top: none;
  }

  .lead + .field {
    border-top: none;
  }

  .field > label:first-child,
  .field > .label {
    font-weight: 500;
  }

  @media (min-width: 560px) {
    .field {
      grid-template-columns: 10rem minmax(0, 1fr);
      align-items: start;
    }

    .field > label:first-child,
    .field > .label {
      padding-top: 0.45em;
    }

    .field > :not(:first-child) {
      grid-column: 2;
    }

    .field.check {
      grid-template-columns: minmax(0, 1fr);
    }

    .field.check > :not(:first-child) {
      grid-column: 1;
    }
  }

  .field > select {
    justify-self: start;
  }

  form :global(:is(input, select, button)) {
    scroll-margin-bottom: 5rem;
  }

  .wide {
    width: 100%;
    max-width: 28rem;
  }

  .field.check > label {
    display: flex;
    align-items: baseline;
    gap: 0.5em;
    padding-top: 0;
  }

  .field.check > .help {
    padding-left: 1.6em;
  }

  .pair {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6rem 1.5rem;
  }

  .pair label {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: 0.9rem;
  }

  .unit {
    display: inline-flex;
    align-items: center;
    gap: 0.4em;
    color: var(--fg-dim);
  }

  .unit input {
    width: 7.5em;
    color: var(--fg);
  }

  .tag {
    display: inline-block;
    padding: 0 0.4em;
    border: 1px solid var(--border);
    border-radius: 3px;
    font-size: 0.85em;
    color: var(--fg-dim);
  }

  .savebar {
    position: sticky;
    bottom: 0.75rem;
    z-index: 2;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem 1rem;
    padding: 0.6rem 0.8rem;
    background: var(--bg-raised);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    box-shadow: 0 6px 20px rgb(0 0 0 / 25%);
  }

  /* Without overflow-x: clip, body scrolls and sticky cannot follow the window. */
  @supports not (overflow-x: clip) {
    .savebar {
      position: fixed;
      right: var(--gutter);
      bottom: var(--gutter);
      left: var(--gutter);
      max-width: 60rem;
      margin: 0 auto;
    }

    form:has(.savebar) {
      padding-bottom: 5rem;
    }
  }

  .field-error {
    margin: 0.3rem 0 0;
    font-size: 0.85rem;
  }

  .unit input[aria-invalid='true'] {
    border-color: var(--danger);
  }

  .savebar p {
    margin: 0;
    font-weight: 500;
  }

  .actions {
    display: flex;
    gap: 0.5rem;
    margin-left: auto;
  }

  .error {
    color: var(--danger);
  }
</style>
