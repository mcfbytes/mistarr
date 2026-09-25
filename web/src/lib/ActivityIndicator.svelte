<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { getJobs, getRecentJobs, jobOutcome, loadJobs, watchRecent } from './stores/jobs.svelte';
  import { findPlatform } from './stores/platforms.svelte';
  import { QUIET_KINDS, describeProgress, jobDetail, jobHref, jobStatus, kindLabel } from './status';
  import StatusPill from './StatusPill.svelte';
  import ProgressBar from './ProgressBar.svelte';
  import ClientHeld from './ClientHeld.svelte';
  import type { Job } from './types';

  /** The always-visible work indicator in the nav and its panel; see docs/UI.md "Activity indicator". */
  let open = $state(false);
  let button = $state<HTMLButtonElement>();
  let panel = $state<HTMLElement>();
  let place = $state({ top: 0, right: 8 });
  let stopRecent: (() => void) | null = null;

  onMount(() => {
    void loadJobs().catch(() => undefined);
    return () => stopRecent?.();
  });

  const active = $derived(getJobs().filter((j) => !QUIET_KINDS.has(j.kind)));
  const running = $derived(active.filter((j) => j.state === 'running'));
  const waiting = $derived(active.filter((j) => j.state !== 'running'));
  const recent = $derived(getRecentJobs().slice(0, 5));
  const summary = $derived(
    active.length === 0
      ? 'nothing running'
      : [running.length > 0 ? `${running.length} running` : '', waiting.length > 0 ? `${waiting.length} waiting` : '']
          .filter(Boolean)
          .join(', ')
  );

  function name(id: string): string {
    return findPlatform(id)?.name ?? id;
  }

  function title(job: Job): string {
    const detail = typeof job.payload.platform_id === 'string' ? name(job.payload.platform_id) : jobDetail(job.payload);
    return detail ? `${kindLabel(job.kind)}: ${detail}` : kindLabel(job.kind);
  }

  const relative = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });

  function ago(secs: number): string {
    const diff = Math.round(secs - Date.now() / 1000);
    if (Math.abs(diff) < 60) {
      return relative.format(diff, 'second');
    }
    if (Math.abs(diff) < 3600) {
      return relative.format(Math.round(diff / 60), 'minute');
    }
    if (Math.abs(diff) < 86_400) {
      return relative.format(Math.round(diff / 3600), 'hour');
    }
    return relative.format(Math.round(diff / 86_400), 'day');
  }

  function measure(): void {
    const rect = button?.getBoundingClientRect();
    if (rect) {
      place = { top: Math.round(rect.bottom + 6), right: Math.max(8, Math.round(window.innerWidth - rect.right)) };
    }
  }

  async function show(): Promise<void> {
    measure();
    open = true;
    stopRecent = watchRecent();
    await tick();
    panel?.focus();
  }

  function hide(returnFocus: boolean): void {
    if (!open) {
      return;
    }
    open = false;
    stopRecent?.();
    stopRecent = null;
    if (returnFocus) {
      button?.focus();
    }
  }

  function toggle(): void {
    if (open) {
      hide(false);
    } else {
      void show();
    }
  }

  function onKey(e: KeyboardEvent): void {
    if (e.key === 'Escape' && open) {
      e.stopPropagation();
      hide(true);
    }
  }

  // A press outside the panel and its button closes it without moving focus.
  function onPointer(e: PointerEvent): void {
    const target = e.target as Node | null;
    if (open && target && !panel?.contains(target) && !button?.contains(target)) {
      hide(false);
    }
  }

  // Focus leaving the panel for somewhere other than its button closes it.
  function onFocusOut(e: FocusEvent): void {
    const next = e.relatedTarget as Node | null;
    if (next && !panel?.contains(next) && !button?.contains(next)) {
      hide(false);
    }
  }
</script>

<svelte:window onkeydown={onKey} onpointerdown={onPointer} onresize={() => open && measure()} />

<button
  bind:this={button}
  type="button"
  class="indicator"
  class:busy={running.length > 0}
  class:waiting={running.length === 0 && waiting.length > 0}
  aria-expanded={open}
  aria-controls="activity-panel"
  aria-label={`Background work: ${summary}`}
  onclick={toggle}
>
  <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true">
    <path
      d="M2 10h3.5l2-5 4 10 2-5H18"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
  </svg>
  <span class="count" aria-hidden="true">{active.length > 0 ? active.length : ''}</span>
  <span class="dot" aria-hidden="true"></span>
</button>
<p class="visually-hidden" aria-live="polite">Background work: {summary}</p>

{#if open}
  <section
    bind:this={panel}
    id="activity-panel"
    class="panel"
    aria-labelledby="activity-heading"
    tabindex="-1"
    style:top={`${place.top}px`}
    style:right={`${place.right}px`}
    onfocusout={onFocusOut}
  >
    <header>
      <h2 id="activity-heading">Background work</h2>
      <span class="muted">{summary.charAt(0).toUpperCase() + summary.slice(1)}</span>
    </header>
    <ClientHeld />

    {#if active.length === 0}
      <p class="empty muted">Nothing is running or waiting.</p>
    {/if}

    {#if running.length > 0}
      <h3>Running</h3>
      <ul aria-label="Running">
        {#each running as job (job.id)}
          {@const view = describeProgress(job.kind, job.progress) ?? { fraction: null, text: 'Starting' }}
          <li>
            <div class="row">
              <StatusPill {...jobStatus(job)} />
              <a href={jobHref(job)} onclick={() => hide(false)}>{title(job)}</a>
            </div>
            <ProgressBar {view} label={`${title(job)} progress`} compact />
          </li>
        {/each}
      </ul>
    {/if}

    {#if waiting.length > 0}
      <h3>Waiting</h3>
      <ul aria-label="Waiting">
        {#each waiting as job (job.id)}
          <li>
            <div class="row">
              <StatusPill {...jobStatus(job)} />
              <a href={jobHref(job)} onclick={() => hide(false)}>{title(job)}</a>
            </div>
            {#if job.reason}<p class="why">{job.reason}</p>{/if}
          </li>
        {/each}
      </ul>
    {/if}

    {#if recent.length > 0}
      <h3>Finished</h3>
      <ul aria-label="Finished">
        {#each recent as job (job.id)}
          <li>
            <div class="row">
              <StatusPill {...jobStatus(job)} />
              <a href={jobHref(job)} onclick={() => hide(false)}>{jobOutcome(job, name)}</a>
            </div>
            <p class="why">{ago(job.updated_at)}</p>
          </li>
        {/each}
      </ul>
    {/if}

    <footer><a href="#/activity" onclick={() => hide(false)}>Open Activity</a></footer>
  </section>
{/if}

<style>
  .indicator {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 0.25em;
    margin-left: auto;
    min-width: 3.2em;
    padding: 0.35em 0.5em 0.35em 0.6em;
    border-color: transparent;
    background: transparent;
    color: var(--fg-dim);
  }

  .indicator.busy {
    color: var(--accent);
  }

  .indicator.waiting {
    color: var(--warn);
  }

  .indicator[aria-expanded='true'] {
    background: var(--bg-raised);
    border-color: var(--border);
  }

  .count {
    min-width: 1ch;
    font-size: 0.85em;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .dot {
    position: absolute;
    top: 0.25em;
    left: 0.3em;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0;
  }

  .busy .dot {
    opacity: 1;
    animation: breathe 2.4s ease-in-out infinite;
  }

  @keyframes breathe {
    50% {
      opacity: 0.25;
    }
  }

  .panel {
    position: fixed;
    z-index: 20;
    width: min(24rem, calc(100vw - 16px));
    max-height: min(32rem, calc(100vh - 80px));
    overflow-y: auto;
    padding: 0.8em 1em;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.3);
    font-size: 0.92em;
  }

  .panel:focus {
    outline: none;
  }

  .panel:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.2em 0.8em;
  }

  h2 {
    margin: 0;
    font-size: 1em;
  }

  h3 {
    margin: 0.9em 0 0.3em;
    font-size: 0.75em;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-dim);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  li {
    padding: 0.45em 0;
    border-top: 1px solid var(--border);
  }

  .row {
    display: flex;
    align-items: baseline;
    gap: 0.5em;
    margin-bottom: 0.3em;
    min-width: 0;
  }

  .row a {
    min-width: 0;
    color: var(--fg);
    overflow-wrap: anywhere;
  }

  .why {
    margin: 0;
    font-size: 0.8rem;
    color: var(--fg-dim);
    overflow-wrap: anywhere;
  }

  .empty {
    margin: 0.8em 0 0.2em;
  }

  footer {
    margin-top: 0.8em;
    padding-top: 0.6em;
    border-top: 1px solid var(--border);
    font-size: 0.9em;
  }

  @media (max-width: 480px) {
    .panel {
      left: 8px;
      right: 8px !important;
      width: auto;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .busy .dot {
      animation: none;
    }
  }
</style>
