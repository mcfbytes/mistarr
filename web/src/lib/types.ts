export type ClientKind = 'none' | 'transmission' | 'rtorrent';

export interface SystemStatus {
  version: string;
  uptime: number;
  client_kind: ClientKind;
  client_reachable: boolean;
  corename: string | null;
  paused: boolean;
  disk_free: number;
  rss: number;
}

export type WizardStepId = 'paths' | 'dats' | 'client' | 'sources';

export interface WizardStatus {
  steps: Record<WizardStepId, boolean>;
}

export type PlatformKind = 'cartridge' | 'disc' | 'computer' | 'arcade';

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
  wanted?: boolean;
  region?: string;
  flags?: string;
  sort?: 'name' | 'have' | 'recent';
}

export interface ArtUrls {
  boxart: string;
  title: string;
  snap: string;
}

export interface TitleGroup {
  parent_id: number;
  platform_id: string;
  base_name: string;
  pick_name: string;
  variants: number;
  have_verified: number;
  wanted: number;
  has_pick: boolean;
  art: ArtUrls;
}

export type RomStatus = 'good' | 'baddump' | 'nodump' | 'verified';

export type FileState = 'verified' | 'unverified' | 'misnamed' | 'bad' | 'pending';

export interface TitleRom {
  id: number;
  name: string;
  size: number;
  status: RomStatus;
  file_state: FileState | null;
  file_id: number | null;
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
  roms: TitleRom[];
  torrent_files_available: number;
}

export interface TitleDetail {
  parent_id: number;
  platform_id: string;
  base_name: string;
  pick_variant_id: number;
  art: ArtUrls;
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
  seed_policy: SeedPolicy;
  file_count: number;
  matched_count: number;
  total_size: number;
  client_status: string | null;
  added_at: number;
}

export interface SourceFile {
  file_index: number;
  path: string;
  size: number;
  matched_rom_name: string | null;
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
  source_id: number;
  state: DownloadState;
  progress: number;
  error: string | null;
  created_at: number;
  updated_at: number;
}

export type ImportAction = 'placed' | 'replaced' | 'quarantined' | 'skipped_existing';

export interface ImportLogEntry {
  id: number;
  at: number;
  download_id: number | null;
  file_id: number | null;
  action: ImportAction;
  detail: string;
}

export type JobKind = 'scan' | 'import' | 'poll' | 'bind';
export type JobState = 'queued' | 'running' | 'paused' | 'done' | 'failed';

export interface Job {
  id: number;
  kind: JobKind;
  state: JobState;
  progress: Record<string, unknown> | null;
  created_at: number;
  updated_at: number;
}

export interface Settings {
  [key: string]: string | number | boolean;
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
  data: { id: number; kind: JobKind; progress: Record<string, unknown> };
}

export interface SseDatEvent {
  name: 'dat.loaded' | 'dat.rejected';
  data: { dat_version_id?: number; file: string; reason?: string };
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

export type SseEvent =
  | SseStatusEvent
  | SseJobProgressEvent
  | SseDatEvent
  | SseSourceEvent
  | SseDownloadEvent
  | SseImportEvent
  | SseFileEvent;
