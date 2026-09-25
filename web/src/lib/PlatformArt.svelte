<script lang="ts">
  import { platformArt, type ArtFormat } from './art/generate';
  import type { PlatformKind } from './types';

  interface Props {
    id: string;
    kind?: PlatformKind | undefined;
    format?: ArtFormat;
    /** Desaturated and dimmed, for platforms that are disabled or have no core. */
    muted?: boolean;
  }

  const { id, kind, format = 'card', muted = false }: Props = $props();
  const art = $derived(platformArt(id, kind, format));
</script>

<div class="art" class:muted aria-hidden="true" data-art-family={art.family}>
  <!-- eslint-disable-next-line svelte/no-at-html-tags -- built from numbers in generate.ts, never from input -->
  {@html art.svg}
</div>

<style>
  .art {
    position: absolute;
    inset: 0;
    overflow: hidden;
    pointer-events: none;
  }

  .art :global(svg) {
    display: block;
    width: 100%;
    height: 100%;
    --c0: var(--c0d);
    --c1: var(--c1d);
    --c2: var(--c2d);
    --c3: var(--c3d);
    --c4: var(--c4d);
    --c5: var(--c5d);
    --c6: var(--c6d);
    --c7: var(--c7d);
    --c8: var(--c8d);
  }

  @media (prefers-color-scheme: light) {
    .art :global(svg) {
      --c0: var(--c0l);
      --c1: var(--c1l);
      --c2: var(--c2l);
      --c3: var(--c3l);
      --c4: var(--c4l);
      --c5: var(--c5l);
      --c6: var(--c6l);
      --c7: var(--c7l);
      --c8: var(--c8l);
    }
  }

  .muted {
    filter: grayscale(0.85);
    opacity: 0.45;
  }
</style>
