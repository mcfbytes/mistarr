/**
 * Mock mode's scenario, from `?mock=` in the page URL: `idle` shows no work,
 * and `showcase` an idle, fully set up board with more platforms, for the README.
 */
export function mockScenario(): string {
  return new URLSearchParams(globalThis.location.search).get('mock') ?? 'busy';
}

const PREFIX = 'mistarr.mock.';

/** Whether `value` is the same kind of JSON value as `fallback`: number, object, array and so on. */
function sameKind(value: unknown, fallback: unknown): boolean {
  return value !== null && typeof value === typeof fallback && Array.isArray(value) === Array.isArray(fallback);
}

/**
 * Test knob `key`: the JSON in localStorage `mistarr.mock.<key>`, read on every call so
 * a test can change it while the page runs; `fallback` when unset, unreadable or of
 * another kind. docs/TESTING.md lists the knobs.
 */
export function mockKnob<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(PREFIX + key);
    if (raw === null) {
      return fallback;
    }
    const value: unknown = JSON.parse(raw);
    return sameKind(value, fallback) ? (value as T) : fallback;
  } catch {
    // Storage blocked or the value is not JSON: the fallback.
    return fallback;
  }
}

/** Stores `value` as knob `key`, so a test can read back what the mock was sent. */
export function recordKnob(key: string, value: unknown): void {
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(value));
  } catch {
    // Storage blocked: nothing to keep.
  }
}
