import { api } from '../api';
import { fixtureDownloads, fixtureImports } from '../fixtures';
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
  downloads = isMock ? fixtureDownloads : (await api.downloads()).items;
}

export async function loadImports(): Promise<void> {
  imports = isMock ? fixtureImports : (await api.imports()).items;
}

export function applyDownloadChanged(downloadId: number, state: DownloadState, progress: number): void {
  downloads = downloads.map((d) => (d.id === downloadId ? { ...d, state, progress } : d));
}
