import { ApiError, type Api } from '../lib/api';
import type {
  Download,
  IncomingFile,
  Paged,
  Platform,
  Source,
  SourceUpdated,
  SystemStatus,
  TitleDetail,
  TitleFilters,
  TitleGroup,
  WizardStatus
} from '../lib/types';
import { emit, mockEvents } from './events';
import {
  delayMs,
  extraPlatforms,
  fixtureCores,
  fixtureIncomingDats,
  fixtureSources,
  fixtureTitle,
  fixtureTitles,
  fixtureUnidentified,
  mockSourceDetail,
  mockSourceFilesPage,
  mockSourcePreview,
  removedIds,
  scenarioPlatforms,
  scenarioStatus,
  scenarioWizard
} from './fixtures';
import { cancelFetch, runJob, startFetch, startProgress } from './jobs';
import { mockKnob, recordKnob } from './knobs';
import { mock, nextJobId, notFound, nowSecs, paged, reply, wire, type Watched } from './state';

/** The Browse grid's page size, by which knob `delayMs` numbers pages. */
const TITLES_PAGE = 60;
/** How long a mock binding job runs. */
const BIND_MS = 2500;

function status(): SystemStatus {
  return { ...scenarioStatus(), ...mock().status };
}

function wizard(): WizardStatus {
  return { ...scenarioWizard(), ...mock().wizard };
}

function platforms(): Platform[] {
  const changed = mock().platforms;
  return [...scenarioPlatforms(), ...extraPlatforms()].map((p) => ({ ...p, ...changed.get(p.id) }));
}

function findPlatform(id: string): Platform {
  const found = platforms().find((p) => p.id === id);
  if (!found) {
    throw notFound('platform');
  }
  return found;
}

function findSource(id: number): Source {
  const found = mock().sources.find((s) => s.id === id);
  if (!found) {
    throw notFound('source');
  }
  return found;
}

function patchSource(id: number, patch: Partial<Source>): Source {
  const s = mock();
  const next = { ...findSource(id), ...patch };
  s.sources = s.sources.map((row) => (row.id === id ? next : row));
  return next;
}

/** The title's detail with the variants marked wanted through `want`. */
function titleDetail(id: number): TitleDetail {
  if (removedIds().includes(id)) {
    throw notFound('title');
  }
  const detail = fixtureTitle(id);
  const wanted = mock().wantedVariants;
  return { ...detail, variants: detail.variants.map((v) => ({ ...v, wanted: wanted.has(v.id) })) };
}

/** Waits `ms`, or less when `signal` aborts first; the answer then lands at once, as one already sent would. */
function pause(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener('abort', () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

async function titles(
  platformId: string,
  filters: TitleFilters,
  limit: number,
  offset: number,
  signal?: AbortSignal
): Promise<Paged<TitleGroup>> {
  const ms = delayMs(filters.q ?? '', Math.floor((offset + limit - 1) / TITLES_PAGE));
  await pause(Math.abs(ms), signal);
  if (ms < 0) {
    throw new ApiError('internal', 'Mock search failed.', 500);
  }
  const removed = removedIds();
  const wanted = mock().wantedGroups;
  const all = fixtureTitles(platformId, 240, filters)
    .filter((g) => !removed.includes(g.parent_id))
    .map((g) => ({ ...g, wanted: wanted.get(g.parent_id) ?? g.wanted }));
  return { items: all.slice(offset, offset + limit), total: all.length };
}

/** Lists `up` in its directory, ahead of the files already there. */
function place(which: Watched, up: IncomingFile): IncomingFile {
  const s = mock();
  s.incoming = { ...s.incoming, [which]: [up, ...s.incoming[which].filter((f) => f.file !== up.file)] };
  return up;
}

/** Answers an upload like a server whose writer is busy with the fixture's DAT import. */
function receive(which: Watched, file: string): IncomingFile {
  const importing = fixtureIncomingDats[0]?.file ?? 'a DAT';
  const reason = `Waiting for the DAT import of ${importing} to finish.`;
  return place(which, { file, size: 1, state: 'waiting', reason, job_id: null, progress: null, modified: 0 });
}

/**
 * Queues a binding job for `row`, as `PUT /sources/{id}` does: to `platformId`, or
 * back to the fixture's own binding when `automatic`. Returns the job id.
 */
function bind(row: Source, platformId: string | null, automatic: boolean): number {
  const original = fixtureSources.find((s) => s.id === row.id);
  const target = automatic ? (original?.platform_id ?? null) : platformId;
  const matched = automatic
    ? (original?.matched_count ?? 0)
    : (mockSourcePreview(row).platforms.find((p) => p.platform_id === platformId)?.matched ?? 0);
  const id = nextJobId();
  const now = nowSecs();
  const job = {
    id,
    kind: 'bind_source',
    lane: 'background' as const,
    payload: { source_id: row.id, source_name: row.display_name },
    state: 'running' as const,
    progress: { phase: 'binding', source_id: row.id },
    reason: null,
    created_at: now,
    updated_at: now
  };
  const outcome = { source_id: row.id, platform_id: target, matched, total: row.file_count };
  runJob(job, outcome, BIND_MS, () => {
    const notGames = 'Marked as not a game set. It is not bound automatically.';
    const bound = patchSource(row.id, {
      pending_binding: null,
      platform_id: target,
      state: target ? 'bound' : 'unbound',
      matched_count: matched,
      bind_score: target && row.file_count > 0 ? matched / row.file_count : null,
      reason: target ? null : automatic ? (original?.reason ?? null) : notGames
    });
    emit({ name: 'source.changed', data: { source_id: row.id, state: bound.state, platform_id: bound.platform_id } });
  });
  return id;
}

type SourcePatch = Parameters<Api['updateSource']>[1];

function updateSource(id: number, patch: SourcePatch): SourceUpdated {
  const row = findSource(id);
  const changed: Partial<Source> = {};
  if (patch.seed_policy !== undefined) {
    changed.seed_policy = patch.seed_policy;
  }
  if (patch.state === 'disabled' || patch.state === 'unbound' || patch.state === 'bound') {
    changed.state = patch.state;
  }
  let jobId: number | null = null;
  if (patch.binding === 'automatic') {
    jobId = bind(row, null, true);
    Object.assign(changed, { user_binding: false, pending_binding: { automatic: true, platform_id: null } });
  } else if (patch.platform_id !== undefined) {
    jobId = bind(row, patch.platform_id, false);
    Object.assign(changed, { user_binding: true, pending_binding: { automatic: false, platform_id: patch.platform_id } });
  }
  return { ...patchSource(id, changed), job_id: jobId };
}

/** The mock server: every `api` call answered from fixtures and the state the calls build up. */
export const mockApi: Api = {
  status: () => reply(status),
  wizard: () => reply(wizard),
  scan: () => reply(() => ({ job_id: null }), 400),
  cores: () => reply(() => fixtureCores),
  pause: () =>
    reply(() => {
      const s = mock();
      s.status = { ...s.status, paused: true, pause_reason: 'manual', override: 'paused' };
      return status();
    }),
  resume: () =>
    reply(() => {
      const s = mock();
      s.status = { ...s.status, paused: false, pause_reason: null, override: 'running', waiting: [] };
      return status();
    }),
  jobs: (limit, offset) => reply(() => paged(mock().jobs, limit, offset)),
  recentJobs: (limit, offset) => reply(() => paged(mock().recent, limit, offset)),
  wizardDone: () =>
    reply(() => {
      const s = mock();
      s.wizard = { ...s.wizard, open_on_start: false };
      return wizard();
    }),
  startClient: (kind) =>
    reply(() => {
      const client = status().client;
      const s = mock();
      s.status = { ...s.status, client: client && { ...client, kind, reachable: true } };
      return status();
    }),
  datsIncoming: (limit, offset) => reply(() => paged(mock().incoming.dats, limit, offset)),
  sourcesIncoming: (limit, offset) => reply(() => paged(mock().incoming.sources, limit, offset)),
  settings: () =>
    reply(() => {
      if (mockKnob<boolean>('settingsFail', false)) {
        throw new ApiError('internal', 'The settings could not be read.', 500);
      }
      return mock().settings;
    }),
  putSettings: (patch) =>
    reply(() => {
      const body = wire(patch);
      recordKnob('savedSettings', body);
      const s = mock();
      s.settings = { ...s.settings, ...body };
      return s.settings;
    }),

  platforms: (limit, offset) => reply(() => paged(platforms(), limit, offset)),
  unidentified: (id, offset, limit) =>
    reply(() => paged(Object.hasOwn(fixtureUnidentified, id) ? (fixtureUnidentified[id] ?? []) : [], limit, offset)),
  setPlatform: (id, enabled) =>
    reply(() => {
      findPlatform(id);
      const changed = mock().platforms;
      changed.set(id, { ...changed.get(id), enabled });
      return findPlatform(id);
    }),
  bindPlatformDat: (id, datVersionId) =>
    reply(() => ({ dat_version_id: datVersionId, platform_id: findPlatform(id).id, job_id: nextJobId() })),
  launchCore: (id) => reply(() => ({ core: findPlatform(id).core_dir, file: null })),

  titles,
  title: (id) => reply(() => titleDetail(id)),
  want: (id, variantId) =>
    reply(() => {
      const detail = titleDetail(id);
      const s = mock();
      s.wantedVariants.add(variantId ?? detail.pick_variant_id ?? id);
      s.wantedGroups.set(id, 1);
      return titleDetail(id);
    }),
  unwant: (id) =>
    reply(() => {
      const s = mock();
      for (const v of titleDetail(id).variants) {
        s.wantedVariants.delete(v.id);
      }
      s.wantedGroups.set(id, 0);
      return titleDetail(id);
    }),
  rename: (id) => reply(() => titleDetail(id)),
  launchTitle: (id) => reply(() => ({ core: 'NES', file: `${titleDetail(id).base_name} (USA).nes` })),

  dats: (limit, offset) => reply(() => paged(mock().dats, limit, offset)),
  uploadDat: (file) => reply(() => receive('dats', file.name), 900),
  deleteDat: (id) =>
    reply(() => {
      const s = mock();
      if (!s.dats.some((d) => d.id === id)) {
        throw notFound('DAT version');
      }
      const reason = 'Removed; its games are no longer listed';
      s.dats = s.dats.map((d) => (d.id === id ? { ...d, retired: true, reason } : d));
    }),
  retryRejectedDat: (file) =>
    reply(() => {
      const found = mock().incoming.dats.find((f) => f.file === file && f.state === 'rejected');
      if (!found) {
        throw new ApiError('not_found', `${file} is not a rejected DAT.`, 404);
      }
      return place('dats', { ...found, state: 'waiting', reason: 'Queued.', job_id: nextJobId() });
    }),
  deleteRejectedDat: (file) =>
    reply(() => {
      const s = mock();
      s.incoming = { ...s.incoming, dats: s.incoming.dats.filter((f) => f.file !== file) };
    }),

  sources: (limit, offset) => reply(() => paged(mock().sources, limit, offset)),
  uploadSource: (file) => reply(() => receive('sources', file.name), 900),
  addMagnet: () => reply(() => receive('sources', 'Example magnet.magnet'), 900),
  fetchUrl: (url) =>
    reply(() => {
      if (/^magnet:/i.test(url)) {
        const file = place('sources', {
          file: 'Example magnet.magnet',
          size: 1,
          state: 'waiting',
          reason: 'Queued.',
          job_id: null,
          progress: null,
          modified: 0
        });
        return { token: null, job_id: null, target: 'sources' as const, file };
      }
      if (!/^https?:\/\/[^/@]+/i.test(url)) {
        throw new ApiError('bad_request', 'Only http, https and magnet links are accepted.', 400);
      }
      const { token, id } = startFetch(url);
      return { token, job_id: id, target: null, file: null };
    }, 600),
  cancelFetch: (token) => reply(() => cancelFetch(token), 800),
  updateSource: (id, patch) => reply(() => updateSource(id, patch)),
  deleteSource: (id) =>
    reply(() => {
      findSource(id);
      const s = mock();
      s.sources = s.sources.filter((row) => row.id !== id);
    }),
  source: (id) => reply(() => mockSourceDetail(findSource(id))),
  sourceFiles: (id, opts) => reply(() => mockSourceFilesPage(findSource(id), opts)),
  sourcePreview: (id) => reply(() => mockSourcePreview(findSource(id))),

  downloads: (limit, offset, state) =>
    reply(() => paged(mock().downloads.filter((d) => state === undefined || d.state === state), limit, offset)),
  retryDownload: (id) => reply(() => patchDownload(id, { state: 'queued', error: null })),
  cancelDownload: (id) => reply(() => patchDownload(id, { state: 'cancelled' })),
  imports: (limit, offset) => reply(() => paged(mock().imports, limit, offset)),

  events: (onEvent, onStateChange) => {
    const stream = mockEvents(onEvent, onStateChange);
    return {
      start() {
        stream.start();
        startProgress();
      },
      stop() {
        stream.stop();
      }
    };
  }
};

function patchDownload(id: number, patch: Partial<Download>): Download {
  const s = mock();
  const row = s.downloads.find((d) => d.id === id);
  if (!row) {
    throw notFound('download');
  }
  const next = { ...row, ...patch };
  s.downloads = s.downloads.map((d) => (d.id === id ? next : d));
  return next;
}
