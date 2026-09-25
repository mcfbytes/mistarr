<script lang="ts">
  import PlatformArt from './PlatformArt.svelte';
  import { posterVariation } from './poster';
  import type { PlatformKind } from './types';

  interface Props {
    platformId: string;
    kind?: PlatformKind | undefined;
    /** The title's name without its tags, set large. */
    title: string;
    /** The full entry name; its parenthesised tags, such as the region, go on a line beneath. */
    name?: string | null;
  }

  const { platformId, kind, title, name = null }: Props = $props();
  const look = $derived(posterVariation(name ?? title));
  const tags = $derived(
    [...(name ?? '').matchAll(/\(([^()]+)\)/g)]
      .map((m) => m[1] ?? '')
      .filter((t) => t.length > 0)
      .join(' · ')
  );
</script>

<!-- The name is already on the page as text, so the poster is decoration. -->
<div class="poster-placeholder" aria-hidden="true" data-testid="poster-placeholder">
  <div class="band">
    <div
      class="shift"
      style:filter="hue-rotate({look.hue}deg)"
      style:transform="translateX({look.shift}%) scale({look.scale})"
    >
      <PlatformArt id={platformId} {kind} />
    </div>
  </div>
  <p class="title">{title}</p>
  {#if tags}
    <p class="tags">{tags}</p>
  {/if}
</div>

<style>
  .poster-placeholder {
    container-type: inline-size;
    position: relative;
    display: flex;
    flex-direction: column;
    width: 100%;
    aspect-ratio: 10 / 14;
    overflow: hidden;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--bg-raised);
  }

  .band {
    position: relative;
    flex: 0 0 46%;
    overflow: hidden;
    opacity: 0.8;
  }

  .shift {
    position: absolute;
    inset: 0;
    transform-origin: 50% 0;
  }

  .band::after {
    content: '';
    position: absolute;
    inset: 0;
    background: linear-gradient(to bottom, transparent 55%, var(--bg-raised));
  }

  .title {
    margin: 0;
    padding: 0.5em 9cqi 0;
    font-size: clamp(0.8rem, 11cqi, 1.6rem);
    font-weight: 700;
    line-height: 1.15;
    color: var(--fg);
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    overflow: hidden;
  }

  .tags {
    margin: auto 0 0;
    padding: 0 9cqi 8cqi;
    font-size: clamp(0.65rem, 7cqi, 1rem);
    color: var(--fg-dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
