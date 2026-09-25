<script lang="ts">
  import type { SeedPolicy } from './types';

  /** A source's seed policy picker; `onpick` gets the chosen policy. */
  let {
    policy,
    label,
    onpick
  }: { policy: string; label?: string; onpick: (policy: SeedPolicy) => void } = $props();

  // The server may format a ratio as "1.0"; compare the parsed number so the matching option shows.
  const value = $derived.by(() => {
    const ratio = policy.startsWith('ratio:') ? parseFloat(policy.slice('ratio:'.length)) : null;
    return ratio !== null && Number.isFinite(ratio) ? `ratio:${ratio}` : policy;
  });
</script>

<select {value} aria-label={label} onchange={(e) => onpick(e.currentTarget.value as SeedPolicy)}>
  <option value="none">None</option>
  <option value="client">Client default</option>
  <option value="ratio:1">Until ratio 1</option>
  <option value="ratio:2">Until ratio 2</option>
</select>
