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

  /** Sets the node's markup; the SVG is built from numbers in generate.ts, never from input. */
  function paint(node: HTMLElement, svg: string): { update(next: string): void } {
    node.innerHTML = svg;
    return {
      update(next) {
        node.innerHTML = next;
      }
    };
  }
</script>

<div class="art" class:muted aria-hidden="true" data-art-family={art.family} use:paint={art.svg}></div>

<style>
  .art {
    position: absolute;
    inset: 0;
    overflow: hidden;
    pointer-events: none;
    --l: 0;
  }

  .art :global(svg) {
    display: block;
    width: 100%;
    height: 100%;
  }

  @media (prefers-color-scheme: light) {
    .art {
      --l: 1;
    }
  }

  .muted {
    filter: grayscale(0.85);
    opacity: 0.45;
  }
</style>
