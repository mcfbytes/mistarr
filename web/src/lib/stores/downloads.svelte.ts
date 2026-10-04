import { api } from '../api';
import { readAllPages } from '../paging';
import type { Download, DownloadState, ImportLogEntry } from '../types';

let downloads = $state<Download[]>([]);
let imports = $state<ImportLogEntry[]>([]);
// Pages showing the imports list; it is re-read on import.done only while one is open.
let importWatchers = 0;

export function getDownloads(): Download[] {
  return downloads;
}

export function getImports(): ImportLogEntry[] {
  return imports;
}

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

export async function loadDownloads(): Promise<void> {
  downloads = await readAllPages((limit, offset) => api.downloads(limit, offset), (d) => d.id);
}

export async function loadImports(): Promise<void> {
  imports = await readAllPages((limit, offset) => api.imports(limit, offset), (i) => i.id);
}

export function patchDownload(id: number, patch: Partial<Download>): void {
  downloads = downloads.map((d) => (d.id === id ? { ...d, ...patch } : d));
}

export function applyDownloadChanged(downloadId: number, state: DownloadState, progress: number): void {
  const exists = downloads.some((d) => d.id === downloadId);
  if (exists) {
    patchDownload(downloadId, { state, progress });
  } else {
    void loadDownloads();
  }
}
