import type { EventStream } from '../lib/api';
import type { SseEvent } from '../lib/types';

/**
 * The window event whose `detail`, an `SseEvent`, a test dispatches to send that
 * event down every open mock stream; `e2e/helpers.ts` `emitEvent` does so.
 */
export const MOCK_EVENT = 'mistarr:mock-event';

type Listener = (event: SseEvent) => void;

const listeners = new Set<Listener>();

/** Sends `event` to every open stream on a later turn, as the server's SSE would. */
export function emit(event: SseEvent): void {
  setTimeout(() => {
    for (const listener of listeners) {
      listener(event);
    }
  }, 0);
}

/** Whether a stream is open, so timed work knows someone is listening. */
export function streamOpen(): boolean {
  return listeners.size > 0;
}

/**
 * An in-memory stream standing in for `GET /events`: it is connected once started and
 * delivers what `emit` and the page's `MOCK_EVENT`s send, until stopped.
 */
export function mockEvents(onEvent: Listener, onStateChange: (connected: boolean) => void): EventStream {
  const fromPage = (e: Event): void => {
    if (e instanceof CustomEvent) {
      onEvent(e.detail as SseEvent);
    }
  };
  return {
    start() {
      listeners.add(onEvent);
      window.addEventListener(MOCK_EVENT, fromPage);
      onStateChange(true);
    },
    stop() {
      listeners.delete(onEvent);
      window.removeEventListener(MOCK_EVENT, fromPage);
      onStateChange(false);
    }
  };
}
