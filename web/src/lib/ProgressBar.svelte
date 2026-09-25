<script lang="ts">
  import type { ProgressView } from './status';

  /** A job's progress: a bar with its share done, or a moving band while the share is unknown. */
  let { view, label, compact = false }: { view: ProgressView; label: string; compact?: boolean } = $props();

  const pct = $derived(view.fraction === null ? null : Math.round(view.fraction * 100));
</script>

<div class="wrap" class:compact>
  <div
    class="bar"
    role="progressbar"
    aria-label={label}
    aria-valuemin={0}
    aria-valuemax={100}
    aria-valuenow={pct ?? undefined}
    aria-valuetext={view.text || undefined}
    data-fraction={view.fraction ?? ''}
  >
    {#if pct === null}
      <span class="band"></span>
    {:else}
      <span class="fill" style:width={`${pct}%`}></span>
    {/if}
  </div>
  {#if view.text}<p class="text">{view.text}</p>{/if}
</div>

<style>
  .wrap {
    min-width: 0;
  }

  .bar {
    position: relative;
    height: 6px;
    border-radius: 3px;
    background: var(--border);
    overflow: hidden;
  }

  .fill {
    display: block;
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.3s ease-out;
  }

  .band {
    position: absolute;
    inset: 0 auto 0 0;
    width: 35%;
    background: var(--accent);
    border-radius: 3px;
    animation: slide 1.6s ease-in-out infinite;
  }

  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(290%);
    }
  }

  .text {
    margin: 0.25em 0 0;
    font-size: 0.8rem;
    color: var(--fg-dim);
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }

  .compact .text {
    font-size: 0.75rem;
  }

  @media (prefers-reduced-motion: reduce) {
    .fill {
      transition: none;
    }

    .band {
      width: 100%;
      animation: none;
      opacity: 0.45;
      background: repeating-linear-gradient(
        -45deg,
        var(--accent) 0 6px,
        transparent 6px 12px
      );
    }
  }
</style>
