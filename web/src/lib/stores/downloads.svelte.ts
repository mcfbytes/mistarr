import { api } from '../api';
import { DELAY_MS } from '../coalesce';
import type { Download, ImportLogEntry } from '../types';
import { ListStore } from './list.svelte';

export const downloads = new ListStore<Download>(
  (limit, offset) => api.downloads(limit, offset),
  (d) => d.id
);

// A mass import fires import.done per file; one full-log re-read per window is enough.
export const imports = new ListStore<ImportLogEntry>(
  (limit, offset) => api.imports(limit, offset),
  (i) => i.id,
  { reload: { ms: DELAY_MS.window, leading: true } }
);

// Pages showing the imports list; it is re-read on import.done only while one is open.
let importWatchers = 0;

/** Whether an open page wants the imports list kept fresh on `import.done`. */
export function watchingImports(): boolean {
  return importWatchers > 0;
}

/** Marks the imports list as shown; returns the stop function. */
export function watchImports(): () => void {
  importWatchers += 1;
  return () => {
    importWatchers -= 1;
  };
}
