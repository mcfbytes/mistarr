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
| SD card, exFAT, ~10-20 MB/s writes | Stage and rename on the same filesystem, never copy. Idle I/O class while a core runs. No symlinks, case-insensitive names. |
| `/media/fat` mounted `sync,dirsync` | Every write syscall there is flushed to the card before it returns, about 25 ms each, so the database's cost is its count of writes, not bytes. SQLite's temporary files and the DAT stage live in RAM, and a DAT import or a migration runs on a copy of the database in RAM written back 1 MiB at a time; see "Writes on a sync mount" and "DAT import in RAM". |
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
| `mistarr-core` | Domain types. DAT parser for Logiqx XML and No-Intro DB exports. Catalog model with parent/clone groups. Hashing (CRC32, MD5, SHA1 in one streaming pass). Matching of files to DAT entries. 1G1R selection with region and revision preferences. Header detection and stripping for hashing. Cue sheet parsing. | none |
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
2. Open or create SQLite at `/media/fat/mistarr/mistarr.db`, run migrations,
   on a copy in RAM when memory allows ("DAT import in RAM").
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
   background lane, so a loaded core does not hold it. Parse the DAT
   (Logiqx, or a DB export read twice for its parents) with `quick-xml`,
   streaming, into a copy of the database in RAM when memory allows ("DAT
   import in RAM"), else into the card file. Each game is parsed outside the database's write lock and appended to `dat_stage`, a TEMP table of
   the writer connection in SQLite's temporary directory, in chunks of 2,000
   games, one short transaction per chunk; one transaction then applies the
   stage (steps 3 to 5) with the writer's bulk cache, so readers see the old
   titles or the new ones and never part of a DAT. A parse error empties the stage and changes nothing
   else. Reject anything else and move it to `dats/rejected/` with a
   `<name>.reason.txt` beside it. The apply holds the writer for the SQL
   alone; scans and imports resolve rom ids on the read connection and
   write afterwards, so one that straddles an apply records a rom id that
   is still a row, retired or not, as it would had it finished just before.
   The job reports the bytes it has read and its phase through memory and
   transient events, never the database (API.md "Live progress"), so an
   apply that holds the writer still shows its progress; an upload records
   its job on a task of its own and answers without waiting for the apply
   (API.md "Upload answers").
2. Identify the platform from the DAT header name using the table in
   PLATFORMS.md, falling back to the platform an earlier version of the same
   family was bound to. A header without a name takes the member's or file's
   stem as dropped; a DB export takes `<System> (DB Export)` from its member's
   or file's name (VERIFICATION.md "DB export"). Unknown DAT names are stored as an unbound
   `dat_versions` row the user can bind in the UI; binding re-reads the file
   from `dats/loaded/` and loads its titles.
3. Upsert `dat_versions`, then `titles` and `roms`. A newer version of the same
   DAT family on the same platform supersedes the old one, whether it comes
   as a Logiqx DAT or a DB export (VERIFICATION.md "DAT families"); other
   families on the platform stay live. Titles of the same name are reused
   across the family's versions, and entries not present in the new DAT are
   marked `retired`, never deleted. The platform's recompute then runs, on
   the copy in RAM as part of the import, or as a queued recompute job when
   the import ran on the card: it matches files of roms that retired again
   against the live roms by their stored hashes, in chunks, or marks them
   `unverified`; then, outside arcade, it matches the platform's unmatched
   files by their stored hashes (VERIFICATION.md "Matching stored hashes");
   it recomputes the picks and then queues a re-map of the platform's bound
   sources.
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
   verification there come only from the arcade catalogue, its presence
   pass and md5 check, and the import path (PLATFORMS.md "MRA catalogue").
   A manual scan of `arcade` queues the arcade catalogue instead
   (`POST /system/scan`, API.md "System"). Skip
   while a core is running; the timer goes through the same heavy lane as
   the others, so it waits for the gate too.
2. For each file, compare size and mtime with `files`. Unchanged files are
   not hashed again. An unchanged, fully hashed file with no rom is matched
   from its stored hashes, and an unchanged zip member never hashed, known by
   its CRC32 alone, is hashed once a rom of that CRC32 and size exists
   (VERIFICATION.md "Matching stored hashes"); other unchanged files are
   skipped. New or changed files are hashed in one streaming pass with the
   platform's header rule. Zip members are hashed through the decompressor,
   and the zip central-directory CRC is used as a pre-check to skip hashing
   members that cannot match anything.
3. Match by SHA1, then MD5, then CRC32 plus size. Record `verified`,
   `unverified` (no DAT match) or `misnamed` (match but wrong filename).
4. Rows the walk did not see are deleted at the end, except under a
   directory that exists but could not be listed, whose rows are kept.
5. The job's final progress is `{ platform_id, done, total, matched,
   unmatched }`: the platform's files with a rom state (`verified`,
   `misnamed`, `bad`) and those `unverified` once the scan ends.
6. Scans are resumable: hashed rows are written 256 at a time and the
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
   tracks which zips named by live MRAs are on disk. It takes the set of zips
   live MRA titles name once per run, `{dir}/{name}` lowercased, each with
   every live zip rom naming it, then lists `games/mame` and `games/hbmame`
   and, 500 zips per batch, stats each zip (size and mtime), looks up its
   existing `files` rows with the reader held for that lookup only, and
   writes the batch in one transaction. Rows are matched to a zip ignoring
   ASCII case, as exFAT names files (the `files_rel_lower` index), and keep
   the spelling they were written with. A member row's zip is its
   `rel_path` up to the first `.zip#`, in any case. The rules per zip:
   - Member rows (`dir/name.zip#member`, written by the import path) stand
     for the zip. While every one carries the zip's mtime they are never
     touched. When the mtime moved, the zip's central directory is read
     (never decompressed): a member with the same size and CRC32 keeps its
     hashes and state and takes the new mtime; one whose size or CRC32
     changed gets them recorded, loses its md5 and sha1, keeps `rom_id` and
     becomes `unverified` until an import or md5 check promotes it again; a
     member no longer in the zip loses its row. A zip that cannot be read is
     logged and keeps every row as it was.
   - Otherwise a zip a live MRA names gets one presence row,
     `dir/name.zip`, `unverified`, no hashes, `rom_id` the lowest live zip
     rom naming it. It is left as it is while the zip's size and mtime are
     unchanged and its `rom_id` is one of the live zip roms naming the zip,
     so a row `verify_siblings` promoted under any of those MRAs stays
     `verified`; any other change rewrites it `unverified`. A presence row
     is removed once member rows exist for its zip or no live MRA names the
     zip, so the rows follow MRAs added and removed even when no zip
     changed. A second spelling of one zip's presence row is removed.
   - Only a stat is needed to track presence; the central directory is read
     only for member rows of a changed zip. A zip that cannot be stated is
     logged and its rows are kept.
   A directory that exists but cannot be listed (an I/O or permission
   error) is logged and skipped for the run: nothing under it is recorded
   or pruned. The pass then walks each directory's rows a page at a time and
   deletes those whose zip is neither listed nor on disk, keeping a row
   whose zip's existence cannot be checked, and clearing their
   `import_log` references in the same statement set, so a zip removed from
   disk drops out of `have`. Memory holds the directory listing's names, the
   live MRA zip set and one batch. It never records a row without a
   `rom_id`, and never verifies anything itself: a DAT-sourced arcade title
   is verified only when a zip is imported (PLATFORMS.md "MRA import").

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
   per-platform hit rates. The fuzzy and size-only tiers of
   VERIFICATION.md "Pre-download matching" do not count toward the rate.
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
   confidence where one exists, and the further candidate roms of every tier
   in `torrent_candidates` (VERIFICATION.md "Pre-download matching"). Move
   the file to `sources/loaded/` and emit `source.changed`. A `.torrent` is
   not told to the client until something is wanted. Binding and mapping
   share one pass over the files, and a source with nothing stored gets its
   matches written straight. When a DAT loads titles for a platform, the
   rebind of step 4 runs and a background `remap_sources` job maps every
   source bound to that platform again; the same job is queued when a DAT is
   retired, when an arcade catalogue run stores or retires titles, and, for
   every platform, at each start. A source is skipped when its `map_stamp`
   equals the platform's current stamp: its live DAT versions with their load
   times, leaving out the MRA catalogue's version, whose load time every run
   touches, the count of its live roms, and a sum of a hash of each one's id
   and effective group, so roms trading groups move it. Otherwise only the rows
   that changed are written, 2 000 per transaction, with its hit rate
   refreshed and `source.changed` sent only when its mapping changed. A row
   an import proved by hash is never overwritten, and rebinding to the same
   platform keeps it; unbinding forgets every match, proofs included.

### Wanted and transfer

1. The user marks a title as wanted. mistarr creates a download for each of
   its roms without a verified file, choosing the best `torrent_file` for it
   across bound sources, from its `torrent_files` matches and its
   `torrent_candidates` alike: a file a hash proved or a name tier matched
   before any `fuzzy` or `size` candidate, then, within that tier, a size
   match (exact, or with the platform's header on top), then the stronger
   confidence (`hash`, `name`, `base`, `fuzzy`, `size`), then the source
   with fewer open downloads. A file a `bad` download of the rom used is never chosen.
   With no such file the download is `wanted` until a source binds that has
   one. Two wanted versions may share one file.
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
   searched. A cartridge file that is a live rom of another live, non-BIOS
   entry in the wanted entry's clone group is that version: the wanted
   download becomes `bad` with "the file in this source is a different
   version: <name>", the file is recorded as proven to be that rom, and the
   wanted rom is wanted again (DATA-MODEL.md "downloads.state"). The file is
   placed and verified as that version through steps 3 to 5 for it, and the
   version's other open downloads are cancelled as redundant; when the
   library already holds that version verified, nothing is written and
   `import_log` records `skipped_existing` against the file it holds; when
   the version's place holds an unverified file, that file is never
   replaced and the staged file is quarantined instead. When another
   download of the same file placed or kept it first, a later one ends the
   same way; this is never inferred for a file inside a zip. Any other file
   is a mismatch: the download becomes
   `bad` and the file moves to `staging/quarantine/<infohash>/` beside a
   `<name>.report.txt` naming the expected rom, the actual hashes and the
   other entry it matches, if any; when the download's file was only a
   `fuzzy` or `size` candidate, the rom is wanted again on its next best
   file. An entry flagged `bios` is refused: the
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
6. Once a source has a download that placed its file (`done`, or `bad`
   after placing another version) and none queued, transferring,
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
| background | `dat_import`, `recompute_1g1r`, `source_import` | Runs. A DAT parse sleeps 20 ms every 200 entries, on top of the process's `nice` level. Held, like the heavy lane, by a manual pause; a DAT import in RAM drops its copy and starts again after it, and while a core runs its copy into RAM and back rests as long as each 1 MiB step took. |
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
| Peak RSS during scan or import | under 64 MiB, checked per job by `tests/memory.rs`; a DAT load, with its bulk cache and the connections to its copy in RAM, within 32 MiB of an idle server, a source import or remap within 20 MiB, other jobs within 12 or 16 MiB |
| tokio worker threads | 2 |
| Blocking threads (SQLite, hashing, file work) | at most 4 |
| Stack per runtime thread | 1 MiB reserved, touched pages only in RSS |
| Soft `RLIMIT_DATA` | `[memory] data_limit_mib`, 192 MiB, never below 64 |
| SQLite page cache | 2 MiB, 1 MiB on each of the two connections; the writer's rises to 8 MiB while a DAT load applies its stage, a source import, resolve, rebind or re-map first keys new roms (one committed batch of 1 000 per transaction), or a source binds (`db::bulk`) |
| SQLite other | `mmap_size = 0`, `temp_store = FILE` under `/tmp/mistarr` (`SQLITE_TMPDIR`, set at startup and emptied of stale files; `MISTARR_TEMP_DIR` names another; RAM on the board, so temporary pages never reach the card), created with mode 0700 and refused when it is a symlink or another user's, in which case `<data>/tmp` is used and the log warns; WAL checkpoint every 256 pages, WAL cut to 1 MiB after a checkpoint, `soft_heap_limit` 8 MiB, 16 MiB while a bulk write is open |
| DAT stage | in `/tmp/mistarr` while a DAT loads, about 1.5 times the DAT's size (18 MB for 20 000 games of three roms), given back when the load ends, as the temporary database vacuums itself; when `/tmp` fills the load fails naming `/tmp/mistarr` and the database is unchanged |
| SQLite writes | one writer; async writes wait their turn on a semaphore before taking a blocking thread, so queued writers never starve reads; a DAT import in RAM holds the writer from its copy to its swap; one on the card, already on a blocking thread, takes it per staged chunk; an upload waits at most 250 ms for the writer to record its import job |
| DAT import or migration in RAM | a copy in `[memory] import_dir`, tmpfs, so it counts in `MemAvailable` and not in RSS: the database, what the import adds and the copy's rollback journal. Made only when `MemAvailable` covers the file's size and half again, three times the DATs' uncompressed size, and 32 MiB, above `[memory] import_floor_mib` (128 MiB), and dropped when `MemAvailable` falls below the floor during the load; 1 MiB write-back buffer |
| Hashing buffer | 256 KiB, one file at a time |
| Arcade catalogue | 64 MRA files per batch; only zip listings and names taken persist across batches; MRA files up to 16 MiB, streamed, inline part data never held |
| Arcade presence pass | 500 zips per batch, stat only unless import rows of a changed zip need its central directory; the listing's names and the live MRA zip set persist across batches |
| `.torrent` or `.magnet` file | 16 MiB, read whole, parsed in place |
| Browse page or search, with its total | under 100 ms on the board with every major platform's DAT loaded; `tests/browse.rs` holds a host bound and `mistarr bench-search` measures the board |
| SPA bundle, gzipped | under 200 KiB |
| Concurrent client RPC calls | 1, serialised |

The data limit is set in `main` before the runtime starts, so thread stacks
and heap both count against it. `RLIMIT_DATA` rather than `RLIMIT_AS`
because it counts only private writable memory (brk, anonymous mappings,
thread stacks), which is what a runaway allocation grows, and not the
binary, the SQLite shared-memory index or reserved address space. An
allocation past it fails and Rust aborts the process, which ends one daemon
instead of starving the MiSTer process of memory on a board without swap.

The launcher runs the daemon under `nice -n 10` where the board has it, at
the default I/O class, so work at the menu gets the disk's full share. While
a core other than the menu runs, the daemon moves every thread to the idle
I/O class by running `ionice -c 3 -p <tid>` for each entry of
`/proc/self/task`, listing again until a pass finds no new thread; threads
created later inherit the class from the thread that creates them. Back at
the menu it runs `ionice -c 0 -p <tid>` the same way, and the kernel derives
a best-effort level from `nice` again. A switch counts, and its class is
recorded, once at least one thread takes the class; a thread `ionice`
refused, almost always one that has exited, keeps the old class until the
next core change. One that fails, for example because `/proc` cannot be
listed or no thread takes the class, is logged at debug and tried again 30
seconds later, the wait doubling after each further failure up to 4 minutes,
or at once at the next change of the gate, which also resets the wait.
Without `ionice` it logs once at debug, stops switching and leaves the class
as launched. A process the daemon starts inherits the class of the thread
that forks it and is not in `/proc/self/task`, so a download client started
from the UI while a core runs is forked from a thread set back to class 0
for the launch, and every thread takes the idle class again afterwards; when
that restore fails, the recorded class is cleared and the switch is tried
again at once. The class lock is taken only on blocking threads, so a long
launch never stalls an async worker. The client's transfers slow under the
gate's rate limit instead. Heavy jobs also stop at their next file boundary
while a core runs. Heavy work has no thread of its own to lower further: it
shares the blocking pool with request handlers.

### Thread names

Runtime workers and blocking threads are named `mistarr-rt-N`. While a
blocking thread runs work, `threads::blocking` sets its `comm` to a label of
at most 15 bytes (`threads::label`: `db-read`, `db-write`, `hash`,
`scan-list`, `zip-list`, `dat-import`, `dat-save`, `source-file`,
`source-watch`, `import`, `rename`, `arcade`, `romsets`, `launch`, `detect`,
`incoming`, `io-class`) and puts the pool name back when it ends; the thread
that reaps a started rtorrent is `rtorrent-reap`, the one that rewrites
`mistarr.migrating` while migrations run is `db-migrate`, and a torrent's data is
deleted under `torrent-delete`. The board's BusyBox `top` and `ps` cannot
list threads, so read them from procfs:
`for t in /proc/$(pidof mistarr)/task/*; do echo "${t##*/} $(cat $t/comm)"; done`.

A DAT loads in one write transaction, so the WAL file, in RAM beside the copy
when the import runs there, can grow to the size of the pages that DAT touches
while it loads; it is cut back to 1 MiB at the
next checkpoint. Page memory stays within the cache either way: a load whose
dirty pages outgrow the bulk cache spills the rest to the WAL before its
commit, writing those pages more than once.

### Writes on a sync mount

The board mounts `/media/fat` with `sync,dirsync`, so each `write` or
`pwrite` there runs the file's fsync, a block device sync and a device
flush before it returns: about 25 ms on a typical card, whatever its size.
SQLite writes one page per `pwrite`, and two per WAL frame (its header, then
the page), so the time a load takes on the board is its count of write
syscalls, and the design counts those:

- SQLite's temporary files (statement journals, sorter spills, temporary
  b-trees) go to `/tmp/mistarr`, RAM on the board. Statement journals are
  the large part: every statement of a long transaction copies each page it
  changes that the transaction already dirtied, and past 64 KiB that copy
  is a file. The DAT stage is a TEMP table for the same reason; it is
  rebuilt from the file after a restart anyway.
- `db::bulk` raises the writer's page cache from 1 to 8 MiB, and the soft
  heap limit to match, for the one transaction that applies a DAT, and for
  migrations, re-map key batches and source binding, and puts both back
  after, on error or panic too. With 1 MiB the cache fills with dirty
  pages, SQLite spills them to the WAL, and the same page is written again
  each time it is changed after a spill. A connection opened meanwhile never
  lowers the process-wide heap limit under an open bulk write. The cache is
  sized to the measurement below: 8 and 16 MiB give the same count, and the
  memory budget caps it.
- The rom indexes a load does not need to update are partial (DATA-MODEL.md
  "Indexes"): md5 is indexed only for roms without a sha1, since only those
  are matched by md5, and the size and base-name indexes only for roms
  binding has keyed, which a load never does.
- Pages stay 4 KiB.

Measured on the host with `/proc/thread-self/io`, which counts the syscalls
the board would flush: the full synthetic catalogue (`synth`, every DAT rom
with CRC32, MD5 and SHA1), a 450-game, 266 KB disc DAT of a second family on
`psx` with every hash, and 3 000 unmatched `psx` files in directories of
three, a third of them tracks of that DAT
(`jobs::dat_import::sync_writes::sync_writes_on_the_bench_catalogue`):

| Configuration | Load: writes to the database and WAL | Load: to temporary files | Recompute |
|---|---|---|---|
| 1 MiB cache, full rom indexes, temporary files on the card | 14 847 (38.4 MB) | 62 876 (129.9 MB) | 263 to the database and WAL, 464 temporary, 4.7 s |
| 8 MiB bulk cache, partial rom indexes, temporary files in RAM | 5 785 (15.8 MB) | none on the card | 257, none on the card, 0.2 s |
| the same with full rom indexes | 8 098 (22.2 MB) | none on the card | 257 |
| the same with a 1 MiB cache | 9 996 (25.2 MB) | none on the card | 257 |

At 25 ms a write, the load's 77 723 card writes of the first row take about
32 minutes on the board and the 5 785 of the second about 2.4 minutes. The
4.7 s of the first row's recompute is a lookup of each disc directory's
tracks that reads every file row of the platform; `files::in_directory`
seeks one directory's range of the `(platform_id, rel_path)` key instead.
`load_writes_alone` in the same module holds a 450-game load on a tenth of
the catalogue under 1 500 writes to the database and WAL (about 1 300; 2 000
with a 1 MiB cache), in a test process of its own since the heap limit is
process-wide.

Every write transaction commits through `db::commit`, which first refreshes
the clone groups its writes touched in `title_groups` (DATA-MODEL.md
"Derived tables"). Browse and the platform counts then read one indexed row
per group; a page and its total cost about as much as reading the page, and
the refresh adds about 0.1 s on the host to a 15 000-game DAT's first load.

A search of three or more characters asks `title_search` for the browsed
platform's sentinel-wrapped id and the term in one `MATCH`, adds the
platform's groups whose parent title is on another platform through the
partial `title_groups_split` index, and confirms each candidate with
`LIKE`. Shorter searches run `LIKE` over the platform's `title_groups_name`
range, whose cost follows the platform's group count. The default
(`titles::SEARCH_SHAPE`) is the shape with the best worst case on the board
with real DATs loaded; `mistarr bench-search` compares it with `LIKE` alone
and the trigram index without the platform filter. Board medians in ms, 16
DATs and 41 000 titles loaded, warm cache:

| platform | search | groups | like | fts | fts-platform |
|---|---|---|---|---|---|
| nes | none | 3699 | 87.3 | 87.1 | 87.2 |
| nes | `the` | 335 | 48.3 | 78.3 | 48.5 |
| nes | `sta` | 85 | 53.7 | 52.8 | 39.7 |
| nes | `man` | 127 | 46.5 | 49.5 | 36.7 |
| nes | `super` | 197 | 57.3 | 62.1 | 46.8 |
| nes | `vex` | 0 | 52.4 | 24.9 | 28.6 |
| snes | `the` | 233 | 25.3 | 69.0 | 31.2 |
| snes | `super` | 351 | 32.8 | 49.1 | 43.3 |
| psx | none | 6345 | 42.6 | 42.5 | 42.6 |
| psx | `sta` | 572 | 59.1 | 38.0 | 40.3 |
| psx | `man` | 271 | 57.5 | 37.9 | 33.6 |
| psx | `world` | 135 | 73.3 | 48.4 | 53.1 |
| psx | `super` | 208 | 89.2 | 68.8 | 65.2 |
| psx | `vex` | 0 | 89.7 | 41.7 | 46.1 |

Searches of one or two letters take the same path in every shape. The worst
case over three or more letters was 89.7 ms for `LIKE`, 78.3 ms for the plain
index and 65.2 ms with the platform filter. On the host every shape takes
under 3 ms for the same data and the ranking does not carry over. Keeping
`title_search` current costs
about 1 s on the host per 15 000-game first load, against 0.5 s for the load
without it.

### DAT import in RAM

A load changes pages all over the file, and on the card each costs about
three flushed writes, so a DAT import, a bind and a re-import run on a copy
of the database in RAM and write it back whole (`db::ram`):

1. The job takes the write turn and the writer and holds them to the end, so
   no other write reaches the file meanwhile; async writes queue on the
   semaphore, and reads keep running on the card file. An upload still
   answers: it waits at most 250 ms to record its job, which is recorded
   when the import ends.
2. It checkpoints the WAL with TRUNCATE, so the file is complete and its WAL
   empty, and checks, in order, that no other process has the database open
   (step 6), that `[memory] import_dir` is usable and that memory allows.
   The directory must be, or become, one of mode 0700 that this user owns
   and that is not a symlink, as SQLite's temporary directory must, on tmpfs
   or ramfs by its `statfs` type, and not on the database's file system.
   `MemAvailable` from `/proc/meminfo` must cover the copy's need on top of
   `[memory] import_floor_mib`, 128 MiB by default, left for MiSTer Main and
   a running core; `import_dir` must have the need free, and the card the
   file's size. The need is the file's size and half again, for the copy's
   journal and pages freed and reused, three times the uncompressed size of
   the file's DATs, for the rows they stage and keep, and 32 MiB. Short of
   any, the import runs on the card, and the job's progress, as `reason`,
   and the log at info say why. A floor of 0 is allowed and warned about at
   startup.
3. SQLite's backup copies the file through the held writer into
   `<import_dir>/import-<key>-<job>/mistarr.db`, 1 MiB a step. The key hashes
   the database's path, so servers of two data directories never touch each
   other's copies. SQLite reads the source itself: a descriptor of the card
   file opened and closed beside its connections would drop their POSIX locks.
4. Every member of the file loads into the copy, on a pair of connections of
   its own with a rollback journal and no syncs (`Db::open_copy`), and then
   each platform that loaded titles has its recompute there: rematching, the
   picks, `title_groups` and the search index. A rollback journal holds only
   the pages a transaction overwrites, where a WAL would hold every page the
   load writes. The follow-up is the platform's re-map. The job's progress
   goes to the copy's job row and out as `job.progress`, with the phases
   "copying the database to memory", the load's own, "matching" and
   "picking" for the recompute, and "writing the database to the card". A
   file that loads nothing is not written back.
5. The copy is closed, put back in WAL mode so the card file opens without a
   write, and written to `mistarr.db.new` beside the database in writes of
   exactly 1 MiB, then synced.
6. The swap waits, up to 30 s, until no other process has the database or
   its `-wal` or `-shm` open, from `/proc/<pid>/fd`. SQLite in such a
   process, closing its last connection to the old file, would remove the
   `-wal` and `-shm` names the new file uses. Still held, the copy is dropped
   and the import runs on the card. The scan cannot see another user's
   processes, nor the file opened through another path such as a bind mount.
7. With the reader held too, the old WAL is checkpointed again and must be
   empty and both connections close. The old `-wal` and `-shm` are removed
   and an empty `mistarr.db.swap` marker is written and synced. The swap then
   takes three steps, each followed by a sync of the directory: `mistarr.db`
   renamed to `mistarr.db.old`, `mistarr.db.new` renamed to `mistarr.db`, and
   `mistarr.db.old` and the marker removed. Both connections then open on the
   file named `mistarr.db`, never creating one. A reader sees the old file
   or the new one, never part of either.
8. The working directory goes on every way out. At startup, before the
   database opens, a swap cut short is finished, `mistarr.db.new` is removed
   when `mistarr.db` stands beside it, this database's working directories
   are removed, and a data directory that lists `mistarr.db` twice, which
   only a damaged file system does, is warned about.

exFAT on Linux rewrites a renamed file's directory entries in place, one
synced write each, and finds a name by its hash and length before the name
itself. A power cut inside a rename can therefore leave neither the old
name nor the new one readable. The start reads which step it was from the
files it can read, and never creates a database while `.old`, `.new` or the
marker is present or was at the start:

| Cut during | Files readable | At the next start |
|---|---|---|
| the check, the copy or the import | `mistarr.db`, WAL empty | opened as it was; the job re-runs, as an interrupted DAT import does |
| the write-back | `mistarr.db`, a partial `.new` | `.new` removed unread |
| after the write-back, before the first rename | `mistarr.db`, a whole `.new`, perhaps the marker | `.new` and the marker removed; the import or migration runs again |
| the first rename | a whole `.new`, the marker | `.new`, synced before the swap began, passes SQLite's `quick_check` and is renamed to `mistarr.db` |
| after the first rename | `.old`, a whole `.new`, the marker | `.new` checked and renamed to `mistarr.db`, `.old` removed |
| the second rename | `.old`, the marker | `.old` renamed back; the import or migration runs again |
| after the second rename | `mistarr.db` (new), `.old`, the marker | `.old` and the marker removed |
| after the removals | `mistarr.db` (new) | opened |
| a rename of the start's own recovery | the marker alone, or nothing | the start refuses, naming the database; the card needs checking, and `mistarr.db.prev` holds the last upgrade's copy |

A `.new` with no `mistarr.db` beside it is never removed: a `.new` that
fails the check stops the start with a message naming it, to be moved aside
or kept for recovery. The old WAL is removed before the renames because it
is empty and nothing writes it after the checkpoint, so removing it loses
nothing and the new file never meets frames written for the old one.
`db::ram::tests::a_start_after_a_crash_at_any_step_of_the_swap_opens_one_whole_database`
builds each state and starts through `app::open_db`.

Shutdown is checked between members, every 500 games, between copy steps
and between written chunks, and leaves the card file untouched and the job
queued. A manual pause during the copy or the import drops the copy, waits,
and starts again; during the write-back it lets the write-back finish. A
core starting does not hold the import, which is on the background lane,
and the write-back is not deferred for it: waiting for the core to exit
would hold every write for the length of a game, against a burst of about
one write per MiB. While a core runs, the copy into RAM and the write-back
rest after each 1 MiB step as long as the step took, at least 20 ms and at
most 1 s, so they keep at most half of a CPU.

`MemAvailable` is read again between members, every 500 games and between
recompute chunks; below the floor the copy is dropped and the import runs
on the card. Running out of memory or room while copying or importing
(`ENOSPC`, `ENOMEM`, `SQLITE_FULL`, `SQLITE_NOMEM`, which the load reports
as a typed `NoRoom` error), or finding no room on the card for
`mistarr.db.new`, drops the copy the same way. Any other failure fails the
job with the card file untouched. A swap whose connections cannot reopen
after the renames fails the job with `Reopen`, never a fallback: the
connections then fail every statement and mistarr must restart.

Nothing else may write the database while a server runs: a write from
another process during the hold would be lost with the old file. The data
directory's lock keeps a second server out, and
`mistarr doctor --rebuild-groups` takes the same lock and refuses while a
server holds it.

Startup migrations take the same path: before the server opens its
connections, when a migration is pending and memory allows, the downgrade
guard reads the card file, the copy is migrated, written back and swapped
in the same way; otherwise the open migrates the card file itself. A
migration that fails on the copy leaves the card file byte for byte as it
was, and the open then runs it on the card, where it fails or succeeds as
it would have.

Measured on the host as the write-sync tests count them, in a release
build, on the bench catalogue with 3 000 unmatched `psx` files, loading a
`psx` DAT with its recompute
(`jobs::dat_import::sync_writes::in_place_and_in_ram_on_the_bench_catalogue`):

| DAT | Database before, after | On the card: writes | In RAM: card writes | Host time on the card, in RAM |
|---|---|---|---|---|
| 450 games | 47.8, 48.4 MB | 6 042 | 55 | 0.39 s, 0.47 s |
| 10 000 games | 47.8, 61.0 MB | 43 857 | 67 | 1.4 s, 1.9 s |

The card writes in RAM are one per MiB of the file and about eight more:
reopening maps SQLite's shared-memory file, writing a byte to each of its
4 KiB pages. At 25 ms a write, the 10 000-game load takes about 18 minutes
on the card and 1.7 s of flushes in RAM, plus the 61 MB transfer, a few
seconds at the card's 10 to 20 MB/s, and the import's own time on the
board's CPU. The last migration on the same catalogue, with half its roms
lacking a sha1, writes the card 1 254 times in place and 51 times in RAM
(`db::ram::tests::the_last_migration_in_place_and_in_ram`); with every rom
keyed, 6 844 times in place and 62 in RAM
(`migration_writes_on_the_bench_catalogue`).
`a_load_in_ram_writes_the_card_about_once_per_mebibyte` holds a 450-game
load on a tenth of the catalogue to one write per MiB plus 16.

The copy's size in RAM for a 50 MiB DAT of about 198 000 games loaded into
an empty database peaks at 145 MiB, the file it becomes, against a need of
182 MiB (`tests/memory.rs`, which samples the working directory): 2.9 bytes
per byte of DAT, hence the factor of three. A WAL in place of the rollback
journal peaks at 253 MiB on the same load.

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
import_dir = "/tmp/mistarr"  # tmpfs directory, private to mistarr, a DAT import or migration copies the database into
import_floor_mib = 128      # MemAvailable kept free beyond the copy; short of it, work runs on the card
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
