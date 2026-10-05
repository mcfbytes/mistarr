/** Every delay, in milliseconds, that merges a burst of calls or events into one. */
export const DELAY_MS = {
  search: 250,
  incoming: 300,
  reload: 500,
  window: 2000
} as const;

/** A function that coalesces its calls; `cancel` drops any pending run. */
export interface Coalesced {
  (): void;
  cancel(): void;
}

/**
 * Runs `fn` once for a burst of calls: after `ms` by default, or at once with `leading`,
 * after which calls inside the window earn one more run when it closes.
 */
export function coalesce(fn: () => void, ms: number, { leading = false }: { leading?: boolean } = {}): Coalesced {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let again = false;
  const call = (): void => {
    if (timer !== undefined) {
      again = true;
      return;
    }
    again = false;
    if (leading) {
      fn();
    }
    timer = setTimeout(() => {
      timer = undefined;
      if (!leading) {
        fn();
      } else if (again) {
        call();
      }
    }, ms);
  };
  call.cancel = (): void => {
    clearTimeout(timer);
    timer = undefined;
    again = false;
  };
  return call;
}

/** Runs `fn` with the latest value once calls have paused for `ms`. */
export function debounce<A>(fn: (value: A) => void, ms: number): ((value: A) => void) & { cancel(): void } {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const call = (value: A): void => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      timer = undefined;
      fn(value);
    }, ms);
  };
  call.cancel = (): void => {
    clearTimeout(timer);
    timer = undefined;
  };
  return call;
}
