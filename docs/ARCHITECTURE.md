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
| MiSTer main process wants the CPU when a core runs | Watch `/tmp/CORENAME`; pause hashing, scans and file placement while it is anything other than `MENU`. DAT and source parsing continue at low priority; transfers continue at a reduced rate limit. |
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
| `mistarr-mister` | The DAT-name to `games/<Core>` table. `CoreAdapter` trait and implementations for every quirk. `/tmp/CORENAME` watcher. Installed-core detection from `_Console`, `_Computer`, `_Arcade` and `_Other`. MRA parsing for arcade wanted lists. MGL building and the `CommandSink` that hands `load_core` commands to MiSTer Main. | core |
| `mistarr-sources` | Watched-directory scanner. `.torrent` (bencode) and `.magnet` parsing into a file list. Binding a torrent to a platform by name and size overlap with loaded DATs. Mapping torrent file indices to DAT entries. | core |
| `mistarr-clients` | `DownloadClient` trait. Transmission JSON-RPC implementation. rtorrent XML-RPC over SCGI implementation. Client detection and, for rtorrent on stock, launch with a generated rc. Remote path mapping. | none |
| `mistarr-server` | The binary. axum HTTP server, SQLite via `rusqlite` (bundled), job scheduler, SSE event bus, embedded SPA via `rust-embed`, config, first-run wizard state, CLI flags. | all |
| `mistarr-fixture` | Development tool, never shipped: synthetic DATs, `.torrent` files, the synthetic set and a local tracker for the tests in TESTING.md. | core, mister, sources |
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
   Read CORENAME once, so the gate is closed from the start while a core is
   loaded, then reconcile the jobs a previous process left open (see
   "Pausing for the core").
3. Detect the download client: probe Transmission RPC on `127.0.0.1:9091`,
   then rtorrent SCGI at the configured socket or `127.0.0.1:5000`. If neither
   answers and `rtorrent` is on `PATH`, offer to launch it with a generated rc
   pointing at the staging directory; if `transmission-daemon` is installed,
   offer to start it (DOWNLOAD-CLIENTS.md "Starting a stopped client").
   Record the result; do not retry on every request, but re-detect every
   minute while no client answers.
4. Detect installed cores by listing the `_Console`, `_Computer`, `_Arcade`
   and `_Other` directories. Platforms whose core is absent are shown but
   collapsed. When `_Arcade` exists, or MRA titles are stored, queue the
   arcade catalogue.
5. Start the watched-directory scanner, the CORENAME watcher, the job
   scheduler and the HTTP server on port 8420.
6. If no DAT has ever been loaded, the UI opens on the wizard.

### DAT import

1. A file appears in `dats/`. The watcher lists the directory every 10 s and
   enqueues an import once the file's mtime is 2 s old and its size held
   between two listings. Accept `.dat`, `.xml`, and `.zip` containing either;
   each member of a zip is a separate DAT, and a file whose import job failed
   is enqueued again on a later listing. The `dat_import` job runs on the
   background lane, so a loaded core does not hold it. Parse Logiqx
   `<datafile>` with `quick-xml`, streaming. Each game is parsed outside the
   database's write lock and appended to `dat_stage` in chunks of 2,000
   games, one short transaction per chunk; one transaction then applies the
   stage (steps 3 to 5), so readers see the old titles or the new ones and
   never part of a DAT. A parse error empties the stage and changes nothing
   else. Reject anything else and move it to `dats/rejected/` with a
   `<name>.reason.txt` beside it. The apply holds the writer for the SQL
   alone; scans and imports resolve rom ids on the read connection and
   write afterwards, so one that straddles an apply records a rom id that
   is still a row, retired or not, as it would had it finished just before.
2. Identify the platform from the DAT header name using the table in
   PLATFORMS.md, falling back to the platform an earlier version of the same
   name was bound to. A header without a name takes the member's or file's
   stem as dropped. Unknown DAT names are stored as an unbound
   `dat_versions` row the user can bind in the UI; binding re-reads the file
   from `dats/loaded/` and loads its titles.
3. Upsert `dat_versions`, then `titles` and `roms`. A newer version of the same
   DAT name supersedes the old one: entries not present in the new DAT are
   marked `retired`, never deleted, so verified files keep their provenance.
4. Parent/clone data is read from `cloneof` attributes when present. When
   absent, clone groups are inferred by normalising the name (strip region,
   revision, language and flag tags) so 1G1R still works with plain DATs.
5. Recompute the platform's 1G1R picks, move the file to `dats/loaded/` and
   emit `dat.loaded` on the event bus, then bind the unbound sources waiting
   for that platform ("Source import" step 4). Changing `prefs` recomputes the
   picks of every platform.

### Library scan

1. Triggered manually, on the `[jobs] scan_interval_minutes` schedule,
   automatically for a platform once its DAT finishes loading if that
   platform's games directory already exists (deduped per platform, so a
   zipped pack of several DATs queues one scan each), or once, full, the
   first time every wizard step reports done. Walk each platform's
   `games/<Core>` directory and its other accepted directories, except
   arcade: its zips are never walked as cartridges, since presence and
   verification there come only from the arcade catalogue's own presence
   pass, its md5 check, and the import path (PLATFORMS.md "MRA catalogue").
   A manual scan of `arcade` queues the arcade catalogue instead
   (`POST /system/scan`, API.md "System"). Skip
   while a core is running; the timer goes through the same heavy lane as
   the others, so it waits for the gate too.
2. For each file, compare size and mtime with `files`. Unchanged files are
   skipped. New or changed files are hashed in one streaming pass with the
   platform's header rule. Zip members are hashed through the decompressor,
   and the zip central-directory CRC is used as a pre-check to skip hashing
   members that cannot match anything.
3. Match by SHA1, then MD5, then CRC32 plus size. Record `verified`,
   `unverified` (no DAT match) or `misnamed` (match but wrong filename).
4. Scans are resumable: hashed rows are written 256 at a time and the
   finished directories at most every 2 s. A directory not yet recorded as
   finished is walked again after a restart, and its unchanged files are not
   hashed again.

### Arcade catalogue

1. A heavy `arcade_catalog` job, queued at startup, by `POST /system/scan`
   for every platform or for `arcade`, and by `POST /system/cores`. It lists
   every `.mra` under `_Arcade`, four folder levels deep including
   `_alternatives`, follows symlinks to files, and leaves out symlinked
   folders, the Arcade Organizer's `_Organized` tree and second names of one
   file (same device and inode); see PLATFORMS.md "MRA catalogue".
2. Each MRA becomes one `arcade` title named by its `<name>`, trimmed with
   inner whitespace collapsed (the file stem when that is empty), with one rom
   per zip it names. MRAs are taken shallowest first and a later MRA with a
   name already taken is skipped. An MRA whose size, mtime and parser version
   match those its live title was stored from is not read again, and one that
   cannot be read keeps its title and check until it can. The job works in
   batches of 64 files, each read, checked and written in one short
   transaction, and reports `{ done, total, parsed, checked }` as it goes.
   Titles whose MRA is gone are retired at the end, and the 1G1R picks are
   recomputed then when a batch stored a title or a title retired; a batch
   that stores marks the platform in `settings`, so a run stopped before its
   end leaves the recompute to the next.
3. Each zip is looked up under `games/`, directories and file name
   case-insensitively, and its presence stored on its rom. A title whose MRA carries an `md5` and whose zips are
   all present is checked by assembling its roms (PLATFORMS.md "MRA
   assembly"), streaming each part from its zip; the check reruns only when
   the MRA or one of its zips changes size or mtime, so each MRA is read at
   most once per run. Placing one of its zips reruns this for every title naming
   that zip. Nothing is ever fetched, rebuilt, merged or split.
4. Arcade presence pass: after titles are stored and retired, the same job
   walks every zip directly under `games/mame` and `games/hbmame`, in
   batches, reading each one's central directory only (member names, sizes
   and CRC32; never decompressed). A member whose CRC32 and size match a
   loaded DAT's rom is recorded against it, at CRC32 level rather than the
   scan's full hash. A member of a zip a live MRA names, that no DAT
   matches, is recorded `unverified` against the MRA's own zip rom, giving
   the next import's `verify_siblings` a row to promote once it reads that
   zip as a sibling; without this row the promotion was a silent no-op. A
   zip that fails to open gets one bare-path row and a warning, not a
   failed job. The pass is incremental by each zip's size and mtime, like a
   scan, and ends by deleting `files` rows for this platform whose zip or
   member it did not see, so a zip removed from disk drops out of `have`.

### Source import

1. A `.torrent` or `.magnet` appears in `sources/`, found by a scan every
   10 s once its size has held for two scans, or written there by
   `POST /sources/upload`. A `source_import` job per file, on the background
   lane, parses it, reading the bencode in place so only the file list is
   built. A file over 16 MiB, one that does not parse, or one that repeats a
   loaded source moves to `sources/rejected/` with a `<name>.reason.txt`.
2. For a magnet, the source is `resolving`. A light `resolve_magnet` job adds
   it to the client paused into `staging/<infohash>/` with nothing wanted and
   starts it, since a paused magnet never fetches metadata. While it is
   started the job asks the client for its file list every 2 s, because once
   metadata arrives the client wants every file. When the list appears the
   torrent is stopped first, then every file is set unwanted and the source
   is bound like a `.torrent`. A magnet not yet in the client, because none
   is detected or the add failed, stays `resolving` with a reason and is
   retried every 15 s.
3. For each file in the torrent, normalise the leaf name and look it up
   against every loaded DAT by name, then by base name plus size. Compute
   per-platform hit rates.
4. Bind the source to the platform with the best rate at or above
   `sources.bind_threshold` (default 0.6). Below that, the source is
   `unbound` and the user picks a platform or discards it.
   Independently of any DAT, the platform table's DAT-name patterns are
   matched against the dropped file's stem and the torrent's info name,
   taking the longest match within each name. If either matches, those
   decide; otherwise the matching directories that hold the most files
   decide, among those holding at least half of them. Two different
   platforms at the deciding level mean no suggestion. The suggestion is
   stored, shown on the source and named in its reason. After a DAT pack
   loads, unbound sources are bound again once: to the suggested platform
   when its hit rate reaches the threshold and no other platform scores
   higher, otherwise as above. A source the user unbound is never bound
   automatically, and `source.changed` is sent only for sources whose state
   or platform changed.
5. Store the file list in `torrent_files` with the matched `rom_id` and its
   confidence where one exists. Move the file to `sources/loaded/` and emit
   `source.changed`. A `.torrent` is not told to the client until something
   is wanted.

### Wanted and transfer

1. The user marks a title as wanted. mistarr creates a download for each of
   its roms without a verified file, choosing the best `torrent_file` for it
   across bound sources: an exact size match first, then a name match, then
   the source with fewer open downloads. With no such file the download is
   `wanted` until a source binds that has one.
2. A light `transfer` job takes `queued` downloads per source. If the torrent
   is not yet in the client, create `staging/<infohash>/` (rtorrent makes only
   the last level of a download path) and add the torrent paused to it, through
   the remote path map, with only the selected files wanted and the source's
   seed policy. If it is, extend the wanted set. Start it. A magnet whose
   metadata is pending keeps its downloads queued.
3. Poll the client at an interval (5 s while something is transferring or
   checking, 60 s otherwise, 5 minutes after three failed polls). Per-file
   progress is written to `downloads` and fanned out on SSE as
   `download.changed`, only for rows that moved.
4. When a file reaches 100 percent and the client reports it checked, the
   download becomes `importing` with its `staged_path`, which hands it to the
   importer. Under seed policy `none` neither client stops a torrent on its
   own; the poller stops it once no download of its source is queued,
   transferring or checking and every file selected in the client is
   complete. The importer removes it from the client, without deleting data,
   once the files are placed. A later want on a stopped torrent extends the
   selection and starts it again.

### Import

A heavy `import` job runs per download in `importing`. The importer enqueues
one for every such row at startup, oldest first, and whenever
`download.changed` reports a download entering `importing`, unless a job for
it is already queued or running. A download that fails or is cancelled also wakes the `importing`
tracks of its entry that wait for it. A job whose row has left `importing`
does nothing.

1. Hash the staged file with the platform's header rule, every member of a
   staged zip, and match it against the roms of the download's own entry, so
   byte-identical regional variants and identical disc tracks resolve to the
   wanted rom. Only when nothing of the entry matches is the rest of the DAT
   searched, and the file is a mismatch either way: the download becomes
   `bad` and the file moves to `staging/quarantine/<infohash>/` beside a
   `<name>.report.txt` naming the expected rom, the actual hashes and the
   other entry it matches, if any. An entry flagged `bios` is refused: the
   download is `failed` and the file stays in staging. A zip an MRA title
   names is verified and placed as PLATFORMS.md "MRA import" describes.
2. A romset or arcade zip verifies only when every member is a rom of the
   entry and every rom of the entry is a member. A disc entry waits until
   every track is `importing` and is placed together; when a track is missing
   and nothing of the entry is still transferring, its downloads become
   `failed` and the tracks stay in staging.
3. Ask the platform's `CoreAdapter` for a placement plan from the DAT entry
   and the staged item (its zip members, and the first bytes that decide an
   iNES header or N64 byte order). Its staging steps (unzip, zip, header, byte
   order) run first and write only to `staging/.import/<download id>/`,
   leaving the staged files untouched; any failure there removes that
   directory and leaves the download `failed` and retryable.
4. Then create directories and rename into `games/<Core>/` on the same
   filesystem; staging and `games/` on different filesystems fail the import
   with both paths named. If the target exists and is verified, keep the
   existing file and discard the new one. If it exists and is not verified,
   replace it and record the previous file. When a rename fails partway, the
   files that landed are recorded and the downloads become `failed`; a retry
   treats a track whose staged file is gone but whose rom has a verified file
   as placed, and completes the rest.
5. Update `files` with the rows the library scan would write (one per member
   of a zip placed whole, as `a.zip#member`), which marks the title `have`,
   log the action in `import_log`, set the downloads `done` and emit
   `import.done`.
6. Once a source has a `done` download and none queued, transferring,
   checking or importing, and its seed policy is `none`, remove the torrent
   from the client without deleting data, clear `sources.client_id` and
   remove the empty directories under `staging/<infohash>/`.

### Launching

A title in the collection, or a platform's core alone, can be started from
the UI through MiSTer Main's command FIFO, `/dev/MiSTer_cmd` (API.md
"Launching"). Every path comes from the database and the SD card, never
from the request.

1. `prefs.launch` must be on and the FIFO must exist; otherwise nothing is
   written. Launches are serialised by one lock held from planning to the
   write, and a launch within 3 s of the last one sent is refused as busy,
   so two taps never start two cores. The FIFO is opened write-only and
   non-blocking for each command, so a Main that is not reading fails at
   once instead of stalling a worker.
2. The core is chosen from the platform row's launch cores in order
   (PLATFORMS.md "Launch parameters"): the first name with an installed
   `.rbf`, and of those the newest by the `_YYYYMMDD` date in its name. A
   bare core is started with `load_core <path>`.
3. A DAT entry needs a `verified`, `misnamed` or `bad` file for every live
   rom; a disc needs every track `verified` and loads the first cue sheet
   whose `FILE` entries all exist beside it. mistarr writes a new MGL naming
   the core and the entry's file with that core's parameters, as
   `/tmp/mistarr-<millis>-<seq>.mgl` (tmpfs on the board), keeps the newest
   three, and sends `load_core` on it.
4. An MRA title with every zip present and no failed md5 check is started
   with `load_core` on its `.mra` file; a stored MRA path that is not plain
   names below `_Arcade` is refused.

The CORENAME watcher then sees the core and pauses heavy jobs as below.

### Pausing for the core

Jobs run on three serial lanes, one job at a time each:

| Lane | Jobs | While a core runs |
|---|---|---|
| heavy | `scan`, `import`, `arcade_catalog` | Held: a queued job does not start and a running one stops at its next file boundary, `paused`. |
| background | `dat_import`, `recompute_1g1r`, `source_import` | Runs. A DAT parse sleeps 20 ms every 200 entries, on top of the process's `nice` level. Held, like the heavy lane, by a manual pause. |
| light | `detect_client`, `transfer`, `resolve_magnet`, `deselect` | Runs. |

The CORENAME watcher polls `/tmp/CORENAME` every 2 s. When the value is not
`MENU` the gate closes for the heavy lane and the poller applies the "core
running" rate limits. When it returns to `MENU` everything resumes. This is
a scheduler-level gate, not something each job needs to know about.

"Pause" (`POST /system/pause`) holds the heavy and background lanes; a DAT
parse in progress waits at its next 200 entries. While a lane is held,
`/system/status` lists its queued and paused jobs as `waiting`, and each of
them carries a `reason` in `/system/jobs`, so the UI can say what waits and
why. "Run now" (`POST /system/resume`) opens the gate
until CORENAME changes or the heavy queue drains, whichever comes first;
after that, new heavy work waits for the core again.

`scan`, `arcade_catalog` and `recompute_1g1r` are singletons per payload: a
request joins a queued or paused job of the same kind and payload instead of
queueing another. Any other kind joins only a job that has not started.

At startup the scheduler takes over the queued, running and paused rows the
previous process left. The first row of each kind and payload goes back on
its lane under its own id when the kind can be re-run (`scan`,
`arcade_catalog`, `dat_import` of a dropped file, `recompute_1g1r`,
`source_import`, `import`); other kinds fail with "interrupted by a
restart", and repeats of a kind and payload are deleted. A job stopped by a
shutdown is left `queued` for this.

## Resource budgets

| Item | Budget |
|---|---|
| Binary size, stripped, with SPA | under 8 MiB |
| Idle RSS | under 30 MiB |
| Peak RSS during scan or import | under 64 MiB, checked per job by `tests/memory.rs` |
| tokio worker threads | 2 |
| Blocking threads (SQLite, hashing, file work) | at most 4 |
| Stack per runtime thread | 1 MiB reserved, touched pages only in RSS |
| Soft `RLIMIT_DATA` | `[memory] data_limit_mib`, 192 MiB, never below 64 |
| SQLite page cache | 2 MiB, 1 MiB on each of the two connections |
| SQLite other | `mmap_size = 0`, `temp_store = FILE` under `<data>/tmp` (`SQLITE_TMPDIR`, set at startup and emptied of stale files, since the board's `/tmp` is RAM), WAL checkpoint every 256 pages, WAL cut to 1 MiB after a checkpoint, `soft_heap_limit` 8 MiB |
| SQLite writes | one writer; async writes wait their turn on a semaphore before taking a blocking thread, so queued writers never starve reads; a DAT import already on a blocking thread takes the writer per staged chunk |
| Hashing buffer | 256 KiB, one file at a time |
| Arcade catalogue | 64 MRA files per batch; only zip listings and names taken persist across batches |
| Arcade presence pass | 500 zips per batch, central directory only, never decompressed |
| `.torrent` or `.magnet` file | 16 MiB, read whole, parsed in place |
| Browse page or search, with its total | under 100 ms on the board for a platform of 15 000 titles; `tests/browse.rs` holds a host bound |
| SPA bundle, gzipped | under 200 KiB |
| Concurrent client RPC calls | 1, serialised |

The data limit is set in `main` before the runtime starts, so thread stacks
and heap both count against it. `RLIMIT_DATA` rather than `RLIMIT_AS`
because it counts only private writable memory (brk, anonymous mappings,
thread stacks), which is what a runaway allocation grows, and not the
binary, the SQLite shared-memory index or reserved address space. An
allocation past it fails and Rust aborts the process, which ends one daemon
instead of starving the MiSTer process of memory on a board without swap.

The launcher runs the daemon under `nice -n 10` and `ionice -c 3` where the
board has them, and heavy jobs stop at their next file boundary while a core
runs. Heavy work has no thread of its own to lower further: it shares the
blocking pool with request handlers.

A DAT loads in one write transaction, so the WAL file can grow to the size
of the pages that DAT touches while it loads; it is cut back to 1 MiB at the
next checkpoint. Page memory stays within the cache either way, since SQLite
spills dirty pages to the WAL.

Every write transaction commits through `db::commit`, which first refreshes
the clone groups its writes touched in `title_groups` (DATA-MODEL.md
"Derived tables"). Browse and the platform counts then read one indexed row
per group; a page and its total cost about as much as reading the page, and
the refresh adds a few hundred milliseconds on the host to a 15 000-game DAT
load. Search probes the `title_search` trigram index for three or more
characters and confirms with `LIKE`. A common short term costs time in
proportion to its matches across every platform; if measurements on the
board ever call for it, the next step is a sentinel-wrapped platform column
in `title_search` queried with an FTS column filter.

## Configuration

`mistarr.toml`, all optional:

```toml
[server]
listen = "0.0.0.0:8420"
api_key = ""                 # empty means LAN-open, like the *arr default
allowed_hosts = []           # extra Host names for writes, e.g. ["nas.example", "*.home.arpa"]

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
launch = true               # allow starting cores and games from the UI

[sources]
bind_threshold = 0.6        # lowest per-platform hit rate, 0 to 1, that binds a source

[jobs]
scan_interval_minutes = 1440   # a daily rescan by default, 0 disables it

[memory]
data_limit_mib = 192        # soft RLIMIT_DATA set at startup, at least 64; 0 keeps the inherited limit
```

The file is `--config FILE` if given, else `<data>/mistarr.toml` when it
exists, where `<data>` is `--data DIR` or `/media/fat/mistarr`; `--data`
also overrides `paths.data` and `--listen` overrides `server.listen`. The
`client`, `limits` and `prefs` sections are editable through
`/system/settings`; saved values live in the `settings` table and take
precedence over the file on every start. `server`, `paths`, `sources`,
`jobs` and `memory` need a restart.

## Non-goals

- No emulation and no save management. Launching hands a command to MiSTer
  Main and stops there.
- No metadata providers beyond libretro thumbnails. No IGDB, no ScreenScraper,
  no API keys.
- No embedded torrent client.
- No CHD conversion in the first release. bin/cue and ISO are supported by the
  CD cores and are what Redump DATs describe. CHD verification can come later
  behind a feature flag if someone wants to pay the CPU cost.
- No arcade romset building. Arcade support is "this MRA needs these zips, here
  is which are missing"; the zips are files like any other.
