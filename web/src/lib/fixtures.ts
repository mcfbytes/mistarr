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
  WizardStatus
} from './types';

export const fixturePlatforms: Platform[] = [
  {
    id: 'nes',
    name: 'Nintendo Entertainment System',
    core_dir: 'NES',
    kind: 'cartridge',
    core_present: true,
    enabled: true,
    counts: { titles: 240, have: 180, wanted: 12, unverified: 4 }
  },
  {
    id: 'megadrive',
    name: 'Sega Mega Drive',
    core_dir: 'Genesis',
    kind: 'cartridge',
    core_present: true,
    enabled: true,
    counts: { titles: 310, have: 90, wanted: 30, unverified: 1 }
  },
  {
    id: 'psx',
    name: 'PlayStation',
    core_dir: 'PSX',
    kind: 'disc',
    core_present: false,
    enabled: true,
    counts: { titles: 420, have: 0, wanted: 0, unverified: 0 }
  },
  {
    id: 'amiga',
    name: 'Commodore Amiga',
    core_dir: 'Amiga',
    kind: 'computer',
    core_present: false,
    enabled: true,
    counts: { titles: 150, have: 0, wanted: 0, unverified: 0 }
  }
];

function artFor(playlist: string, name: string) {
  const clean = name.replace(/[&*/:`<>?\\|"]/g, '_');
  const base = `https://thumbnails.libretro.com/${encodeURIComponent(playlist)}`;
  return {
    boxart: `${base}/Named_Boxarts/${encodeURIComponent(clean)}.png`,
    title: `${base}/Named_Titles/${encodeURIComponent(clean)}.png`,
    snap: `${base}/Named_Snaps/${encodeURIComponent(clean)}.png`
  };
}

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
      art: artFor(coreDir, name)
    });
  }
  return rows;
}

export function fixtureTitle(id: number): TitleDetail {
  const base = exampleNames[id % exampleNames.length] ?? 'Example Quest';
  const name = `${base} (USA)`;
  return {
    parent_id: id,
    platform_id: 'nes',
    base_name: base,
    pick_variant_id: id * 10,
    art: artFor('Nintendo - Nintendo Entertainment System', name),
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
        torrent_files_available: 0,
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
  rss_bytes: 41_000_000,
  launch: 'ready'
};

export const fixtureWizard: WizardStatus = {
  paths: true,
  dats: true,
  client: false,
  sources: false,
  open_on_start: false
};

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
    retired: false
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
    retired: false
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
    job_id: 7,
    progress: { members: 1, done: 0, games: 1200 },
    modified: 1_770_040_000
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
      confidence: 'name'
    },
    { file_index: 1, path: 'Sample Racer (USA).nes', size: 262144, rom_id: null, rom_name: null, title_id: null, confidence: null }
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
    progress: { members: 1, done: 0, games: 1200 },
    reason: null,
    created_at: 1_770_032_000,
    updated_at: 1_770_032_500
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

export const fixtureSettings: Settings = {
  client: { kind: 'auto', url: '', remote_path_map: [] },
  limits: { down_kbps_menu: 0, down_kbps_core: 512, up_kbps_menu: 0, up_kbps_core: 64 },
  prefs: {
    regions: ['USA', 'World', 'Europe', 'Japan'],
    languages: ['En'],
    prefer_latest_revision: true,
    hide: ['bios', 'beta', 'proto', 'demo', 'sample', 'program'],
    launch: true
  }
};
