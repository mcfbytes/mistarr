<script lang="ts">
  import { onMount } from 'svelte';
  import { getStatus } from './stores/status.svelte';
  import { showToast } from './stores/toast.svelte';
  import { api, errorMessage } from './api';
  import PathMapEditor from './PathMapEditor.svelte';
  import SaveBar from './SaveBar.svelte';
  import { setLeaveGuard } from './router.svelte';
  import { saveSettings } from './settings';
  import { speedText } from './unidentified';
  import type { LimitsSettings, Settings } from './types';

  type LimitKey = keyof LimitsSettings;

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

  // The form, its section list and save bar; unsaved changes block a reload or leaving.
  let settings = $state<Settings | null>(null);
  let baseline = $state('');
  let saving = $state(false);
  let settingsError = $state<string | null>(null);
  let current = $state<string>(SECTIONS[0].id);
  let jumpedAt = 0;
  let invalidLimits = $state<LimitKey[]>([]);
  let limitsKey = $state(0);

  const status = $derived(getStatus());
  const dirty = $derived(settings !== null && JSON.stringify(settings) !== baseline);
  const showBar = $derived(dirty || invalidLimits.length > 0 || settingsError !== null);

  onMount(() => {
    void loadSettings();
  });

  function adopt(next: Settings): void {
    baseline = JSON.stringify(next);
    settings = JSON.parse(baseline) as Settings;
  }

  async function loadSettings(): Promise<void> {
    try {
      adopt(await api.settings());
      settingsError = null;
    } catch (err) {
      settingsError = errorMessage(err);
    }
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
    saving = true;
    const result = await saveSettings(settings);
    saving = false;
    if ('error' in result) {
      settingsError = result.error;
      return;
    }
    adopt(result.settings);
    showToast('Settings saved.', 'success');
  }
</script>

{#if !settings && settingsError}
  <section class="settings" aria-labelledby="settings-h">
    <h2 id="settings-h">Settings</h2>
    <p class="error" role="alert">{settingsError} <button type="button" onclick={() => void loadSettings()}>Retry</button></p>
  </section>
{:else if settings}
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
            <p id="hide-help" class="help">
              Comma-separated flags whose entries are left out: any of bios, beta, proto, demo, sample and program.
            </p>
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
          <SaveBar error={settingsError} {saving} ondiscard={discard} />
        {/if}
      </form>
    </div>
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

  @supports not (overflow-x: clip) {
    form:has(:global(.savebar)) {
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
</style>
