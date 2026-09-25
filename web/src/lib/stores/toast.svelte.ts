/** `info` reports that work started, `success` that it finished, `error` that it failed. */
export type ToastTone = 'info' | 'success' | 'error';

export interface ToastMessage {
  id: number;
  text: string;
  tone: ToastTone;
}

let toasts = $state<ToastMessage[]>([]);
// The newest message of each urgency, for the live regions; dismissing a toast never repeats one.
let spoken = $state({ polite: '', assertive: '' });
let nextId = 1;
const SHOWN_MS: Record<ToastTone, number> = { info: 6000, success: 6000, error: 10000 };
const MAX_SHOWN = 4;

export function getToasts(): ToastMessage[] {
  return toasts;
}

/** The newest information or success message, and the newest error, for screen readers. */
export function getSpoken(): { polite: string; assertive: string } {
  return spoken;
}

/** Shows one short sentence; errors stay longer. The oldest goes once four are shown. */
export function showToast(text: string, tone: ToastTone = 'error'): void {
  const id = nextId++;
  toasts = [...toasts, { id, text, tone }].slice(-MAX_SHOWN);
  spoken = tone === 'error' ? { ...spoken, assertive: text } : { ...spoken, polite: text };
  setTimeout(() => {
    dismissToast(id);
  }, SHOWN_MS[tone]);
}

export function dismissToast(id: number): void {
  toasts = toasts.filter((t) => t.id !== id);
}
