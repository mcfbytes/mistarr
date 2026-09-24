<script lang="ts">
  import './app.css';
  import { getRoute, navigate } from './lib/router.svelte';
  import { getWizard, isConnected, loadWizard } from './lib/stores/status.svelte';
  import { startEvents } from './lib/stores/events';
  import Nav from './lib/Nav.svelte';
  import Toasts from './lib/Toasts.svelte';
  import Wizard from './routes/Wizard.svelte';
  import Platforms from './routes/Platforms.svelte';
  import Browse from './routes/Browse.svelte';
  import TitleScreen from './routes/Title.svelte';
  import Activity from './routes/Activity.svelte';
  import Sources from './routes/Sources.svelte';
  import System from './routes/System.svelte';

  startEvents();

  async function checkFirstRun(): Promise<void> {
    await loadWizard();
    const wizard = getWizard();
    if (wizard && !wizard.steps.paths && getRoute().name !== 'wizard') {
      navigate('/wizard');
    }
  }

  void checkFirstRun();

  const route = $derived(getRoute());
  const connected = $derived(isConnected());
</script>

{#if route.name !== 'wizard'}
  <Nav />
{/if}

{#if !connected && route.name !== 'wizard'}
  <div class="banner">Disconnected from server. Reconnecting…</div>
{/if}

<Toasts />

{#if route.name === 'wizard'}
  <Wizard />
{:else if route.name === 'platforms'}
  <Platforms />
{:else if route.name === 'browse'}
  <Browse platformId={route.params.id ?? ''} />
{:else if route.name === 'title'}
  <TitleScreen titleId={Number(route.params.id ?? 0)} />
{:else if route.name === 'activity'}
  <Activity />
{:else if route.name === 'sources'}
  <Sources />
{:else if route.name === 'system'}
  <System />
{:else}
  <div class="page">Not found.</div>
{/if}

<style>
  .banner {
    background: var(--warn);
    color: #1a1c20;
    text-align: center;
    padding: 0.4em;
    font-size: 0.9em;
  }
</style>
