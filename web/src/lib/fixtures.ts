import { BROWSE_FLAGS } from './types';
import type {
  CoresResult,
  DatVersion,
  Download,
  ImportLogEntry,
  IncomingFile,
  Job,
  Platform,
  Settings,
  Source,
  SourceFile,
  SystemStatus,
  TitleDetail,
  TitleFilters,
  TitleGroup,
  UnidentifiedFile,
  WizardStatus
} from './types';

/**
 * Mock mode's scenario, from `?mock=` in the page URL: `idle` shows no work,
 * and `showcase` an idle, fully set up board with more platforms, for the README.
 */
export function mockScenario(): string {
  return new URLSearchParams(globalThis.location.search).get('mock') ?? 'busy';
}

export const fixturePlatforms: Platform[] = [
  {
    id: 'nes',
    name: 'Nintendo Entertainment System',
    core_dir: 'NES',
    kind: 'cartridge',
    core_present: true,
    enabled: true,
    counts: { titles: 240, have: 180, wanted: 12, unmatched_files: 4, unidentified_files: 0, failing_check: 0, partial: 0 }
  },
  {
    id: 'megadrive',
    name: 'Sega Mega Drive',
    core_dir: 'Genesis',
    kind: 'cartridge',
    core_present: true,
    enabled: true,
    counts: { titles: 310, have: 90, wanted: 30, unmatched_files: 1, unidentified_files: 0, failing_check: 0, partial: 0 }
  },
  {
    id: 'psx',
    name: 'PlayStation',
    core_dir: 'PSX',
    kind: 'disc',
    core_present: false,
    enabled: true,
    counts: { titles: 420, have: 0, wanted: 0, unmatched_files: 0, unidentified_files: 0, failing_check: 0, partial: 0 }
  },
  {
    id: 'saturn',
    name: 'Sega Saturn',
    core_dir: 'Saturn',
    kind: 'disc',
    core_present: true,
    enabled: true,
    counts: { titles: 60, have: 12, wanted: 0, unmatched_files: 1, unidentified_files: 3, failing_check: 0, partial: 0 }
  },
  {
    id: 'amiga',
    name: 'Commodore Amiga',
    core_dir: 'Amiga',
    kind: 'computer',
    core_present: false,
    enabled: true,
    counts: { titles: 150, have: 0, wanted: 0, unmatched_files: 0, unidentified_files: 0, failing_check: 0, partial: 0 }
  },
  {
    id: 'arcade',
    name: 'Arcade',
    core_dir: 'mame',
    kind: 'arcade',
    core_present: true,
    enabled: true,
    counts: { titles: 1298, have: 978, wanted: 0, unmatched_files: 0, unidentified_files: 0, failing_check: 14, partial: 6 }
  }
];

const counts = (titles: number, have: number, wanted: number, unmatched = 0): Platform['counts'] => ({
  titles,
  have,
  wanted,
  unmatched_files: unmatched,
  unidentified_files: 0,
  failing_check: 0,
  partial: 0
});

/** Present platforms added in the `showcase` scenario, beside the fixture set. */
const showcaseExtra: Platform[] = [
  { id: 'snes', name: 'Super Nintendo Entertainment System', core_dir: 'SNES', kind: 'cartridge', core_present: true, enabled: true, counts: counts(1460, 612, 8, 2) },
  { id: 'n64', name: 'Nintendo 64', core_dir: 'N64', kind: 'cartridge', core_present: true, enabled: true, counts: counts(410, 38, 3) },
  { id: 'gba', name: 'Game Boy Advance', core_dir: 'GBA', kind: 'cartridge', core_present: true, enabled: true, counts: counts(1530, 204, 5, 1) },
  { id: 'gb', name: 'Game Boy', core_dir: 'GAMEBOY', kind: 'cartridge', core_present: true, enabled: true, counts: counts(820, 117, 0) }
];

/** The platforms mock mode lists for the current scenario. */
export function scenarioPlatforms(): Platform[] {
  if (mockScenario() !== 'showcase') {
    return fixturePlatforms;
  }
  const [nes, megadrive, psx, saturn, amiga, arcade] = fixturePlatforms;
  const [snes, n64, gba, gb] = showcaseExtra;
  return [nes, snes, n64, megadrive, gba, gb, saturn, arcade, psx, amiga].filter((p): p is Platform => p !== undefined);
}

function artFor(playlist: string, name: string) {
  const clean = name.replace(/[&*/:`<>?\\|"]/g, '_');
  const base = `https://thumbnails.libretro.com/${encodeURIComponent(playlist)}`;
  return {
    boxart: `${base}/Named_Boxarts/${encodeURIComponent(clean)}.png`,
    title: `${base}/Named_Titles/${encodeURIComponent(clean)}.png`,
    snap: `${base}/Named_Snaps/${encodeURIComponent(clean)}.png`
  };
}

/** Titles whose id ends in 9 have no cover URL, as a title the thumbnail server lacks. */
const noArt = (id: number): boolean => id % 10 === 9;

const exampleNames = [
  'Example Quest',
  'Sample Racer',
  'Fixture Fighters',
  'Placeholder Park',
  'Synthetic Story',
  'Test Tactics',
  'Mock Manor',
  'Demo Dungeon',
  'Trial Trail',
  'Stub Squad'
];

/** Default `prefs.hide` flags, mirroring the server's fixture entries. */
const HIDDEN_FLAGS = ['bios', 'beta'];

function flagsFor(i: number): string[] {
  if (i % 11 === 0) {
    return ['bios'];
  }
  if (i % 13 === 0) {
    return ['beta'];
  }
  return [];
}

export function fixtureTitles(platformId: string, count = 60, filters: TitleFilters = {}): TitleGroup[] {
  const platform = fixturePlatforms.find((p) => p.id === platformId);
  const coreDir = platform?.core_dir ?? 'NES';
  const showHidden = filters.hidden === 'show';
  const required = (filters.flags ?? '')
    .split(',')
    .map((f) => f.trim().toLowerCase())
    .filter((f) => f.length > 0);
  const q = filters.q?.trim().toLowerCase() ?? '';
  const rows: TitleGroup[] = [];
  for (let i = 0; i < count; i += 1) {
    const flags = flagsFor(i);
    if (!showHidden && flags.some((f) => HIDDEN_FLAGS.includes(f))) {
      continue;
    }
    if (required.length > 0 && !required.every((f) => flags.includes(f))) {
      continue;
    }
    const base = exampleNames[i % exampleNames.length] ?? 'Example Quest';
    if (q && !base.toLowerCase().includes(q)) {
      continue;
    }
    const name = `${base} (USA)`;
    rows.push({
      parent_id: i + 1,
      platform_id: platformId,
      base_name: base,
      name,
      pick_id: i + 1,
      pick_name: name,
      variants: 1 + (i % 3),
      have_verified: i % 4 === 0 ? 1 : 0,
      wanted: i % 5 === 0 ? 1 : 0,
      has_pick: true,
      bios: flags.includes('bios'),
      art: noArt(i + 1) ? null : artFor(coreDir, name)
    });
  }
  return rows;
}

/**
 * Mock group ids the mock server no longer lists, read from localStorage
 * `mistarr.mockRemovedIds` as a JSON array of numbers, standing in for rows a
 * scan or an import moves out of the browsed list.
 */
export function mockRemovedIds(): number[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem('mistarr.mockRemovedIds') ?? '[]');
    return Array.isArray(parsed) ? parsed.filter((v): v is number => typeof v === 'number') : [];
  } catch {
    // Storage blocked or the value is not JSON: nothing removed.
    return [];
  }
}

/**
 * Mock latency in ms of page `page` of a title search for `q`, read from
 * localStorage `mistarr.mockDelayMs`: a number for every request, or an object
 * of numbers keyed `q#page` or `q`, with `*` as the default. A negative value
 * makes the request fail.
 */
export function mockDelayMs(q: string, page: number): number {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem('mistarr.mockDelayMs') ?? '0');
    if (typeof parsed === 'number') {
      return parsed;
    }
    if (parsed !== null && typeof parsed === 'object') {
      const byQuery = parsed as Record<string, unknown>;
      const ms = byQuery[`${q}#${page}`] ?? byQuery[q] ?? byQuery['*'];
      return typeof ms === 'number' ? ms : 0;
    }
  } catch {
    // Storage blocked or the value is not JSON: no delay.
  }
  return 0;
}

/**
 * Extra present, enabled mock platforms named by their ids, read from
 * localStorage `mistarr.mockPlatformIds` as a JSON array of strings.
 */
export function mockExtraPlatforms(): Platform[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem('mistarr.mockPlatformIds') ?? '[]');
    if (Array.isArray(parsed)) {
      return parsed
        .filter((id): id is string => typeof id === 'string')
        .map((id) => ({
          id,
          name: id,
          core_dir: id,
          kind: 'cartridge',
          core_present: true,
          enabled: true,
          counts: { titles: 0, have: 0, wanted: 0, unmatched_files: 0, unidentified_files: 0, failing_check: 0, partial: 0 }
        }));
    }
  } catch {
    // Storage blocked or the value is not JSON: no extra platforms.
  }
  return [];
}

export function fixtureTitle(id: number): TitleDetail {
  const base = exampleNames[id % exampleNames.length] ?? 'Example Quest';
  const name = `${base} (USA)`;
  const detail: TitleDetail = {
    parent_id: id,
    platform_id: 'nes',
    base_name: base,
    pick_variant_id: id * 10,
    art: noArt(id) ? null : artFor('Nintendo - Nintendo Entertainment System', name),
    variants: [
      {
        id: id * 10,
        name,
        regions: ['USA'],
        languages: ['en'],
        revision: null,
        flags: [],
        is_1g1r_pick: true,
        wanted: false,
        retired: false,
        inferred: false,
        dat_version_id: 1,
        torrent_files_available: 2,
        availability: [
          {
            source_id: 1,
            source_name: 'Example Pack',
            file_index: 0,
            path: `NES/${base} (USA).nes`,
            rom_id: id * 100,
            confidence: 'name'
          },
          {
            source_id: 2,
            source_name: 'examplepack1.0',
            file_index: 3,
            path: 'example.nes',
            rom_id: id * 100,
            confidence: 'fuzzy'
          }
        ],
        roms: [
          {
            id: id * 100,
            name: `${base}.nes`,
            size: 131072,
            crc32: 'deadbeef',
            md5: null,
            sha1: null,
            status: 'verified',
            file_state: 'verified',
            file_id: id * 1000,
            file_path: `NES/${base}.nes`
          }
        ]
      },
      {
        id: id * 10 + 1,
        name: `${base} (Europe)`,
        regions: ['Europe'],
        languages: ['en', 'fr', 'de'],
        revision: 'Rev 1',
        flags: [],
        is_1g1r_pick: false,
        wanted: false,
        retired: false,
        inferred: false,
        dat_version_id: 1,
        torrent_files_available: 1,
        availability: [
          {
            source_id: 2,
            source_name: 'examplepack1.0',
            file_index: 3,
            path: 'example.nes',
            rom_id: id * 100 + 1,
            confidence: 'fuzzy'
          }
        ],
        roms: [
          {
            id: id * 100 + 1,
            name: `${base} (Europe).nes`,
            size: 131072,
            crc32: 'baadf00d',
            md5: null,
            sha1: null,
            status: 'good',
            file_state: 'unverified',
            file_id: null,
            file_path: null
          }
        ]
      },
      {
        id: id * 10 + 2,
        name: `${base} System (BIOS)`,
        regions: ['World'],
        languages: [],
        revision: null,
        flags: ['bios'],
        is_1g1r_pick: false,
        wanted: false,
        retired: false,
        inferred: false,
        dat_version_id: 1,
        torrent_files_available: 0,
        availability: [],
        roms: [
          {
            id: id * 100 + 2,
            name: `${base} System (BIOS).bin`,
            size: 8192,
            crc32: '0badc0de',
            md5: null,
            sha1: null,
            status: 'good',
            file_state: 'verified',
            file_id: id * 1000 + 2,
            file_path: `NES/${base} System (BIOS).bin`
          }
        ]
      }
    ]
  };
  if (mockScenario() !== 'showcase') {
    return detail;
  }
  return { ...detail, variants: detail.variants.filter((v) => !v.flags.includes('bios')) };
}

export const fixtureStatus: SystemStatus = {
  version: '0.1.0-fixture',
  uptime_secs: 4521,
  client: {
    kind: 'rtorrent',
    url: 'scgi://127.0.0.1:5000',
    reachable: true,
    version: null,
    rtorrent_on_path: true,
    transmission_on_path: false,
    transmission_service: false,
    transmission_opt_in: false,
    checked_at: 1_770_000_000
  },
  corename: 'FCEUmm',
  paused: true,
  pause_reason: 'core',
  override: null,
  waiting: [{ id: 2, kind: 'scan', state: 'queued', detail: 'nes' }],
  disk_free_bytes: 12_400_000_000,
  dats_dir: '/media/fat/mistarr/dats',
  rss_bytes: 41_000_000,
  launch: 'ready',
  chd_decode_bytes_per_sec: 1_200_000
};

/** The status mock mode reports: the fixture's, or at the menu with nothing held for `showcase`. */
export function scenarioStatus(): SystemStatus {
  return mockScenario() === 'showcase'
    ? { ...fixtureStatus, version: '0.1.0', corename: 'MENU', paused: false, pause_reason: null, waiting: [] }
    : fixtureStatus;
}

/** Browse's flag choices: the README showcase leaves out the `pirate` DAT tag. */
export function scenarioBrowseFlags(): readonly string[] {
  return mockScenario() === 'showcase' ? BROWSE_FLAGS.filter((f) => f !== 'pirate') : BROWSE_FLAGS;
}

/** Files not identified, by platform, for the Platforms card. */
export const fixtureUnidentified: Record<string, UnidentifiedFile[]> = {
  saturn: [
    { rel_path: 'Saturn/Example Orbit (Japan)/Example Orbit (Japan).chd', size: 412_000_000, reason: 'off' },
    { rel_path: 'Saturn/Sample Rally (Europe)/Sample Rally (Europe).chd', size: 380_000_000, reason: 'cooked' },
    { rel_path: 'Saturn/Test Pattern Disc/Test Pattern Disc.chd', size: 2_000_000, reason: 'no_layout' }
  ]
};

export const fixtureWizard: WizardStatus = {
  paths: true,
  dats: true,
  client: false,
  sources: false,
  open_on_start: false
};

/** The wizard state mock mode reports: every step done for `showcase`. */
export function scenarioWizard(): WizardStatus {
  return mockScenario() === 'showcase'
    ? { ...fixtureWizard, client: true, sources: true }
    : fixtureWizard;
}

export const fixtureCores: CoresResult = {
  platforms: fixturePlatforms.filter((p) => p.core_present).map((p) => p.id),
  arcade_job_id: null
};

export const fixtureDats: DatVersion[] = [
  {
    id: 1,
    platform_id: 'nes',
    dat_name: 'Example Console DAT',
    version: '20260101',
    source_file: 'example-console.dat',
    loaded_at: 1_770_000_000,
    superseded_by: null,
    game_count: 240,
    retired: false,
    family: 'example console dat',
    reason: null,
    suggested: []
  },
  {
    id: 5,
    platform_id: 'nes',
    dat_name: 'mistarr samples - Nintendo Entertainment System (Headered)',
    version: '1',
    source_file: 'mistarr samples - Nintendo Entertainment System (Headered).dat',
    loaded_at: 1_770_010_800,
    superseded_by: null,
    game_count: 4,
    retired: false,
    family: 'mistarr samples - nintendo entertainment system',
    reason: null,
    suggested: []
  },
  {
    id: 3,
    platform_id: 'megadrive',
    dat_name: 'Example Vendor - Mega Drive - Genesis (DB Export)',
    version: '20260101-000000',
    source_file: 'Example Vendor - Mega Drive - Genesis (DB Export) (20260101-000000).zip',
    loaded_at: 1_770_007_200,
    superseded_by: null,
    game_count: 310,
    retired: false,
    family: 'example vendor - mega drive - genesis',
    reason: null,
    suggested: []
  },
  {
    id: 4,
    platform_id: 'megadrive',
    dat_name: 'Example Vendor - Mega Drive - Genesis',
    version: '20251201-000000',
    source_file: 'Example Vendor - Mega Drive - Genesis (20251201-000000).dat',
    loaded_at: 1_769_000_000,
    superseded_by: 3,
    game_count: 305,
    retired: false,
    family: 'example vendor - mega drive - genesis',
    reason: 'Replaced by Example Vendor - Mega Drive - Genesis (DB Export) version 20260101-000000',
    suggested: []
  },
  {
    id: 2,
    platform_id: null,
    dat_name: 'Unbound Sample DAT',
    version: '20260102',
    source_file: 'unbound-sample.dat',
    loaded_at: 1_770_003_600,
    superseded_by: null,
    game_count: 88,
    retired: false,
    family: 'unbound sample dat',
    reason: null,
    suggested: []
  }
];

export const fixtureSources: Source[] = [
  {
    id: 1,
    infohash: '0'.repeat(40),
    display_name: 'Example bundle one',
    origin_file: 'example-bundle-one.torrent',
    platform_id: 'nes',
    bind_score: 0.94,
    state: 'bound',
    reason: null,
    seed_policy: 'ratio:1.0',
    file_count: 240,
    matched_count: 238,
    total_size: 900_000_000,
    client_id: 'abc123',
    added_at: 1_770_010_000,
    suggested_platform_id: 'nes'
  },
  {
    id: 2,
    infohash: '1'.repeat(40),
    display_name: 'Example bundle two',
    origin_file: 'example-bundle-two.torrent',
    platform_id: null,
    bind_score: null,
    state: 'unbound',
    reason: 'Looks like Game Boy. No DAT for it is loaded yet; it binds once one loads.',
    seed_policy: 'none',
    file_count: 60,
    matched_count: 12,
    total_size: 300_000_000,
    client_id: null,
    added_at: 1_770_020_000,
    suggested_platform_id: 'gb'
  }
];

export const fixtureIncomingDats: IncomingFile[] = [
  {
    file: 'Example Console (20260101).zip',
    size: 180_000,
    state: 'importing',
    reason: null,
    job_id: 1,
    progress: { members: 1, done: 0, games: 1200 },
    modified: 1_770_040_000
  },
  {
    file: 'Example Handheld (20260101).xml',
    size: 4_096,
    state: 'rejected',
    reason:
      'root element is <softwarelist>; expected a Logiqx DAT (<datafile>) or a No-Intro DB export (<header> followed by <datafile>)',
    job_id: null,
    progress: null,
    modified: 1_770_039_500
  },
  {
    file: 'notes.txt',
    size: 12,
    state: 'rejected',
    reason: 'not a DAT: expected a .dat, .xml or .zip file',
    job_id: null,
    progress: null,
    modified: 1_770_039_000
  }
];

export const fixtureIncomingSources: IncomingFile[] = [
  {
    file: 'Example bundle two.torrent',
    size: 52_000,
    state: 'waiting',
    reason: 'Waiting for the DAT import of Example Console (20260101).zip to finish.',
    job_id: 4,
    progress: null,
    modified: 1_770_040_500
  },
  {
    file: 'Example bundle three.torrent',
    size: 40_000,
    state: 'waiting',
    reason: 'Waiting for the file to stop changing.',
    job_id: null,
    progress: null,
    modified: 1_770_041_000
  }
];

export function fixtureSourceFiles(): SourceFile[] {
  return [
    {
      file_index: 0,
      path: 'Example Quest (USA).nes',
      size: 131072,
      rom_id: 1,
      rom_name: 'Example Quest (USA).nes',
      title_id: 1,
      confidence: 'name',
      candidates: []
    },
    {
      file_index: 1,
      path: 'Sample Racer (USA).nes',
      size: 262144,
      rom_id: null,
      rom_name: null,
      title_id: null,
      confidence: null,
      candidates: [{ rom_id: 2, rom_name: 'Sample Racer (World).nes', title_id: 2, confidence: 'fuzzy' }]
    }
  ];
}

export const fixtureDownloads: Download[] = [
  {
    id: 1,
    title_id: 1,
    title_name: 'Example Quest (USA)',
    platform_id: 'nes',
    rom_id: 10,
    rom_name: 'Example Quest (USA).nes',
    size: 131072,
    source_id: 1,
    file_index: 0,
    state: 'transferring',
    progress: 0.42,
    staged_path: null,
    error: null,
    created_at: 1_770_030_000,
    updated_at: 1_770_030_500
  },
  {
    id: 2,
    title_id: 2,
    title_name: 'Sample Racer (USA)',
    platform_id: 'nes',
    rom_id: 11,
    rom_name: 'Sample Racer (USA).nes',
    size: 262144,
    source_id: 1,
    file_index: 1,
    state: 'checking',
    progress: 1,
    staged_path: null,
    error: null,
    created_at: 1_770_030_100,
    updated_at: 1_770_030_600
  },
  {
    id: 3,
    title_id: 3,
    title_name: 'Fixture Fighters (USA)',
    platform_id: 'nes',
    rom_id: 12,
    rom_name: 'Fixture Fighters (USA).nes',
    size: 65536,
    source_id: 2,
    file_index: 0,
    state: 'failed',
    progress: 0.1,
    staged_path: null,
    error: 'client unreachable',
    created_at: 1_770_030_200,
    updated_at: 1_770_030_700
  }
];

export const fixtureImports: ImportLogEntry[] = [
  { id: 1, at: 1_770_031_000, download_id: 1, file_id: 10, action: 'placed', detail: { rel_path: 'nes/Example Quest (USA).nes' } },
  { id: 2, at: 1_770_031_100, download_id: null, file_id: 11, action: 'skipped_existing', detail: {} }
];

export const fixtureJobs: Job[] = [
  {
    id: 1,
    kind: 'dat_import',
    lane: 'background',
    payload: { path: '/media/fat/mistarr/dats/Example Console (20260101).zip' },
    state: 'running',
    progress: {
      file: 'Example Console (20260101).zip',
      members: 1,
      done: 0,
      games: 4_120,
      phase: 'reading',
      bytes_read: 5_200_000,
      bytes_total: 18_400_000
    },
    reason: null,
    created_at: 1_770_032_000,
    updated_at: 1_770_032_500
  },
  {
    id: 4,
    kind: 'source_import',
    lane: 'background',
    payload: { path: '/media/fat/mistarr/sources/Example bundle two.torrent' },
    state: 'queued',
    progress: null,
    reason: 'Waiting for the DAT import of Example Console (20260101).zip to finish.',
    created_at: 1_770_032_200,
    updated_at: 1_770_032_200
  },
  {
    id: 2,
    kind: 'scan',
    lane: 'heavy',
    payload: { platform_id: 'nes' },
    state: 'queued',
    progress: null,
    reason: 'Paused while FCEUmm is running',
    created_at: 1_770_032_100,
    updated_at: 1_770_032_100
  }
];

export const fixtureRecentJobs: Job[] = [
  {
    id: 5,
    kind: 'dat_import',
    lane: 'background',
    payload: { path: '/media/fat/mistarr/dats/Example Handheld (20260101).xml' },
    state: 'failed',
    progress: { error: 'root element is <softwarelist>; expected a Logiqx DAT' },
    reason: null,
    created_at: 1_770_031_500,
    updated_at: 1_770_031_560
  },
  {
    id: 6,
    kind: 'chd_tracks',
    lane: 'heavy',
    payload: {},
    state: 'done',
    progress: { done: 5, total: 5, verified: 3, unmatched: 1, not_identified: 1 },
    reason: null,
    created_at: 1_770_031_030,
    updated_at: 1_770_031_900
  },
  {
    id: 3,
    kind: 'scan',
    lane: 'heavy',
    payload: { platform_id: 'megadrive' },
    state: 'done',
    progress: { platform_id: 'megadrive', done: 1, total: 1, matched: 90, unmatched: 1 },
    reason: null,
    created_at: 1_770_031_000,
    updated_at: 1_770_031_020
  }
];

export const fixtureSettings: Settings = {
  client: { kind: 'auto', url: '', remote_path_map: [] },
  limits: { down_kbps_menu: 0, down_kbps_core: 512, up_kbps_menu: 0, up_kbps_core: 64 },
  prefs: {
    regions: ['USA', 'World', 'Europe', 'Japan'],
    languages: ['En'],
    prefer_latest_revision: true,
    hide: ['bios', 'beta', 'proto', 'demo', 'sample', 'program'],
    launch: true
  },
  scan: { chd_tracks: false }
};
