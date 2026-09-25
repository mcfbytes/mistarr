<script lang="ts">
  import './app.css';
  import { getRoute, navigate } from './lib/router.svelte';
  import { getWizard, isConnected, isUnauthorized, loadStatus, loadWizard, setUnauthorized } from './lib/stores/status.svelte';
  import { startEvents } from './lib/stores/events';
  import { ApiError, setApiKey } from './lib/api';
  import Nav from './lib/Nav.svelte';
  import Toasts from './lib/Toasts.svelte';
  import HeldBanner from './lib/HeldBanner.svelte';
  import Wizard from './routes/Wizard.svelte';
  import Platforms from './routes/Platforms.svelte';
  import Browse from './routes/Browse.svelte';
  import TitleScreen from './routes/Title.svelte';
  import Activity from './routes/Activity.svelte';
  import Sources from './routes/Sources.svelte';
  import SourceDetail from './routes/SourceDetail.svelte';
  import Dats from './routes/Dats.svelte';
  import System from './routes/System.svelte';

  const isMock = import.meta.env.VITE_MOCK === '1';
  let apiKeyInput = $state('');
  let wizardRetryMs = 1000;

  // Keeps retrying with backoff on any failure but 401, so a slow-starting
  // server still gets the SSE stream and eventually the wizard redirect.
  async function checkFirstRun(): Promise<void> {
    try {
      await loadWizard();
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) {
        setUnauthorized(true);
        return;
      }
      setTimeout(() => void checkFirstRun(), wizardRetryMs);
      wizardRetryMs = Math.min(wizardRetryMs * 2, 30000);
      return;
    }
    wizardRetryMs = 1000;
    setUnauthorized(false);
    void loadStatus().catch(() => undefined);
    const wizard = getWizard();
    if (wizard && wizard.open_on_start && getRoute().name !== 'wizard') {
      navigate('/wizard');
    }
  }

  startEvents();
  if (isMock) {
    void loadStatus();
  } else {
    void checkFirstRun();
  }

  function submitApiKey(): void {
    setApiKey(apiKeyInput.trim() || null);
    apiKeyInput = '';
    void checkFirstRun();
  }

  const route = $derived(getRoute());
  const connected = $derived(isConnected());
  const unauthorized = $derived(isUnauthorized());
</script>

{#if unauthorized}
  <div class="page">
    <h1>API key required</h1>
    <p class="muted">This server requires an API key. Enter it to continue.</p>
    <form onsubmit={(e) => e.preventDefault()}>
      <input type="password" placeholder="API key" bind:value={apiKeyInput} />
      <button class="primary" onclick={submitApiKey}>Continue</button>
    </form>
  </div>
{:else}
  {#if route.name !== 'wizard'}
    <Nav />
  {/if}

  {#if !connected && route.name !== 'wizard'}
    <div class="banner">Disconnected from server. Reconnecting…</div>
  {/if}

  {#if route.name !== 'wizard'}
    <HeldBanner />
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
  {:else if route.name === 'source'}
    {#key route.params.id}
      <SourceDetail sourceId={Number(route.params.id ?? 0)} />
    {/key}
  {:else if route.name === 'dats'}
    <Dats />
  {:else if route.name === 'system'}
    <System />
  {:else}
    <div class="page">Not found.</div>
  {/if}
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
