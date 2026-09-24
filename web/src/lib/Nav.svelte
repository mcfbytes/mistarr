<script lang="ts">
  import { getRoute } from './router.svelte';

  const route = $derived(getRoute());

  const links: { hash: string; label: string; match: string[] }[] = [
    { hash: '#/', label: 'Platforms', match: ['platforms', 'browse'] },
    { hash: '#/activity', label: 'Activity', match: ['activity'] },
    { hash: '#/sources', label: 'Sources', match: ['sources'] },
    { hash: '#/dats', label: 'DATs', match: ['dats'] },
    { hash: '#/system', label: 'System', match: ['system'] }
  ];
</script>

<nav>
  {#each links as link (link.hash)}
    <a href={link.hash} class:active={link.match.includes(route.name)}>{link.label}</a>
  {/each}
</nav>

<style>
  nav {
    display: flex;
    gap: 0.5em;
    padding: 0.6em var(--gutter);
    border-bottom: 1px solid var(--border);
    overflow-x: auto;
  }

  a {
    padding: 0.4em 0.7em;
    border-radius: var(--radius);
    color: var(--fg-dim);
    text-decoration: none;
    white-space: nowrap;
  }

  a.active {
    color: var(--fg);
    background: var(--bg-raised);
  }

  @media (max-width: 420px) {
    nav {
      flex-wrap: wrap;
      gap: 0.1em;
      padding: 0.5em calc(var(--gutter) / 2);
      font-size: 0.9em;
    }

    a {
      padding: 0.4em 0.35em;
    }
  }
</style>
