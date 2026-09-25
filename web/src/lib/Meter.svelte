<script lang="ts">
  /** A share of a fixed capacity, such as memory or storage in use; warns past `warnAt`. */
  let {
    fraction,
    label,
    text,
    warnAt = 0.85
  }: { fraction: number; label: string; text: string; warnAt?: number } = $props();

  const pct = $derived(Math.round(Math.min(1, Math.max(0, fraction)) * 100));
</script>

<div
  class="meter"
  class:warn={fraction >= warnAt}
  role="meter"
  aria-label={label}
  aria-valuemin={0}
  aria-valuemax={100}
  aria-valuenow={pct}
  aria-valuetext={text}
>
  <span class="fill" style:width={`${pct}%`}></span>
</div>

<style>
  .meter {
    height: 6px;
    border-radius: 3px;
    background: var(--border);
    overflow: hidden;
  }

  .fill {
    display: block;
    height: 100%;
    border-radius: 3px;
    background: var(--accent);
  }

  .warn .fill {
    background: var(--warn);
  }
</style>
