export type ClientKind = 'transmission' | 'rtorrent';

export interface ClientStatus {
  kind: ClientKind | null;
  url: string | null;
  reachable: boolean;
  version: string | null;
  rtorrent_on_path: boolean;
  checked_at: number;
}

export type PauseReason = 'core' | 'manual' | null;
export type Override = 'paused' | 'running' | null;

export interface SystemStatus {
  version: string;
  uptime_secs: number;
  client: ClientStatus | null;
  corename: string | null;
  paused: boolean;
  pause_reason: PauseReason;
  override: Override;
  disk_free_bytes: number | null;
  rss_bytes: number | null;
}

export interface WizardStatus {
  paths: boolean;
  dats: boolean;
  client: boolean;
  sources: boolean;
  open_on_start: boolean;
}

export interface CoresResult {
  platforms: string[];
  arcade_job_id: number | null;
}

export type PlatformKind = 'cartridge' | 'disc' | 'computer' | 'romset' | 'arcade' | 'other';

export interface PlatformCounts {
  titles: number;
  have: number;
  wanted: number;
  unverified: number;
}

export interface Platform {
  id: string;
  name: string;
  core_dir: string;
  kind: PlatformKind;
  core_present: boolean;
  enabled: boolean;
  counts: PlatformCounts;
}

export type HaveFilter = 'any' | 'yes' | 'no';

export interface TitleFilters {
  q?: string;
  have?: HaveFilter;
  wanted?: HaveFilter;
  region?: string;
  flags?: string;
  hidden?: 'hide' | 'show';
  sort?: 'name' | 'have' | 'recent';
}

/** Flags the "require" multi-select offers. */
export const BROWSE_FLAGS = ['bios', 'beta', 'proto', 'demo', 'sample', 'program', 'unl', 'pirate'] as const;

export interface ArtUrls {
  boxart: string;
  title: string;
  snap: string;
}

export interface TitleGroup {
  parent_id: number;
  platform_id: string;
  base_name: string;
  name: string;
  pick_id: number | null;
  pick_name: string | null;
  variants: number;
  have_verified: number;
  wanted: number;
  has_pick: boolean;
  art: ArtUrls | null;
}

export type RomStatus = 'good' | 'baddump' | 'nodump' | 'verified';

export type FileState = 'verified' | 'unverified' | 'misnamed' | 'bad' | 'pending';

export interface TitleRom {
  id: number;
  name: string;
  size: number;
  crc32: string | null;
  md5: string | null;
  sha1: string | null;
  status: RomStatus;
  file_id: number | null;
  file_state: FileState | null;
  file_path: string | null;
}

export interface TitleVariant {
  id: number;
  name: string;
  regions: string[];
  languages: string[];
  revision: string | null;
  flags: string[];
  is_1g1r_pick: boolean;
  wanted: boolean;
  retired: boolean;
  inferred: boolean;
  dat_version_id: number;
  roms: TitleRom[];
  torrent_files_available: number;
}

export interface TitleDetail {
  parent_id: number;
  platform_id: string;
  base_name: string;
  pick_variant_id: number | null;
  art: ArtUrls | null;
  variants: TitleVariant[];
}

export interface DatVersion {
  id: number;
  platform_id: string | null;
  dat_name: string;
  version: string;
  source_file: string;
  loaded_at: number;
  superseded_by: number | null;
  game_count: number;
  retired: boolean;
}

export type SeedPolicy = 'none' | 'client' | `ratio:${string}`;

export type SourceState = 'resolving' | 'unbound' | 'bound' | 'disabled';

export interface Source {
  id: number;
  infohash: string;
  display_name: string;
  origin_file: string;
  platform_id: string | null;
  bind_score: number | null;
  state: SourceState;
  reason: string | null;
  seed_policy: string;
  file_count: number;
  matched_count: number;
  total_size: number;
  client_id: string | null;
  added_at: number;
}

export type SourceFileConfidence = 'name' | 'size' | null;

export interface SourceFile {
  file_index: number;
  path: string;
  size: number;
  rom_id: number | null;
  rom_name: string | null;
  title_id: number | null;
  confidence: SourceFileConfidence;
}

export type DownloadState =
  | 'wanted'
  | 'queued'
  | 'transferring'
  | 'checking'
  | 'importing'
  | 'done'
  | 'bad'
  | 'failed'
  | 'cancelled';

export interface Download {
  id: number;
  title_id: number;
  title_name: string;
  platform_id: string;
  rom_id: number;
  rom_name: string;
  size: number;
  source_id: number | null;
  file_index: number | null;
  state: DownloadState;
  progress: number;
  staged_path: string | null;
  error: string | null;
  created_at: number;
  updated_at: number;
}

export type ImportAction = 'placed' | 'replaced' | 'quarantined' | 'skipped_existing' | 'renamed';

export interface ImportLogEntry {
  id: number;
  at: number;
  download_id: number | null;
  file_id: number | null;
  action: ImportAction;
  detail: Record<string, unknown>;
}

export type JobState = 'queued' | 'running' | 'paused' | 'done' | 'failed';

export interface Job {
  id: number;
  kind: string;
  payload: Record<string, unknown>;
  state: JobState;
  progress: Record<string, unknown> | null;
  created_at: number;
  updated_at: number;
}

export interface PathMapping {
  remote: string;
  local: string;
}

export type ClientChoice = 'auto' | 'transmission' | 'rtorrent';

export interface ClientSettings {
  kind: ClientChoice;
  url: string;
  remote_path_map: PathMapping[];
}

export interface LimitsSettings {
  down_kbps_menu: number;
  down_kbps_core: number;
  up_kbps_menu: number;
  up_kbps_core: number;
}

export interface PrefsSettings {
  regions: string[];
  languages: string[];
  prefer_latest_revision: boolean;
  hide: string[];
}

export interface Settings {
  client: ClientSettings;
  limits: LimitsSettings;
  prefs: PrefsSettings;
}

export interface SettingsPatch {
  client?: ClientSettings;
  limits?: LimitsSettings;
  prefs?: PrefsSettings;
}

export interface Paged<T> {
  items: T[];
  total: number;
}

export interface ApiErrorBody {
  error: { code: string; message: string };
}

export interface SseStatusEvent {
  name: 'status';
  data: SystemStatus;
}

export interface SseJobProgressEvent {
  name: 'job.progress';
  data: { id: number; kind: string; state: JobState; progress: Record<string, unknown> };
}

export interface SseDatLoadedEvent {
  name: 'dat.loaded';
  data: { dat_version_id: number; file: string; platform_id: string | null };
}

export interface SseDatRejectedEvent {
  name: 'dat.rejected';
  data: { file: string; reason: string };
}

export interface SseSourceEvent {
  name: 'source.changed';
  data: { source_id: number; state: SourceState; platform_id: string | null };
}

export interface SseDownloadEvent {
  name: 'download.changed';
  data: { download_id: number; state: DownloadState; progress: number };
}

export interface SseImportEvent {
  name: 'import.done';
  data: { title_id: number; file_id: number; action: ImportAction };
}

export interface SseFileEvent {
  name: 'file.changed';
  data: { file_id: number; state: FileState };
}

export interface SseResyncEvent {
  name: 'resync';
  data: Record<string, never>;
}

export type SseEvent =
  | SseStatusEvent
  | SseJobProgressEvent
  | SseDatLoadedEvent
  | SseDatRejectedEvent
  | SseSourceEvent
  | SseDownloadEvent
  | SseImportEvent
  | SseFileEvent
  | SseResyncEvent;
