import type {
  DatVersion,
  Download,
  ImportLogEntry,
  Job,
  Platform,
  Settings,
  Source,
  SourceFile,
  SystemStatus,
  TitleDetail,
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

function artFor(platformCoreDir: string, datName: string) {
  const clean = datName.replace(/[&*/:`<>?\\|"]/g, '_');
  const base = `https://thumbnails.libretro.com/${encodeURIComponent(platformCoreDir)}`;
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

export function fixtureTitles(platformId: string, count = 60): TitleGroup[] {
  const platform = fixturePlatforms.find((p) => p.id === platformId);
  const coreDir = platform?.core_dir ?? 'NES';
  return Array.from({ length: count }, (_, i) => {
    const name = `${exampleNames[i % exampleNames.length]} (USA)`;
    return {
      parent_id: i + 1,
      platform_id: platformId,
      base_name: exampleNames[i % exampleNames.length] ?? 'Example Quest',
      pick_name: name,
      variants: 1 + (i % 3),
      have_verified: i % 4 === 0 ? 1 : 0,
      wanted: i % 5 === 0 ? 1 : 0,
      has_pick: true,
      art: artFor(coreDir, name)
    };
  });
}

export function fixtureTitle(id: number): TitleDetail {
  const name = `${exampleNames[id % exampleNames.length]} (USA)`;
  return {
    parent_id: id,
    platform_id: 'nes',
    base_name: exampleNames[id % exampleNames.length] ?? 'Example Quest',
    pick_variant_id: id * 10,
    art: artFor('NES', name),
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
        torrent_files_available: 2,
        roms: [
          {
            id: id * 100,
            name: `${exampleNames[id % exampleNames.length]}.nes`,
            size: 131072,
            status: 'verified',
            file_state: 'verified',
            file_id: id * 1000
          }
        ]
      },
      {
        id: id * 10 + 1,
        name: `${exampleNames[id % exampleNames.length]} (Europe)`,
        regions: ['Europe'],
        languages: ['en', 'fr', 'de'],
        revision: 'Rev 1',
        flags: [],
        is_1g1r_pick: false,
        wanted: false,
        torrent_files_available: 0,
        roms: [
          {
            id: id * 100 + 1,
            name: `${exampleNames[id % exampleNames.length]} (Europe).nes`,
            size: 131072,
            status: 'good',
            file_state: 'unverified',
            file_id: null
          }
        ]
      }
    ]
  };
}

export const fixtureStatus: SystemStatus = {
  version: '0.1.0-fixture',
  uptime: 4521,
  client_kind: 'rtorrent',
  client_reachable: true,
  corename: 'FCEUmm',
  paused: false,
  disk_free: 12_400_000_000,
  rss: 41_000_000
};

export const fixtureWizard: WizardStatus = {
  steps: { paths: true, dats: true, client: false, sources: false }
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
    game_count: 240
  },
  {
    id: 2,
    platform_id: null,
    dat_name: 'Unbound Sample DAT',
    version: '20260102',
    source_file: 'unbound-sample.dat',
    loaded_at: 1_770_003_600,
    superseded_by: null,
    game_count: 88
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
    seed_policy: 'ratio:1.0',
    file_count: 240,
    matched_count: 238,
    total_size: 900_000_000,
    client_status: 'seeding',
    added_at: 1_770_010_000
  },
  {
    id: 2,
    infohash: '1'.repeat(40),
    display_name: 'Example bundle two',
    origin_file: 'example-bundle-two.torrent',
    platform_id: null,
    bind_score: null,
    state: 'unbound',
    seed_policy: 'none',
    file_count: 60,
    matched_count: 12,
    total_size: 300_000_000,
    client_status: null,
    added_at: 1_770_020_000
  }
];

export function fixtureSourceFiles(): SourceFile[] {
  return [
    { file_index: 0, path: 'Example Quest (USA).nes', size: 131072, matched_rom_name: 'Example Quest (USA).nes' },
    { file_index: 1, path: 'Sample Racer (USA).nes', size: 262144, matched_rom_name: null }
  ];
}

export const fixtureDownloads: Download[] = [
  {
    id: 1,
    title_id: 1,
    title_name: 'Example Quest (USA)',
    source_id: 1,
    state: 'transferring',
    progress: 0.42,
    error: null,
    created_at: 1_770_030_000,
    updated_at: 1_770_030_500
  },
  {
    id: 2,
    title_id: 2,
    title_name: 'Sample Racer (USA)',
    source_id: 1,
    state: 'checking',
    progress: 1,
    error: null,
    created_at: 1_770_030_100,
    updated_at: 1_770_030_600
  },
  {
    id: 3,
    title_id: 3,
    title_name: 'Fixture Fighters (USA)',
    source_id: 2,
    state: 'failed',
    progress: 0.1,
    error: 'client unreachable',
    created_at: 1_770_030_200,
    updated_at: 1_770_030_700
  }
];

export const fixtureImports: ImportLogEntry[] = [
  { id: 1, at: 1_770_031_000, download_id: 1, file_id: 10, action: 'placed', detail: '{}' },
  { id: 2, at: 1_770_031_100, download_id: null, file_id: 11, action: 'skipped_existing', detail: '{}' }
];

export const fixtureJobs: Job[] = [
  {
    id: 1,
    kind: 'scan',
    state: 'running',
    progress: { scanned: 120, total: 240 },
    created_at: 1_770_032_000,
    updated_at: 1_770_032_500
  }
];

export const fixtureSettings: Settings = {
  games_root: '/media/fat/games',
  api_key_set: false,
  scan_interval_minutes: 60
};
