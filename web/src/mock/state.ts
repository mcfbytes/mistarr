import { ApiError } from '../lib/api';
import { PAGE_SIZE } from '../lib/paging';
import type {
  DatVersion,
  Download,
  ImportLogEntry,
  IncomingFile,
  Job,
  Paged,
  Platform,
  Settings,
  Source,
  SystemStatus,
  WizardStatus
} from '../lib/types';
import {
  fixtureDats,
  fixtureDownloads,
  fixtureImports,
  fixtureIncomingDats,
  fixtureIncomingSources,
  fixtureJobs,
  fixtureRecentJobs,
  fixtureSettings,
  mockSources
} from './fixtures';
import { mockKnob, mockScenario } from './knobs';

/** A watched directory, as `GET /dats/incoming` and `GET /sources/incoming` list it. */
export type Watched = 'dats' | 'sources';

/** What the mock server holds for the page's life; the scenario and knobs seed it. */
export interface MockState {
  dats: DatVersion[];
  sources: Source[];
  incoming: Record<Watched, IncomingFile[]>;
  jobs: Job[];
  recent: Job[];
  downloads: Download[];
  imports: ImportLogEntry[];
  settings: Settings;
  /** Fields a call changed, laid over the scenario's status. */
  status: Partial<SystemStatus>;
  /** Fields a call changed, laid over the scenario's wizard state. */
  wizard: Partial<WizardStatus>;
  /** Fields a call changed per platform id, laid over the scenario's platforms. */
  platforms: Map<string, Partial<Platform>>;
  /** Wanted marks per group id, over the fixture's. */
  wantedGroups: Map<number, number>;
  wantedVariants: Set<number>;
}

let state: MockState | null = null;

/** The mock server's state, built on first use once the page's knobs are set. */
export function mock(): MockState {
  state ??= {
    dats: mockKnob<boolean>('noDats', false) ? [] : fixtureDats.map((d) => ({ ...d })),
    sources: mockKnob<boolean>('noSources', false) ? [] : mockSources(),
    incoming: { dats: [...fixtureIncomingDats], sources: [...fixtureIncomingSources] },
    jobs: mockScenario() === 'busy' ? structuredClone(fixtureJobs) : [],
    recent: mockScenario() === 'idle' ? [] : structuredClone(fixtureRecentJobs),
    downloads: structuredClone(fixtureDownloads),
    imports: structuredClone(fixtureImports),
    settings: structuredClone(fixtureSettings),
    status: {},
    wizard: {},
    platforms: new Map(),
    wantedGroups: new Map(),
    wantedVariants: new Set()
  };
  return state;
}

/** `value` as it arrives over the wire, sharing nothing with what the sender holds. */
export function wire<T>(value: T): T {
  return value === undefined ? value : (JSON.parse(JSON.stringify(value)) as T);
}

/**
 * Answers with what `make` returns after `ms`, as the network would, so the app never
 * holds the server's own objects; an error `make` throws rejects instead.
 */
export function reply<T>(make: () => T, ms = 0): Promise<T> {
  return new Promise((resolve, reject) => {
    setTimeout(() => {
      try {
        resolve(wire(make()));
      } catch (err) {
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    }, ms);
  });
}

/** A page of `all` as the server's `?limit=&offset=` gives it, capped at `PAGE_SIZE` or knob `pageCap`. */
export function paged<T>(all: readonly T[], limit: number, offset: number): Paged<T> {
  const cap = mockKnob<number>('pageCap', PAGE_SIZE);
  const capped = Math.min(limit, Number.isInteger(cap) && cap > 0 ? Math.min(cap, PAGE_SIZE) : PAGE_SIZE);
  return { items: all.slice(offset, offset + capped), total: all.length };
}

/** The 404 the server answers for an id it does not hold. */
export function notFound(what: string): ApiError {
  return new ApiError('not_found', `No ${what} has this id.`, 404);
}

let lastJobId = 9000;

/** A job id no fixture uses. */
export function nextJobId(): number {
  lastJobId += 1;
  return lastJobId;
}

/** The time now in the API's whole seconds. */
export function nowSecs(): number {
  return Math.floor(Date.now() / 1000);
}
