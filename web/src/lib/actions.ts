import { errorMessage } from './api';
import { showToast } from './stores/toast.svelte';

/** What `optimistic` runs: show the change, ask the server, then keep what it answered. */
export interface Optimistic<T> {
  apply: () => void;
  revert: () => void;
  call: () => Promise<T>;
  commit?: (result: T) => void;
}

/** Shows `apply` at once; on a failure undoes it with `revert` and toasts why. True when the call succeeded. */
export async function optimistic<T>({ apply, revert, call, commit }: Optimistic<T>): Promise<boolean> {
  apply();
  try {
    const result = await call();
    commit?.(result);
    return true;
  } catch (err) {
    revert();
    showToast(errorMessage(err));
    return false;
  }
}

/** Runs `fn`; a failure is toasted after `prefix` and gives `undefined`, else the result. */
export async function attempt<T>(fn: () => Promise<T>, prefix = ''): Promise<T | undefined> {
  try {
    return await fn();
  } catch (err) {
    showToast(`${prefix}${errorMessage(err)}`);
    return undefined;
  }
}
