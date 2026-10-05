import { api, errorMessage } from './api';
import { cleanPathMap } from './pathmap';
import { loadStatus } from './stores/status.svelte';
import type { Settings, SettingsPatch } from './types';

/** The settings the server holds after a save, or the sentence saying why it was not saved. */
export type SaveResult = { settings: Settings } | { error: string };

/**
 * Saves the sections in `patch`, with a client path map cleaned of blank rows first, then
 * reads the status again, since the client and the title choice follow the settings.
 */
export async function saveSettings(patch: SettingsPatch): Promise<SaveResult> {
  let body = patch;
  if (patch.client) {
    const cleaned = cleanPathMap(patch.client.remote_path_map);
    if ('error' in cleaned) {
      return { error: cleaned.error };
    }
    body = { ...patch, client: { ...patch.client, remote_path_map: cleaned.map } };
  }
  let settings: Settings;
  try {
    settings = await api.putSettings(body);
  } catch (err) {
    return { error: errorMessage(err) };
  }
  // A failed refresh leaves the tiles as they were; SSE brings the next status.
  void loadStatus().catch(() => undefined);
  return { settings };
}
