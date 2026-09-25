export type ClientKind = 'transmission' | 'rtorrent';

export interface ClientStatus {
  kind: ClientKind | null;
  url: string | null;
  reachable: boolean;
  version: string | null;
  rtorrent_on_path: boolean;
  transmission_on_path: boolean;
  transmission_service: boolean;
  transmission_opt_in: boolean;
  checked_at: number;
}

/** A heavy job the closed gate holds. */
export interface WaitingJob {
  id: number;
  kind: string;
  state: JobState;
  detail: string | null;
}

export type PauseReason = 'core' | 'manual' | null;
export type LaunchState = 'ready' | 'disabled' | 'unavailable';
export type Override = 'paused' | 'running' | null;
export type ClientHold = 'uploads' | 'frozen' | null;

export interface SystemStatus {
  version: string;
  uptime_secs: number;
  client: ClientStatus | null;
  corename: string | null;
  paused: boolean;
  pause_reason: PauseReason;
  override: Override;
  /** How the download client is held while a core runs: stopped, or its uploads held. */
  client_hold: ClientHold;
  /** Whether the client is paused while a core runs. */
  pause_client_while_playing: boolean;
  waiting: WaitingJob[];
  disk_free_bytes: number | null;
  dats_dir: string;
  rss_bytes: number | null;
  launch: LaunchState;
  /** Decoded CHD bytes per second on the last image, null before the first. */
  chd_decode_bytes_per_sec: number | null;
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
  /** Files on disk that match no rom; always 0 for arcade, which a scan never walks. */
  unmatched_files: number;
  /** Disc images not identified by their tracks, listed by `GET /platforms/{id}/unidentified`. */
  unidentified_files: number;
  /** Clone groups with a visible MRA variant failing its md5 check and no verified variant; 0 outside arcade. */
  failing_check: number;
  /** Clone groups with a visible MRA variant missing some of its zips and no verified variant; 0 outside arcade. */
  partial: number;
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
  q?: string | undefined;
  have?: HaveFilter | undefined;
  wanted?: HaveFilter | undefined;
  region?: string | undefined;
  flags?: string | undefined;
  hidden?: 'hide' | 'show' | undefined;
  sort?: 'name' | 'have' | 'recent' | undefined;
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

export type FileState = 'verified' | 'unverified' | 'misnamed' | 'bad' | 'pending' | 'unidentified';

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
  availability: TitleAvailability[];
  source?: 'dat' | 'mra';
  mra?: TitleMra;
}

/** How a torrent file was matched to a rom, strongest first. */
export type MatchConfidence = 'hash' | 'name' | 'base' | 'fuzzy' | 'size';

/** A file of a bound source that may hold a rom of the variant. */
export interface TitleAvailability {
  source_id: number;
  source_name: string;
  file_index: number;
  path: string;
  rom_id: number;
  confidence: MatchConfidence;
}

export interface TitleMra {
  setname: string | null;
  rbf: string | null;
  path: string | null;
  missing_zips: string[];
  md5_check: 'match' | 'mismatch' | 'missing_part' | 'refused' | null;
  md5_detail: string | null;
}

export interface Launched {
  core: string;
  file: string | null;
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
  /** Versions and forms of one list share it; see VERIFICATION.md "DAT families". */
  family: string;
  /** Why the version is not current; `null` for a current one. */
  reason: string | null;
  /** For an unbound version, the platforms its family is current on. */
  suggested: string[];
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
  suggested_platform_id: string | null;
}

export type IncomingState = 'waiting' | 'importing' | 'rejected';

/** A file in `dats/` or `sources/` that has not loaded. */
export interface IncomingFile {
  file: string;
  size: number;
  state: IncomingState;
  reason: string | null;
  job_id: number | null;
  progress: Record<string, unknown> | null;
  modified: number;
}

export type SourceFileConfidence = 'hash' | 'name' | 'base' | null;

/** A further rom a torrent file may hold, from any mapping tier, beyond its matched one. */
export interface SourceFileCandidate {
  rom_id: number;
  rom_name: string;
  title_id: number;
  confidence: MatchConfidence;
}

export interface SourceFile {
  file_index: number;
  path: string;
  size: number;
  rom_id: number | null;
  rom_name: string | null;
  title_id: number | null;
  confidence: SourceFileConfidence;
  candidates: SourceFileCandidate[];
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

export type JobLane = 'heavy' | 'background' | 'light';

export interface Job {
  id: number;
  kind: string;
  lane: JobLane;
  payload: Record<string, unknown>;
  state: JobState;
  progress: Record<string, unknown> | null;
  reason: string | null;
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
  launch: boolean;
}

export interface ScanSettings {
  /** Decode CHD images to hash their tracks; slow on the board. */
  chd_tracks: boolean;
}

export interface TransferSettings {
  /** Pause the download client while a core other than the menu runs. */
  pause_client_while_playing: boolean;
}

export interface Settings {
  client: ClientSettings;
  limits: LimitsSettings;
  prefs: PrefsSettings;
  scan: ScanSettings;
  transfer: TransferSettings;
}

export interface SettingsPatch {
  client?: ClientSettings;
  limits?: LimitsSettings;
  prefs?: PrefsSettings;
  scan?: ScanSettings;
  transfer?: TransferSettings;
}

/** Why a disc image is not identified; `unidentified.ts` has the sentence for each. */
export type UnidentifiedReason =
  | 'off'
  | 'pending'
  | 'no_layout'
  | 'unreadable'
  | 'not_chd'
  | 'version'
  | 'parent'
  | 'not_cd'
  | 'too_large'
  | 'codec'
  | 'gdrom'
  | 'old_layout'
  | 'cooked'
  | 'pregap'
  | 'corrupt'
  | 'checksum'
  | 'no_checksum';

export interface UnidentifiedFile {
  rel_path: string;
  size: number;
  reason: UnidentifiedReason;
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
  data: {
    id: number;
    kind: string;
    state: JobState;
    detail?: string | null;
    progress: Record<string, unknown> | null;
  };
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
