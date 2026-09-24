# Architecture

mistarr is one static ARMv7 binary that runs on the MiSTer's Linux side and
serves a single-page web UI to any browser on the LAN. It owns a SQLite
database, watches two directories for user-supplied inputs, talks to the
torrent client that ships with the image, and moves verified files into the
`games/` tree.

## Constraints that shape everything

| Constraint | Consequence |
|---|---|
| Cortex-A9 dual core at ~800 MHz, ARMv7 hard-float | Cross-compiled static musl binary. Hashing is streaming and background. No CHD decompression in the critical path. |
| Roughly 490 MiB RAM visible to Linux, shared with the MiSTer process | Idle RSS target under 30 MiB, hard ceiling 64 MiB. No in-memory torrent metadata for set torrents; file lists live in SQLite. |
| SD card, exFAT, ~10-20 MB/s writes | Stage and rename on the same filesystem, never copy. Throttle hashing with `ionice`. No symlinks, case-insensitive names. |
| Stock image is Buildroot 2021 with glibc 2.31 | musl static linking, no OpenSSL, `rustls` only. |
| MiSTer main process wants the CPU when a core runs | Watch `/tmp/CORENAME`; pause hashing and imports while it is anything other than `MENU`. Transfers continue at a reduced rate limit. |
| Torrent client already present | Stock image ships rtorrent; Buildroot_MiSTer ships Transmission. mistarr never embeds a client. |
| Content neutrality | See PRINCIPLES.md. No sources in the tree; watched directories are the only input path. |

## Components

```
 browser (Svelte SPA) ──HTTP + SSE──▶ mistarr-server (axum, SQLite)
                                          │
        ┌──────────────┬──────────────────┼────────────────┬───────────────┐
   mistarr-core   mistarr-sources   mistarr-clients   mistarr-mister    jobs
   DAT parsing    watch dirs        DownloadClient    platform table    scheduler,
   catalog        torrent parse     trait:            core adapters     scan, hash,
   matching       platform bind     Transmission,     CORENAME watch    import,
   1G1R, hashes   staging           rtorrent          placement         SSE fanout
                                          │                    │
                                   transmission-daemon    /media/fat/games/<Core>/
                                   or rtorrent (SCGI)     /media/fat/mistarr/{sources,dats,staging}
```

### Crates

All crates live in one Cargo workspace. Interfaces between crates are traits
and plain data types so work packages can proceed in parallel against the
contracts in this document.

| Crate | Responsibility | Depends on |
|---|---|---|
| `mistarr-core` | Domain types. Logiqx DAT parser. Catalog model with parent/clone groups. Hashing (CRC32, MD5, SHA1 in one streaming pass). Matching of files to DAT entries. 1G1R selection with region and revision preferences. Header detection and stripping for hashing. Cue sheet parsing. | none |
| `mistarr-mister` | The DAT-name to `games/<Core>` table. `CoreAdapter` trait and implementations for every quirk. `/tmp/CORENAME` watcher. Installed-core detection from `_Console`, `_Computer`, `_Arcade` and `_Other`. MRA parsing for arcade wanted lists. | core |
| `mistarr-sources` | Watched-directory scanner. `.torrent` (bencode) and `.magnet` parsing into a file list. Binding a torrent to a platform by name and size overlap with loaded DATs. Mapping torrent file indices to DAT entries. | core |
| `mistarr-clients` | `DownloadClient` trait. Transmission JSON-RPC implementation. rtorrent XML-RPC over SCGI implementation. Client detection and, for rtorrent on stock, launch with a generated rc. Remote path mapping. | none |
| `mistarr-server` | The binary. axum HTTP server, SQLite via `rusqlite` (bundled), job scheduler, SSE event bus, embedded SPA via `rust-embed`, config, first-run wizard state, CLI flags. | all |
| `web/` | Svelte 5 + Vite + TypeScript SPA. Built to `web/dist`, embedded at compile time. | API.md |

### Key traits

```rust
// mistarr-clients
#[async_trait]
pub trait DownloadClient: Send + Sync {
    async fn probe(&self) -> Result<ClientInfo>;
    /// Add a torrent paused, with only `wanted` file indices selected, into `download_dir`.
    async fn add(&self, src: TorrentSource, download_dir: &Path, wanted: &[u32], seed: SeedPolicy) -> Result<ClientTorrentId>;
    async fn set_wanted(&self, id: &ClientTorrentId, wanted: &[u32]) -> Result<()>;
    async fn set_seed_policy(&self, id: &ClientTorrentId, seed: SeedPolicy) -> Result<()>;
    async fn start(&self, id: &ClientTorrentId) -> Result<()>;
    async fn stop(&self, id: &ClientTorrentId) -> Result<()>;
    async fn status(&self, id: &ClientTorrentId) -> Result<TorrentStatus>;          // per-file progress included
    async fn files(&self, id: &ClientTorrentId) -> Result<Vec<ClientFile>>;         // paths and sizes, MetadataPending until known
    async fn remove(&self, id: &ClientTorrentId, delete_data: bool) -> Result<()>;
    async fn set_rate_limits(&self, down_kbps: Option<u32>, up_kbps: Option<u32>) -> Result<()>;
}

// mistarr-mister
pub trait CoreAdapter: Send + Sync {
    fn platform(&self) -> PlatformId;
    fn games_dir(&self, root: &Path) -> PathBuf;                     // e.g. root/games/NES
    /// Decide the final filename and any transformation (unzip, header, byte order).
    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan>;
    /// True if this file, as found on disk, is loadable by the core without change.
    fn accepts(&self, path: &Path) -> bool;
    fn requires_bios(&self) -> Option<&'static str>;                 // reported, never fetched
}

// mistarr-core
pub struct HashSet { pub size: u64, pub crc32: u32, pub md5: [u8;16], pub sha1: [u8;20] }
pub fn hash_reader<R: Read>(r: R, rule: HeaderRule, size_hint: Option<u64>) -> io::Result<HashSet>;
pub fn parse_dat(xml: &[u8]) -> Result<Dat>;
pub fn select_1g1r(group: &[DatGame], prefs: &Prefs) -> Option<&DatGame>;
```

## Runtime flows

### Startup

1. Load config from `/media/fat/mistarr/mistarr.toml`, or defaults.
2. Open or create SQLite at `/media/fat/mistarr/mistarr.db`, run migrations.
3. Detect the download client: probe Transmission RPC on `127.0.0.1:9091`,
   then rtorrent SCGI at the configured socket or `127.0.0.1:5000`. If neither
   answers and `rtorrent` is on `PATH`, offer to launch it with a generated rc
   pointing at the staging directory. Record the result; do not retry on every
   request.
4. Detect installed cores by listing the `_Console`, `_Computer`, `_Arcade`
   and `_Other` directories. Platforms whose core is absent are shown but
   collapsed.
5. Start the watched-directory scanner, the CORENAME watcher, the job
   scheduler and the HTTP server on port 8420.
6. If no DAT has ever been loaded, the UI opens on the wizard.

### DAT import

1. A file appears in `dats/`. Accept `.dat`, `.xml`, and `.zip` containing
   either. Parse Logiqx `<datafile>` with `quick-xml`. Reject anything else and
   move it to `dats/rejected/` with a reason file.
2. Identify the platform from the DAT header name using the table in
   PLATFORMS.md. Unknown DAT names are stored as an unbound platform the user
   can map in the UI.
3. Upsert `dat_versions`, then `titles` and `roms`. A newer version of the same
   DAT name supersedes the old one: entries not present in the new DAT are
   marked `retired`, never deleted, so verified files keep their provenance.
4. Parent/clone data is read from `cloneof` attributes when present. When
   absent, clone groups are inferred by normalising the name (strip region,
   revision, language and flag tags) so 1G1R still works with plain DATs.
5. Move the file to `dats/loaded/` and emit `dat.loaded` on the event bus.

### Library scan

1. Triggered by the wizard, manually, or on a schedule. Walk each platform's
   `games/<Core>` directory. Skip while a core is running.
2. For each file, compare size and mtime with `files`. Unchanged files are
   skipped. New or changed files are hashed in one streaming pass with the
   platform's header rule. Zip members are hashed through the decompressor,
   and the zip central-directory CRC is used as a pre-check to skip hashing
   members that cannot match anything.
3. Match by SHA1, then MD5, then CRC32 plus size. Record `verified`,
   `unverified` (no DAT match) or `misnamed` (match but wrong filename).
4. Scans are resumable: progress is committed per directory.

### Source import

1. A `.torrent` or `.magnet` appears in `sources/`, found by a scan every
   10 s once its size has held for two scans, or written there by
   `POST /sources/upload`. A light `source_import` job per file parses it. A
   file that does not parse, or repeats a loaded source, moves to
   `sources/rejected/` with a `<name>.reason.txt`.
2. For a magnet, the source is `resolving`. A light `resolve_magnet` job adds
   it to the client paused into `staging/<infohash>/` with nothing wanted,
   then asks the client for its file list every 15 s. Once the list arrives
   every file is left unwanted and the source is bound like a `.torrent`.
   With no client detected the source stays `resolving` with a reason and is
   retried when one appears.
3. For each file in the torrent, normalise the leaf name and look it up
   against every loaded DAT by name, then by base name plus size. Compute
   per-platform hit rates.
4. Bind the source to the platform with the best rate at or above
   `sources.bind_threshold` (default 0.6). Below that, the source is
   `unbound` and the user picks a platform or discards it.
5. Store the file list in `torrent_files` with the matched `rom_id` and its
   confidence where one exists. Move the file to `sources/loaded/` and emit
   `source.changed`. A `.torrent` is not told to the client until something
   is wanted.

### Wanted and transfer

1. The user marks a title as wanted. mistarr picks the best `rom` for it under
   the 1G1R preferences, then the best `torrent_file` for that rom across bound
   sources, preferring sources with a size match and, if known, healthier
   swarms.
2. If the torrent is not yet in the client, add it paused to `staging/<infohash>/`
   with only that file wanted and the source's seed policy. If it is, extend the
   wanted set. Start it.
3. Poll the client at an interval (5 s while something is active, 60 s idle).
   Per-file progress is written to `downloads` and fanned out on SSE.
4. When a file reaches 100 percent and the client reports it checked, hand it
   to the importer. If every wanted file in the torrent is done and the seed
   policy is `none`, remove the torrent from the client without deleting data
   until the import has succeeded.

### Import

1. Hash the staged file. Match against the DAT. A mismatch marks the download
   `bad` and leaves the file in `staging/quarantine/` with a report; it is
   never placed.
2. Ask the platform's `CoreAdapter` for a placement plan. Apply it: unzip if
   the core cannot read zips or the plan says so, add or strip a header, fix
   byte order, create a per-title directory for multi-file disc images and
   move every track.
3. Rename into `games/<Core>/` on the same filesystem. If the target exists
   and is verified, keep the existing file and discard the new one. If it
   exists and is unverified, replace it and record the previous name.
4. Update `files`, mark the title `have`, emit `import.done`.

### Pausing for the core

The CORENAME watcher polls `/tmp/CORENAME` every 2 s. When the value is not
`MENU` the scheduler pauses hash and import jobs at the next file boundary and
asks the client to apply the "core running" rate limits. When it returns to
`MENU` everything resumes. This is a scheduler-level gate, not something each
job needs to know about.

## Resource budgets

| Item | Budget |
|---|---|
| Binary size, stripped, with SPA | under 8 MiB |
| Idle RSS | under 30 MiB |
| Peak RSS during scan or import | under 64 MiB |
| tokio worker threads | 2 |
| SQLite page cache | 2 MiB |
| Hashing buffer | 256 KiB, one file at a time |
| SPA bundle, gzipped | under 200 KiB |
| Concurrent client RPC calls | 1, serialised |

## Configuration

`mistarr.toml`, all optional:

```toml
[server]
listen = "0.0.0.0:8420"
api_key = ""                 # empty means LAN-open, like the *arr default

[paths]
root      = "/media/fat"
games     = "/media/fat/games"
data      = "/media/fat/mistarr"   # db, sources/, dats/, staging/

[client]
kind      = "auto"          # auto | transmission | rtorrent
url       = ""              # transmission RPC url or rtorrent scgi address
remote_path_map = []        # [{ remote = "/downloads", local = "/media/fat/mistarr/staging" }]

[limits]
down_kbps_menu = 0          # 0 = unlimited
down_kbps_core = 512
up_kbps_menu   = 0
up_kbps_core   = 64

[prefs]
regions   = ["USA", "World", "Europe", "Japan"]
languages = ["En"]
prefer_latest_revision = true
hide = ["bios", "beta", "proto", "demo", "sample", "program"]

[sources]
bind_threshold = 0.6        # lowest per-platform hit rate, 0 to 1, that binds a source
```

The file is `--config FILE` if given, else `<data>/mistarr.toml` when it
exists, where `<data>` is `--data DIR` or `/media/fat/mistarr`; `--data`
also overrides `paths.data` and `--listen` overrides `server.listen`. The
`client`, `limits` and `prefs` sections are editable through
`/system/settings`; saved values live in the `settings` table and take
precedence over the file on every start. `server`, `paths` and `sources`
need a restart.

## Non-goals

- No emulation, no launching of games, no save management. MiSTer Remote and
  friends do that.
- No metadata providers beyond libretro thumbnails. No IGDB, no ScreenScraper,
  no API keys.
- No embedded torrent client.
- No CHD conversion in the first release. bin/cue and ISO are supported by the
  CD cores and are what Redump DATs describe. CHD verification can come later
  behind a feature flag if someone wants to pay the CPU cost.
- No arcade romset building. Arcade support is "this MRA needs these zips, here
  is which are missing"; the zips are files like any other.
