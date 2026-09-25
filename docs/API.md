# HTTP API

Base path `/api/v1`. JSON in and out. When `server.api_key` is set every API
request needs it in the `X-Api-Key` header or, for `EventSource`, which cannot
set headers, the `apikey` query parameter; otherwise the answer is 401.
Every API request other than `GET`, `HEAD` and `OPTIONS` must also come from
the SPA's own origin: it needs the header `X-Mistarr: 1`, which the SPA
sends on every request and a page on another site cannot set without a
preflight, and it is refused when `Sec-Fetch-Site` is `cross-site` or an
`Origin` header is present and does not name the request's `Host`. Against
DNS rebinding, its `Host` must also be an IP literal, `localhost`, a name
ending in `.local`, `.lan` or `.localhost`, the board's own host name, or a
name in `server.allowed_hosts`, where `*.name` allows every subdomain; any
port is ignored. Refused requests answer 403 `forbidden`, checked after the
API key. `GET`, `HEAD` and `OPTIONS` are not checked. All
list endpoints take `?limit=&offset=` (default 100, capped at 1000) and return
`{ items: [...], total: n }`. Errors are `{ error: { code, message } }` with an
appropriate status; codes are `bad_request`, `unauthorized`, `forbidden`,
`not_found`, `method_not_allowed`, `conflict`, `busy`, `not_implemented`,
`unavailable` and `internal`. A
documented route whose work package has not landed answers 501
`not_implemented`. The SPA is served
from `/` and every unknown non-API path returns `index.html`; unknown paths
under `/api` return 404 JSON.

## System

| Method | Path | Purpose |
|---|---|---|
| GET | `/system/status` | Version, uptime, client kind and reachability, CORENAME, paused state, disk free, RSS, CHD decoding speed. |
| GET | `/system/wizard` | Which first-run steps are complete. |
| POST | `/system/wizard/done` | The user finished or dismissed the wizard; it stops opening by itself. Returns the wizard body. |
| POST | `/system/scan` | Enqueue a library scan. Body `{ platform_id? }`. |
| POST | `/system/cores` | Detect installed cores again, for the wizard's detected-cores step. |
| POST | `/system/pause` / `/system/resume` | Manual scheduler gate, overrides CORENAME until CORENAME next changes; `pause` holds the heavy and background lanes; `resume` ("Run now") also ends once the heavy queue drains. Returns the status body. |
| GET | `/system/jobs` | Queued, running and paused jobs with progress. |
| GET | `/system/jobs/recent` | The last 10 finished jobs, such as scans, arcade catalogues, DAT imports, recomputes, CHD decoding and imports. |
| POST | `/system/client/start` | Start an installed client that is not running: `{ kind }`, `transmission` or `rtorrent`. Returns the status body. |
| GET | `/system/settings` / PUT | The config subset that is editable at runtime. |

`/system/status` body:

```json
{
  "version": "0.0.1", "uptime_secs": 12,
  "client": { "kind": "transmission", "url": "http://127.0.0.1:9091/transmission/rpc",
              "reachable": true, "version": "4.0.5", "rtorrent_on_path": false,
              "transmission_on_path": true, "transmission_service": true,
              "transmission_opt_in": true, "checked_at": 1700000000 },
  "corename": "MENU", "paused": false, "pause_reason": null, "override": null,
  "waiting": [],
  "disk_free_bytes": 1000000, "dats_dir": "/media/fat/mistarr/dats",
  "rss_bytes": 1000000, "launch": "ready", "chd_decode_bytes_per_sec": null
}
```

`client` is `null` before the first detection and has `kind: null` when no
client answered. `rtorrent_on_path` and `transmission_on_path` say whether
each executable is on `PATH`, `transmission_service` whether
Buildroot_MiSTer's init script for it exists and `transmission_opt_in`
whether its opt-in directory does (DOWNLOAD-CLIENTS.md "Starting a stopped
client"). `corename` is `null` when the file does not exist.
`pause_reason` is `"core"`, `"manual"` or `null`; `override` is `"paused"`,
`"running"` or `null`. `waiting` lists the queued and paused jobs of the
lanes that are held, heavy lane first, oldest first, as
`{ id, kind, state, detail }`, where `detail` is the file name or platform
the job is about or `null`. Running jobs are not in it. A running core holds
the heavy lane; a manual pause holds the heavy and background lanes. It is
empty while nothing is held. `disk_free_bytes` is for the filesystem holding the data directory.
`dats_dir` is the directory watched for DAT files, from `[paths]`.
`launch` is `"ready"`, `"disabled"` when `prefs.launch` is off, or
`"unavailable"` when MiSTer Main's command FIFO does not exist.
`chd_decode_bytes_per_sec` is the speed of the last CHD decode, decoded
bytes per second of decoding time with pauses left out, or `null` before the
first.

`/system/wizard` body: `{ paths, dats, client, sources, open_on_start }`, all
booleans. `paths` is true when the games directory exists, `dats` when any DAT
version was ever loaded, `client` when detection found a client, `sources`
when any source exists, and `open_on_start` while no DAT was ever loaded and
the wizard was never finished or dismissed (`POST /system/wizard/done`,
stored in `settings` as `wizard.dismissed`). Once it is false the SPA shows
the incomplete steps as a checklist instead of redirecting.

`/system/scan` answers `{ job_id, arcade_job_id }`. A scan of `arcade` queues
no library scan, since arcade presence and verification come from the arcade
catalogue: `job_id` is `null` and, as with a scan of every platform,
`arcade_job_id` is the queued catalogue job when the scan covers arcade and
there is an `_Arcade` directory or stored MRA titles to catalogue, else
absent. Scanning any other platform sets `job_id` to its scan job and leaves
`arcade_job_id` absent. `/system/cores` answers `{ platforms, arcade_job_id
}`: the ids of platforms whose core is installed, and the queued arcade
catalogue or `null`.

`/system/jobs` items: `{ id, kind, lane, payload, state, progress, reason,
created_at, updated_at }`, where `lane` is `heavy`, `background` or `light`
(ARCHITECTURE.md "Pausing for the core"), `state` is `queued`, `running` or
`paused`, and `reason` says why a job is not running: on a held lane, such as
`"Paused while NES is running"` or `"Paused by the user"`; for a queued job,
what it waits for, as for incoming files ("Incoming files"); else `null`. A
running job's `progress` is its latest live progress ("Live progress") when
it has one. A failed job's `progress` is `{ error }`. `/system/jobs/recent` answers `{
items, total }` in the same item shape, newest first, with `state` `done` or
`failed` and `reason` `null`. A finished scan's `progress` is `{ platform_id,
done, total, matched, unmatched, unidentified }` (ARCHITECTURE.md "Library
scan"); a recompute's is `{ groups, picks, matched }`, `matched` counting
files it gave a rom; a `chd_tracks` job's is `{ done, total, verified,
unmatched, not_identified }`, counting images.

### Live progress

Long jobs report progress held in memory, never written to the database, so
a job that holds the writer still reports. Each report goes out as a
`job.progress` event without an `id:` ("Events"), at most one per 250 ms in
a phase plus one at every phase change, and becomes the job's `progress` in
`/system/jobs` and `/dats/incoming`. The value stored on the row replaces it
when the job next stores progress or finishes.

A `dat_import` reports `{ file, members, done, games, phase, bytes_read,
bytes_total }`: `members` is the DATs in the file, `done` those finished,
`games` those read so far. `phase` is `indexing` (a DB export's first pass,
for its clone list), `reading`, `storing` (applying the titles), `picking`
(the 1G1R picks) or `refreshing` (the title groups); `bytes_read` of
`bytes_total` of the current DAT, uncompressed, is present while `reading`
and absent in the other phases, whose share done is unknown, so a bar of
the bytes read never moves backwards. An import on a copy of the database
in RAM (ARCHITECTURE.md "DAT import in RAM") also reports `{ file, members,
phase }` with `phase` `copying the database to memory` before those,
`matching` (with `checked` and `matched`) and `picking` while it recomputes
the platforms it loaded, and `writing the database to the card` after them,
and stores `{ file, members,
done, games, phase: "importing" }` on the copy after each DAT, which the swap
keeps. One on the card carries `reason`, why it is not in RAM in a few
words such as `not enough free memory for a copy`, the log holding the
numbers, in each
report, and stores `{ file, members, done, games, phase: "importing in
place", reason }` after each DAT. A `recompute_1g1r` reports
`{ phase: "matching", checked, matched }`, then `{ phase: "picking", matched
}`. A scan stores `{ platform_id, done, total, matched, unmatched }` as it
goes. A `chd_tracks` job reports `{ platform_id, done, total, file,
bytes_done, bytes_total }` while it decodes: `done` and `total` count images,
`file` names the image being decoded, and the bytes are its decoded share.

`/system/settings` body: `{ client, limits, prefs, scan }` with the fields of
the same sections of `mistarr.toml`. PUT takes any subset of the four sections;
each section present replaces the stored one whole, with absent fields taking
their defaults. Other keys are a 400, as is a `remote_path_map` entry whose
`remote` is blank or whose `local` is not an absolute path; `remote` is the
client's own spelling, so `C:\Torrents` or `C:/Torrents` is accepted. Saved values take precedence over
the file on later starts. Changing `client` re-runs client detection; changing
the 1G1R fields of `prefs` recomputes the picks; changing `prefs.launch`
publishes `status`; turning `scan.chd_tracks` on moves CHD images waiting
with reason `off` to `pending` and queues the `chd_tracks` job, and turning
it off moves `pending` and `no_layout` images to `off` and stops a running
job at its next slice; saving never touches the wizard's state. Settings
saved before `scan` existed leave the file's `[scan]` in force.

`/system/client/start` is a 409 `conflict` while a detected client answers,
a 409 `busy` while another start is running, a 400 when `kind` is not
installed, and a 500 `internal` naming the failure when its start command
fails, runs for more than 30 seconds, or, for rtorrent, exits at once. Otherwise it starts the client as
DOWNLOAD-CLIENTS.md "Starting a stopped client" describes, detects again
every second for up to ten seconds until the client answers, and returns the
status body, whose `client` says whether it did.

## Platforms

| Method | Path | Purpose |
|---|---|---|
| GET | `/platforms` | All platforms with core_present, counts (titles, have, wanted, unmatched_files, unidentified_files, failing_check, partial). |
| GET | `/platforms/{id}/unidentified` | Files of the platform not identified, with why. Paged by `limit` (default 100) and `offset`. |
| PUT | `/platforms/{id}` | `{ enabled }`. |
| POST | `/platforms/{id}/dat` | Bind an unbound dat_version: `{ dat_version_id }`. |

`/platforms` items are the platform row `{ id, name, core_dir, kind,
core_present, enabled }` plus `counts: { titles, have, wanted, unmatched_files,
unidentified_files, failing_check, partial }`. `titles` counts the clone groups the default browse
shows: groups with at least one live variant not flagged with a hidden flag
(only MRA groups while the platform has a live MRA title). The other group counts count among those groups, and look at every live
variant of a group, hidden ones included: `have` counts groups with a fully
verified variant, `wanted` groups with a wanted variant. `failing_check`
counts groups where a visible live MRA variant's md5 check is `mismatch` or
`missing_part`, and `partial` groups where a visible live MRA variant has
some, but not every, zip it names present; neither counts a group that has a
fully verified variant, hidden or not, so a group counted in `have` is never
in either. `unmatched_files` counts `unverified` files on disk, and is always
0 for arcade, whose state `failing_check` and `partial` report instead.
`unidentified_files` counts `unidentified` files: CHD images whose tracks are
not identified (VERIFICATION.md "CHD images").
`PUT` answers with the same item. Binding answers 202 `{ dat_version_id, platform_id, job_id }` and
the import job loads the titles, then publishes `dat.loaded`; a version that
is already bound or retired is a 400. When the job cannot load it, because its
file is gone from `dats/loaded/` or a newer version of the same DAT name is
loaded, it publishes `dat.rejected` with the reason and the version stays
unbound.

`/platforms/{id}/unidentified` answers `{ items, total }` with items `{
rel_path, size, reason }` in `rel_path` order, where `reason` is a code of
VERIFICATION.md "CHD images" "Reasons"; it is a 404 for an unknown platform.

## Catalog

| Method | Path | Purpose |
|---|---|---|
| GET | `/platforms/{id}/titles` | Rows from `title_groups`. Filters: `q`, `have` (yes/no/any), `wanted`, `region`, `flags`, `hidden` (hide/show), `sort` (name/have/recent). |
| GET | `/titles/{id}` | The group: every variant with its roms, file states, available torrent_files, the 1G1R pick and its art URL. |
| POST | `/titles/{id}/want` | Mark the 1G1R pick wanted, or a specific variant with `{ variant_id }`, and create its downloads. |
| DELETE | `/titles/{id}/want` | Unmark. Cancels the group's downloads that are not importing or finished. |
| POST | `/titles/{id}/rename` | Apply the canonical name to a `misnamed` file, `{ file_id }`. |

Browse filters: `q` is a case-insensitive substring of the base name; `have`
and `wanted` take `yes`, `no` or `any` (also `true` and `false`); `region`
keeps groups with a variant of that region; `flags` is a comma list and
requires a live variant to carry every listed flag. `hidden` is `hide`
(default) or `show`: `hide` drops variants carrying a flag in `prefs.hide`
from consideration, so BIOS and beta entries are absent by default; `show`
disables that drop for the request, independently of `flags`, so a group
whose only variants are hidden appears. `sort=recent` puts groups whose
newest entry was added last first. Items are the `title_groups` row `{
parent_id, platform_id, base_name, name, pick_id, pick_name, variants,
have_verified, wanted, has_pick }` plus `bios`, true when every live variant
is flagged `bios` so none can be wanted, and `art` for the pick, or the parent
without one. While the
platform has MRA titles (PLATFORMS.md "MRA catalogue") only they are listed,
and `/platforms` counts only them.

`/titles/{id}` takes any title of the group and answers `{ parent_id,
platform_id, base_name, pick_variant_id, art, variants }`. Each variant is `{
id, name, regions, languages, revision, flags, is_1g1r_pick, wanted, retired,
inferred, dat_version_id, torrent_files_available, availability, source,
roms }`, live variants first, and each rom is `{ id, name, size, crc32, md5, sha1, status,
file_id, file_state, file_path }` for its best file, verified first.
`availability` lists the files of bound sources that may hold a live rom of
the variant, strongest first, as `{ source_id, source_name, file_index, path,
rom_id, confidence }`, where `confidence` is `hash`, `name`, `base`, `fuzzy`
or `size` (VERIFICATION.md "Pre-download matching"), and
`torrent_files_available` counts its distinct files. `source`
is `dat` or `mra`. An MRA variant adds `mra: { setname, rbf, path,
missing_zips, md5_check, md5_detail }`, where `missing_zips` are paths
relative to `games/` and `md5_check` is `match`, `mismatch`, `missing_part`,
`refused` or `null` when not run. A Neo Geo variant adds `romset: { listed,
present }`, with `listed` `null` when `games/NeoGeo/romsets.xml` is absent,
and the group adds `bios: [{ name, present }]` for the BIOS files that file
names; these are reported only. `want` and
`DELETE want` answer with the same body. `want` is a 400 when the variant is
not in the group, is retired or is a BIOS entry, or when no variant is
selectable and none was named.

`rename` takes `{ file_id }` for a `misnamed` file of any variant of the
group, asks the platform's adapter for the file's canonical path and renames
it in place under `games/`, marking it `verified` and logging `renamed`. It
answers with the group body. It is a 404 when the file is not in the group, a
400 when the file is not `misnamed`, lives inside a zip, belongs to a BIOS
entry or needs more than a rename to load (such as a missing header), a
409 `conflict` when another file already has the canonical path, and a 500
`internal` when the file cannot be read or moved. A `files` row left at the
canonical path with no file on disk is removed.

## DATs

| Method | Path | Purpose |
|---|---|---|
| GET | `/dats` | Loaded and unbound dat_versions. |
| GET | `/dats/incoming` | Files in `dats/` not loaded yet, and rejected ones; see "Incoming files". |
| POST | `/dats/upload` | multipart; same handling as dropping into `dats/`. |
| DELETE | `/dats/{id}` | Remove a loaded version from the catalogue; files stay on disk. |
| POST | `/dats/rejected/{file}/retry` | Move a rejected file back into `dats/` and import it again. |
| DELETE | `/dats/rejected/{file}` | Delete a rejected file and its reason. |

`/dats` items are `dat_versions` rows loaded from DAT files, newest first: `{ id, platform_id,
dat_name, version, source_file, loaded_at, superseded_by, game_count, retired,
family, reason, suggested }`. `family` is the DAT family key (VERIFICATION.md
"DAT families"); `reason` says why a version is not current, `null` for a
current one; `suggested` lists, for an unbound version, the platforms its
family is current on, and is empty otherwise. `total` counts every version,
so a client pages with `limit` and `offset`. `source_file` is the name under `dats/loaded/`, which gains ` (N)` before
the extension when the name is taken. Upload takes one `file` part named
`.dat`, `.xml` or `.zip`, writes it into `dats/` and answers 202 with the
file as `/dats/incoming` lists it ("Upload answers"); the result arrives as
`dat.loaded` or `dat.rejected`. Retiring
removes a loaded version: in one transaction its titles and their roms
retire, `wanted` is cleared on those titles and their downloads in `wanted`
or `queued` are cancelled. It answers 204, or 404 for an unknown id, and
queues the platform's recompute job, which matches files of retired roms
again against the live roms by their stored hashes (or marks them
`unverified`), recomputes the picks and queues a re-map of the platform's
bound sources, which drops their hash proofs on the retired roms. A download
already transferring finishes and is placed only if its file matches a live
rom of its entry; otherwise it is quarantined with a reason saying the DAT was
removed. No file on disk is touched, and an older version of the same family
stays superseded.

`{file}` in the two `rejected` routes is a file name as `/dats/incoming`
lists it, percent-encoded; a name with a `/` or `\`, a leading `.` or the
`.reason.txt` suffix is a 400 and a name not in `dats/rejected/` a 404.
Retrying moves the file back into `dats/`, under `name (N)` when the name is
taken, deletes its `.reason.txt` and answers 202 like an
upload, so a file fixed in place in `dats/rejected/` loads again. Deleting
answers 204. Both are writes, so they need the `X-Mistarr` header and an
allowed `Host`.

## Sources

| Method | Path | Purpose |
|---|---|---|
| GET | `/sources` | All sources with state, platform, counts, seed policy, client status. |
| GET | `/sources/incoming` | Files in `sources/` not loaded yet, and rejected ones; see "Incoming files". |
| POST | `/sources/upload` | multipart `.torrent` or a `{ magnet }` body; same handling as the watched dir. |
| PUT | `/sources/{id}` | `{ platform_id?, seed_policy?, state? }` to bind, rebind, disable. |
| DELETE | `/sources/{id}` | Remove from mistarr and, if present, the client. Never deletes placed files. |
| GET | `/sources/{id}/files` | torrent_files with matched rom names. |

`/sources` items: `{ id, infohash, display_name, origin_file, platform_id,
bind_score, state, reason, seed_policy, file_count, matched_count,
total_size, client_id, added_at, suggested_platform_id }`. `matched_count`
counts files matched to a rom or holding a candidate rom. `reason` says why
a source is `resolving` or `unbound` and is otherwise `null`; `client_id` is
set once the torrent is in the client. `suggested_platform_id` is the
platform the torrent's names point at, found without any DAT
(ARCHITECTURE.md "Source import" step 4), or `null`; the SPA preselects it in
the platform picker. `seed_policy` is `"none"`, `"client"` or
`"ratio:N"` with N above 0.

`/sources/upload` takes `multipart/form-data` with one `.torrent` file part,
or a JSON body `{ magnet }`. A file that does not parse, or repeats a source
that is already loaded, is a 400. Otherwise the file is written into
`sources/` under its name, or `name (N)` when that is taken (409 `conflict`
when no such name is free), and the answer is 202 with the file as
`/sources/incoming` lists it ("Upload answers"); the import then emits
`source.changed`.

`PUT /sources/{id}` body fields are all optional. `platform_id` binds or
rebinds the source to that platform, matching its files against it only, and
`null` unbinds it and keeps it from being bound automatically after later
DAT loads until the user binds it again; both are a 400 while the source has
no file list.
`state` is `"disabled"` or `"enabled"`, which returns the source to `bound`,
`unbound` or `resolving` as its files and platform say. A disabled source
stays disabled when rebound. The answer is the updated item.

`DELETE /sources/{id}` answers 204. It removes the torrent from the client
without deleting data; a client that does not answer is a 502 and the source
is kept. A source with a download that is queued, transferring, checking or
importing is a 400; its other downloads are kept with `source_id` `null`.

`/sources/{id}/files` items: `{ file_index, path, size, rom_id, rom_name,
title_id, confidence, candidates }`, where `path` is inside the torrent,
`confidence` is `"hash"`, `"name"`, `"base"` or `null` when no rom is
matched, and `candidates` lists the further roms the file may be, strongest
first, as `{ rom_id, rom_name, title_id, confidence }`. A file counts toward
the source's `matched_count` exactly when it has a matched rom or a candidate.

## Incoming files

### Upload answers

An upload or a retry answers once the file is in place, whatever the
database writer is doing: it waits at most 250 ms for its import job to be
recorded. The body is the file as the incoming list shows it, `{ file, size,
state, reason, job_id, progress, modified }`. `job_id` is `null` when the
writer is still busy, as while a DAT applies; the job is then recorded as
soon as the writer is free, sending the usual queued `job.progress` with the
file name as `detail`, and `reason` says what the file waits for, such as
"Waiting for the DAT import of a.dat to finish.". A file the server placed
never waits to stop changing.

### Listing

`/dats/incoming` and `/sources/incoming` list the files in the watched
directory that have not loaded, by name, then those in its `rejected/`
directory, newest first. Items are `{ file, size, state, reason, job_id,
progress, modified }`. `state` is `waiting` (not picked up yet, or its job
is queued), `importing` (its job is running) or `rejected`. `reason` says
why a file waits ("Waiting for the file to stop changing." for a file
dropped into the directory that the watcher has not queued, "Waiting for the
DAT import of a.dat to finish." for other work behind a DAT import or an
uploaded file whose job waits for the writer, "Queued behind a.dat.", or
"Paused by the user" while a manual pause holds the background lane; a
running core never holds it) or why it was
rejected, from its `.reason.txt`. `job_id` and `progress` are the open
`dat_import` or `source_import` job's, `progress` live while the job
runs. A loaded file leaves this list and
appears in `/dats` or `/sources`. The SPA re-reads the list on
`job.progress` for those kinds, `dat.loaded`, `dat.rejected` and
`source.changed`.

## Downloads

| Method | Path | Purpose |
|---|---|---|
| GET | `/downloads` | Active and recent, with progress. `?state=` filter. |
| POST | `/downloads/{id}/retry` | `failed` back to `queued`. |
| DELETE | `/downloads/{id}` | Cancel. |
| GET | `/imports` | import_log, newest first. |

`/downloads` items, most recently changed first: `{ id, title_id,
title_name, platform_id, rom_id, rom_name, size, source_id, file_index,
state, progress, staged_path, error, created_at, updated_at }`. `state` is
one of `docs/DATA-MODEL.md` "downloads.state"; `progress` runs from 0 to 1;
`source_id` and `file_index` are `null` while `wanted`. `?state=` takes one
state or a comma list; an unknown name is a 400.

`retry` and `DELETE` answer the updated item. `retry` is a 409 `conflict`
unless the download is `failed`, or while another download of the same rom
is open. `DELETE` is a 409 for a download that is importing or finished; a
started download is also deselected in the client. Both are 404 for an
unknown id.

`POST /titles/{id}/want` creates one download per live rom of the variant
that has no verified file and no open download, `queued` on the best
torrent_file of a bound source (exact size first, then a name match, then
the source with fewer open downloads, then the lowest source id) or
`wanted` when no bound source has it, and emits `download.changed` for each.

`/imports` items are `{ id, at, download_id, file_id, action, detail }`,
newest first, where `action` is one of `docs/DATA-MODEL.md` "import_log" and
`detail` is the action's JSON: `rel_path`, `title_id`, `rom_id`, the staged
file name and zip `member` for a placement, plus `previous` `{ rel_path,
state, rom_id, sha1 }` for `replaced`; `path`, `expected` (the rom) and
`actual` (hashes per file or member) for `quarantined`; `from` and `rel_path`
for `renamed`. A placement of an MRA zip adds `verification` (`mra_md5`,
`dat` or `none`), `reason` when its members stay `unverified`, and
`dat_entry` for `dat` (PLATFORMS.md "MRA import"). Its quarantine adds
`missing` (member names), `md5` (the failed check) or `dat_entry`, `extra`
and `absent` (member and rom names).

## Events

`GET /api/v1/events` is a Server-Sent Events stream. Every event is
`event: <name>` with a JSON `data:` line. The SPA reconnects with
`Last-Event-ID`; the server keeps a ring of the last 256 events.

Event ids are `<epoch>-<seq>`: the epoch is fixed for the life of a server
process (lowercase hex) and `seq` counts up from 1 within it. Clients treat
ids as opaque strings. A new connection receives, in order:

1. `resync` with data `{}`, when the replay below may have gaps: the
   `Last-Event-ID` is from another epoch (the server restarted), is not a
   valid id, or is older than the oldest event in the ring. The SPA should
   then re-fetch the resources it shows.
2. The ring events after `Last-Event-ID`, or the whole ring for another
   epoch. Nothing is replayed when the header is absent.
3. One `status`, then live events.

Every event carries an `id:` except `resync`, the `status` sent on
connect and on the 30 s timer, and live progress ("Live progress"), so those
never move the client's `Last-Event-ID` and live progress is never
replayed. Each connection buffers up to 1024 undelivered events; a
connection that falls further behind is closed and, on reconnecting, gets
`resync` plus whatever the ring still holds.

| Event | Data |
|---|---|
| `status` | Same shape as `/system/status`, sent on change and every 30 s. |
| `job.progress` | `{ id, kind, state, detail, progress }`, `detail` being the file name or platform the job is about or `null`; also sent with `state: "queued"` and `progress: null` when a job is queued, and without an id for live progress |
| `dat.loaded` / `dat.rejected` | `{ dat_version_id, file, platform_id }` / `{ file, reason }`; one per DAT in a pack, `file` as dropped |
| `source.changed` | `{ source_id, state, platform_id? }` |
| `download.changed` | `{ download_id, state, progress }` |
| `import.done` | `{ title_id, file_id, action }`, one per file placed, kept or renamed; `action` as in `import_log` |
| `file.changed` | `{ file_id, state }` during scans, throttled to 10 per second, and for each row the `chd_tracks` job writes |

## Launching

| Method | Path | Purpose |
|---|---|---|
| POST | `/titles/{id}/launch` | Start title `id` (a variant, not its group) on the MiSTer. |
| POST | `/platforms/{id}/launch-core` | Start the platform's newest installed core with no game. |

Neither takes a body; everything launched comes from the database and the
SD card (ARCHITECTURE.md "Launching"). Both answer `{ core, file }`: the
`.rbf` or `.mra` loaded, relative to the SD root, and the game file handed
to the core, relative to `games/`, or `null`.

Both are a 409 `conflict` while `prefs.launch` is off, a 409 `busy` when
another launch was sent less than 3 s before (launches are also serialised),
and a 503 `unavailable` when MiSTer Main's command FIFO does not exist, is
not being read or does not take the command.

`launch` is a 404 for an unknown title and a 409 `conflict` when:

- not every live rom has a `verified`, `misnamed` or `bad` file, or, for an
  MRA title, a zip is missing or its md5 check failed;
- a disc has a track that is not `verified`, or no cue sheet whose `FILE`
  entries all exist beside it and no `.chd` or `.iso`; a `.chd` identified
  by its tracks counts, and its `#cue` row is not a cue sheet;
- the entry is a BIOS entry or a DAT entry of the arcade platform;
- no launch core of the platform is installed;
- the MRA file is no longer under `_Arcade`, or its stored path is not plain
  names below it;
- a path cannot be passed to Main: relative, not UTF-8, holding a control
  character, or making the command longer than one FIFO write.

It is a 500 `internal` with the message "the launch file could not be
written" when the MGL cannot be created in the launch directory; the log
names the directory and the error.

`launch-core` is a 404 for an unknown platform and a 409 `conflict` when no
launch core is installed or for `arcade`, whose cores start from an MRA.
Nothing is published on the event bus; the running core shows up in
`status` through CORENAME.

## Art URLs

The server never fetches art. `/titles/{id}` and each browse item include
`art: { boxart, title, snap }` as URLs the browser loads directly from the
libretro thumbnail server, built from the platform's playlist name
(PLATFORMS.md "Thumbnail playlists") and the DAT name with the characters
`&*/:\`<>?\|"` replaced by `_`, as
`<server>/<playlist>/Named_Boxarts|Named_Titles|Named_Snaps/<name>.png` with
both path segments percent-encoded. The SPA treats a 404 as "no art" and shows
a placeholder.
