<script lang="ts">
  import { STATUS_LABEL, type WorkStatus } from './status';

  /** One status from the fixed vocabulary, with a shape as well as a colour; `label` renames it. */
  let { status, label }: { status: WorkStatus; label?: string } = $props();
</script>

<span class="pill {status}" data-status={status}>
  <svg viewBox="0 0 12 12" width="12" height="12" aria-hidden="true" focusable="false">
    {#if status === 'queued'}
      <circle cx="6" cy="6" r="4" fill="none" stroke="currentColor" stroke-width="1.6" />
    {:else if status === 'running'}
      <circle cx="6" cy="6" r="4" fill="none" stroke="currentColor" stroke-width="1.6" opacity="0.3" />
      <path class="arc" d="M6 2a4 4 0 0 1 4 4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
    {:else if status === 'waiting'}
      <circle cx="6" cy="6" r="4.2" fill="none" stroke="currentColor" stroke-width="1.4" />
      <path d="M6 3.6V6l1.7 1.2" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
    {:else if status === 'paused'}
      <rect x="3" y="2.5" width="2" height="7" rx="0.6" fill="currentColor" />
      <rect x="7" y="2.5" width="2" height="7" rx="0.6" fill="currentColor" />
    {:else if status === 'done'}
      <path d="M2.5 6.3l2.3 2.3 4.7-5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
    {:else}
      <path d="M3 3l6 6M9 3l-6 6" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
    {/if}
  </svg>
  <span>{label ?? STATUS_LABEL[status]}</span>
</span>

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 0.3em;
    padding: 0.05em 0.55em 0.05em 0.4em;
    border: 1px solid color-mix(in srgb, currentColor 45%, transparent);
    border-radius: 999px;
    background: color-mix(in srgb, currentColor 12%, transparent);
    font-size: 0.8rem;
    font-weight: 600;
    line-height: 1.5;
    white-space: nowrap;
    vertical-align: baseline;
  }

  svg {
    flex: none;
  }

  .queued,
  .paused {
    color: var(--fg-dim);
  }

  .running {
    color: var(--accent);
  }

  .waiting {
    color: var(--warn);
  }

  .done {
    color: var(--ok);
  }

  .failed {
    color: var(--danger);
  }

  .arc {
    transform-origin: 6px 6px;
    animation: turn 1.2s linear infinite;
  }

  @keyframes turn {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .arc {
      animation: none;
    }
  }
</style>
