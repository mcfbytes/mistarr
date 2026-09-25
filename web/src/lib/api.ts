import type {
  ClientKind,
  CoresResult,
  IncomingFile,
  DatVersion,
  Download,
  DownloadState,
  ImportLogEntry,
  Job,
  Launched,
  Paged,
  Platform,
  Settings,
  SettingsPatch,
  Source,
  SourceFile,
  SseEvent,
  SystemStatus,
  TitleDetail,
  TitleFilters,
  TitleGroup,
  WizardStatus
} from './types';

const BASE = '/api/v1';
const API_KEY_STORAGE = 'mistarr.apiKey';

export function getApiKey(): string | null {
  try {
    return localStorage.getItem(API_KEY_STORAGE);
  } catch {
    return null;
  }
}

export function setApiKey(key: string | null): void {
  try {
    if (key) {
      localStorage.setItem(API_KEY_STORAGE, key);
    } else {
      localStorage.removeItem(API_KEY_STORAGE);
    }
  } catch {
    // storage unavailable, nothing to persist
  }
}

function headers(isFormData: boolean, extra?: Record<string, string>): Record<string, string> {
  const key = getApiKey();
  return {
    ...(isFormData ? {} : { 'Content-Type': 'application/json' }),
    'X-Mistarr': '1',
    ...(key ? { 'X-Api-Key': key } : {}),
    ...extra
  };
}

export class ApiError extends Error {
  code: string;
  status: number;

  constructor(code: string, message: string, status: number) {
    super(message);
    this.code = code;
    this.status = status;
  }
}

export function errorMessage(err: unknown): string {
  return err instanceof ApiError ? err.message : 'Something went wrong.';
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const isFormData = init?.body instanceof FormData;
  const res = await fetch(`${BASE}${path}`, {
    ...init,
    headers: { ...headers(isFormData), ...(init?.headers as Record<string, string> | undefined) }
  });
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: { code: string; message: string } } | null;
    throw new ApiError(body?.error?.code ?? 'unknown', body?.error?.message ?? res.statusText, res.status);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}

function query(params: Record<string, string | number | boolean | undefined>): string {
  const usp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== '') {
      usp.set(k, String(v));
    }
  }
  const s = usp.toString();
  return s ? `?${s}` : '';
}

/** An upload's answer: the file as the incoming list shows it; see docs/API.md "Upload answers". */
export type Uploaded = IncomingFile;

export interface Binding {
  dat_version_id: number;
  platform_id: string;
  job_id: number;
}

export const api = {
  status: (): Promise<SystemStatus> => request('/system/status'),
  wizard: (): Promise<WizardStatus> => request('/system/wizard'),
  scan: (platformId?: string): Promise<{ job_id: number | null; arcade_job_id?: number }> =>
    request('/system/scan', { method: 'POST', body: JSON.stringify({ platform_id: platformId }) }),
  cores: (): Promise<CoresResult> => request('/system/cores', { method: 'POST' }),
  pause: (): Promise<SystemStatus> => request('/system/pause', { method: 'POST' }),
  resume: (): Promise<SystemStatus> => request('/system/resume', { method: 'POST' }),
  jobs: (): Promise<Paged<Job>> => request('/system/jobs'),
  recentJobs: (): Promise<Paged<Job>> => request('/system/jobs/recent'),
  wizardDone: (): Promise<WizardStatus> => request('/system/wizard/done', { method: 'POST' }),
  startClient: (kind: ClientKind): Promise<SystemStatus> =>
    request('/system/client/start', { method: 'POST', body: JSON.stringify({ kind }) }),
  datsIncoming: (): Promise<Paged<IncomingFile>> => request('/dats/incoming'),
  sourcesIncoming: (): Promise<Paged<IncomingFile>> => request('/sources/incoming'),
  settings: (): Promise<Settings> => request('/system/settings'),
  putSettings: (patch: SettingsPatch): Promise<Settings> =>
    request('/system/settings', { method: 'PUT', body: JSON.stringify(patch) }),

  platforms: (): Promise<Paged<Platform>> => request('/platforms'),
  setPlatform: (id: string, enabled: boolean): Promise<Platform> =>
    request(`/platforms/${id}`, { method: 'PUT', body: JSON.stringify({ enabled }) }),
  bindPlatformDat: (id: string, datVersionId: number): Promise<Binding> =>
    request(`/platforms/${id}/dat`, { method: 'POST', body: JSON.stringify({ dat_version_id: datVersionId }) }),
  launchCore: (id: string): Promise<Launched> => request(`/platforms/${id}/launch-core`, { method: 'POST' }),

  titles: (
    platformId: string,
    filters: TitleFilters,
    limit: number,
    offset: number,
    signal?: AbortSignal
  ): Promise<Paged<TitleGroup>> =>
    request(`/platforms/${platformId}/titles${query({ ...filters, limit, offset })}`, { signal: signal ?? null }),
  title: (id: number, signal?: AbortSignal): Promise<TitleDetail> => request(`/titles/${id}`, { signal: signal ?? null }),
  want: (id: number, variantId?: number): Promise<TitleDetail> =>
    request(`/titles/${id}/want`, { method: 'POST', body: JSON.stringify({ variant_id: variantId }) }),
  unwant: (id: number): Promise<TitleDetail> => request(`/titles/${id}/want`, { method: 'DELETE' }),
  rename: (id: number, fileId: number): Promise<TitleDetail> =>
    request(`/titles/${id}/rename`, { method: 'POST', body: JSON.stringify({ file_id: fileId }) }),
  launchTitle: (id: number): Promise<Launched> => request(`/titles/${id}/launch`, { method: 'POST' }),

  dats: (limit: number, offset: number): Promise<Paged<DatVersion>> =>
    request(`/dats${query({ limit, offset })}`),
  uploadDat: (file: File): Promise<Uploaded> => {
    const form = new FormData();
    form.append('file', file);
    return request('/dats/upload', { method: 'POST', body: form });
  },
  deleteDat: (id: number): Promise<void> => request(`/dats/${id}`, { method: 'DELETE' }),
  retryRejectedDat: (file: string): Promise<Uploaded> =>
    request(`/dats/rejected/${encodeURIComponent(file)}/retry`, { method: 'POST' }),
  deleteRejectedDat: (file: string): Promise<void> =>
    request(`/dats/rejected/${encodeURIComponent(file)}`, { method: 'DELETE' }),

  sources: (): Promise<Paged<Source>> => request('/sources'),
  uploadSource: (file: File): Promise<Uploaded> => {
    const form = new FormData();
    form.append('file', file);
    return request('/sources/upload', { method: 'POST', body: form });
  },
  addMagnet: (magnet: string): Promise<Uploaded> =>
    request('/sources/upload', { method: 'POST', body: JSON.stringify({ magnet }) }),
  updateSource: (
    id: number,
    patch: { platform_id?: string | null; seed_policy?: string; state?: string }
  ): Promise<Source> => request(`/sources/${id}`, { method: 'PUT', body: JSON.stringify(patch) }),
  deleteSource: (id: number): Promise<void> => request(`/sources/${id}`, { method: 'DELETE' }),
  sourceFiles: (id: number): Promise<Paged<SourceFile>> => request(`/sources/${id}/files`),

  downloads: (state?: DownloadState): Promise<Paged<Download>> =>
    request(`/downloads${query({ state })}`),
  retryDownload: (id: number): Promise<Download> => request(`/downloads/${id}/retry`, { method: 'POST' }),
  cancelDownload: (id: number): Promise<Download> => request(`/downloads/${id}`, { method: 'DELETE' }),
  imports: (): Promise<Paged<ImportLogEntry>> => request('/imports')
};

const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;

export class EventSubscriber {
  private lastEventId: string | null = null;
  private controller: AbortController | null = null;
  private closed = false;
  private backoff = RECONNECT_MIN_MS;
  private onEvent: (event: SseEvent) => void;
  private onStateChange: (connected: boolean) => void;

  constructor(onEvent: (event: SseEvent) => void, onStateChange: (connected: boolean) => void) {
    this.onEvent = onEvent;
    this.onStateChange = onStateChange;
  }

  start(): void {
    this.closed = false;
    void this.connect();
  }

  stop(): void {
    this.closed = true;
    this.controller?.abort();
  }

  private async connect(): Promise<void> {
    if (this.closed) {
      return;
    }
    this.controller = new AbortController();
    const key = getApiKey();
    const reqHeaders: Record<string, string> = {};
    if (this.lastEventId) {
      reqHeaders['Last-Event-ID'] = this.lastEventId;
    }
    if (key) {
      reqHeaders['X-Api-Key'] = key;
    }
    try {
      const res = await fetch(`${BASE}/events`, { headers: reqHeaders, signal: this.controller.signal });
      if (!res.ok || !res.body) {
        throw new Error(`events stream failed: ${res.status}`);
      }
      this.backoff = RECONNECT_MIN_MS;
      this.onStateChange(true);
      await this.readStream(res.body);
    } catch {
      // connection failed or was aborted, handled by the disconnect signal below
    }
    this.onStateChange(false);
    // close() can run during the awaits above.
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
    if (!this.closed) {
      const wait = this.backoff;
      this.backoff = Math.min(this.backoff * 2, RECONNECT_MAX_MS);
      await new Promise((resolve) => setTimeout(resolve, wait));
      void this.connect();
    }
  }

  private async readStream(body: ReadableStream<Uint8Array>): Promise<void> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    let eventName = '';
    let dataLines: string[] = [];
    let id: string | null = null;

    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        return;
      }
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';
      for (const rawLine of lines) {
        const line = rawLine.replace(/\r$/, '');
        if (line === '') {
          if (dataLines.length > 0 && eventName) {
            this.dispatch(eventName, dataLines.join('\n'));
          }
          if (id !== null) {
            this.lastEventId = id;
          }
          eventName = '';
          dataLines = [];
          id = null;
          continue;
        }
        if (line.startsWith('event:')) {
          eventName = line.slice(6).trim();
        } else if (line.startsWith('data:')) {
          dataLines.push(line.slice(5).trim());
        } else if (line.startsWith('id:')) {
          // Ids are opaque `<epoch>-<seq>` strings; `resync`, `status` and live progress omit one.
          id = line.slice(3).trim();
        }
      }
    }
  }

  private dispatch(name: string, data: string): void {
    try {
      const parsed: unknown = JSON.parse(data);
      this.onEvent({ name, data: parsed } as SseEvent);
    } catch {
      // malformed event, drop it
    }
  }
}
