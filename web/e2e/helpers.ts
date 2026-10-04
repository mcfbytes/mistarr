import type { Page } from '@playwright/test';
import { MOCK_EVENT } from '../src/mock/events';
import type { SseEvent } from '../src/lib/types';

/** Opens the activity panel from the nav indicator and waits for it to show. */
export async function openPanel(page: Page): Promise<void> {
  await page.getByRole('button', { name: /^Background work:/ }).click();
  await page.getByRole('region', { name: 'Background work' }).waitFor();
}

/** Aborts every request that leaves the preview server, so posters are the generated ones. */
export async function noCovers(page: Page): Promise<void> {
  await page.route(/^https?:\/\/(?!localhost)/, (route) => route.abort());
}

/**
 * Sets mock knob `key` (see `src/mock/knobs.ts`) to `value`, or clears it for `null`.
 * Before the first `goto` it is set as each page starts, so the app reads it from the start.
 */
export async function setMockKnob(page: Page, key: string, value: unknown): Promise<void> {
  const args: [string, string | null] = [`mistarr.mock.${key}`, value === null ? null : JSON.stringify(value)];
  const apply = ([name, json]: [string, string | null]): void => {
    if (json === null) {
      localStorage.removeItem(name);
    } else {
      localStorage.setItem(name, json);
    }
  };
  if (page.url() === 'about:blank') {
    await page.addInitScript(apply, args);
  } else {
    await page.evaluate(apply, args);
  }
}

/** What the mock recorded under knob `key`, parsed, or null. */
export async function readMockKnob(page: Page, key: string): Promise<unknown> {
  const raw = await page.evaluate((name) => localStorage.getItem(name), `mistarr.mock.${key}`);
  return raw === null ? null : (JSON.parse(raw) as unknown);
}

/** Sends `event` down the mock's event stream, as the server would over SSE. */
export async function emitEvent(page: Page, event: SseEvent): Promise<void> {
  await page.evaluate(
    ([name, detail]) => window.dispatchEvent(new CustomEvent(name, { detail })),
    [MOCK_EVENT, event] as const
  );
}
