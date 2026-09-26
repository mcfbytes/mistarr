import { api } from '../api';
import { fixtureDownloads, fixtureImports } from '../fixtures';
import { readAllPages } from '../paging';
import type { Download, DownloadState, ImportLogEntry } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

let downloads = $state<Download[]>([]);
let imports = $state<ImportLogEntry[]>([]);

export function getDownloads(): Download[] {
  return downloads;
}

export function getImports(): ImportLogEntry[] {
  return imports;
}

export async function loadDownloads(): Promise<void> {
  downloads = isMock
    ? fixtureDownloads
    : await readAllPages((limit, offset) => api.downloads(limit, offset), (d) => d.id);
}

export async function loadImports(): Promise<void> {
  imports = isMock ? fixtureImports : await readAllPages((limit, offset) => api.imports(limit, offset), (i) => i.id);
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
