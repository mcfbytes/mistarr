import { mount } from 'svelte';
import App from './App.svelte';
import { useApi } from './lib/api';

async function start(): Promise<void> {
  // Mock mode answers the API from src/mock; a production build drops this branch and its chunk.
  if (import.meta.env.VITE_MOCK === '1') {
    useApi((await import('./mock/api')).mockApi);
  }
  const target = document.getElementById('app');
  if (!target) {
    throw new Error('missing #app mount point');
  }
  mount(App, { target });
}

void start();
