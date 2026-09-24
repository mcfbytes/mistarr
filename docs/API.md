# HTTP API

Base path `/api/v1`. JSON in and out. When `server.api_key` is set every API
request needs it in the `X-Api-Key` header or, for `EventSource`, which cannot
set headers, the `apikey` query parameter; otherwise the answer is 401. All
list endpoints take `?limit=&offset=` (default 100, capped at 1000) and return
`{ items: [...], total: n }`. Errors are `{ error: { code, message } }` with an
appropriate status; codes are `bad_request`, `unauthorized`, `not_found`,
`method_not_allowed`, `conflict`, `not_implemented` and `internal`. A
documented route whose work package has not landed answers 501
`not_implemented`. The SPA is served
from `/` and every unknown non-API path returns `index.html`; unknown paths
under `/api` return 404 JSON.

## System

| Method | Path | Purpose |
|---|---|---|
| GET | `/system/status` | Version, uptime, client kind and reachability, CORENAME, paused state, disk free, RSS. |
| GET | `/system/wizard` | Which first-run steps are complete. |
| POST | `/system/scan` | Enqueue a library scan. Body `{ platform_id? }`. |
| POST | `/system/cores` | Detect installed cores again, for the wizard's detected-cores step. |
| POST | `/system/pause` / `/system/resume` | Manual scheduler gate, overrides CORENAME until CORENAME next changes. Returns the status body. |
| GET | `/system/jobs` | Queued, running and paused jobs with progress. |
| GET | `/system/settings` / PUT | The config subset that is editable at runtime. |

`/system/status` body:

```json
{
  "version": "0.0.1", "uptime_secs": 12,
  "client": { "kind": "transmission", "url": "http://127.0.0.1:9091/transmission/rpc",
              "reachable": true, "version": "4.0.5", "rtorrent_on_path": false,
              "checked_at": 1700000000 },
  "corename": "MENU", "paused": false, "pause_reason": null, "override": null,
  "disk_free_bytes": 1000000, "rss_bytes": 1000000
}
```

`client` is `null` before the first detection and has `kind: null` when no
client answered. `corename` is `null` when the file does not exist.
`pause_reason` is `"core"`, `"manual"` or `null`; `override` is `"paused"`,
`"running"` or `null`. `disk_free_bytes` is for the filesystem holding the
data directory.

`/system/wizard` body: `{ paths, dats, client, sources, open_on_start }`, all
booleans. `paths` is true when the games directory exists, `dats` when any DAT
version was ever loaded, `client` when detection found a client, `sources`
when any source exists, and `open_on_start` when no DAT was ever loaded.

`/system/scan` answers `{ job_id }`, plus `arcade_job_id` when the scan
covers every platform or `arcade` and the arcade catalogue was queued (there
is an `_Arcade` directory or stored MRA titles). `/system/cores` answers `{
platforms, arcade_job_id }`: the ids of platforms whose core is installed, and
the queued arcade catalogue or `null`.

`/system/jobs` items: `{ id, kind, payload, state, progress, created_at,
updated_at }`, where `state` is `queued`, `running` or `paused` and a failed
job's `progress` is `{ error }`.

`/system/settings` body: `{ client, limits, prefs }` with the fields of the
same sections of `mistarr.toml`. PUT takes any subset of the three sections;
each section present replaces the stored one whole, with absent fields taking
their defaults. Other keys are a 400. Saved values take precedence over the
file on later starts. Changing `client` re-runs client detection.

## Platforms

| Method | Path | Purpose |
|---|---|---|
| GET | `/platforms` | All platforms with core_present, counts (titles, have, wanted, unverified). |
| PUT | `/platforms/{id}` | `{ enabled }`. |
| POST | `/platforms/{id}/dat` | Bind an unbound dat_version: `{ dat_version_id }`. |

`/platforms` items are the platform row `{ id, name, core_dir, kind,
core_present, enabled }` plus `counts: { titles, have, wanted, unverified }`:
clone groups the default browse shows, groups with a verified variant, groups
with a wanted variant, and `unverified` files on disk. `PUT` answers with the
same item. Binding answers 202 `{ dat_version_id, platform_id, job_id }` and
the import job loads the titles, then publishes `dat.loaded`; a version that
is already bound or retired is a 400. When the job cannot load it, because its
file is gone from `dats/loaded/` or a newer version of the same DAT name is
loaded, it publishes `dat.rejected` with the reason and the version stays
unbound.

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
have_verified, wanted, has_pick }` plus `art` for the pick, or the parent
without one. While the
platform has MRA titles (PLATFORMS.md "MRA catalogue") only they are listed,
and `/platforms` counts only them.

`/titles/{id}` takes any title of the group and answers `{ parent_id,
platform_id, base_name, pick_variant_id, art, variants }`. Each variant is `{
id, name, regions, languages, revision, flags, is_1g1r_pick, wanted, retired,
inferred, dat_version_id, torrent_files_available, source, roms }`, live
variants first, and each rom is `{ id, name, size, crc32, md5, sha1, status,
file_id, file_state, file_path }` for its best file, verified first. `source`
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
| POST | `/dats/upload` | multipart; same handling as dropping into `dats/`. |
| DELETE | `/dats/{id}` | Retire; files keep their provenance. |

`/dats` items are `dat_versions` rows loaded from DAT files, newest first: `{ id, platform_id,
dat_name, version, source_file, loaded_at, superseded_by, game_count, retired
}`. `source_file` is the name under `dats/loaded/`, which gains ` (N)` before
the extension when the name is taken. Upload takes one `file` part named
`.dat`, `.xml` or `.zip`, writes it into `dats/` and answers 202 `{ file,
job_id }`; the result arrives as `dat.loaded` or `dat.rejected`. Retiring
answers 204 and recomputes the platform's picks.

## Sources

| Method | Path | Purpose |
|---|---|---|
| GET | `/sources` | All sources with state, platform, counts, seed policy, client status. |
| POST | `/sources/upload` | multipart `.torrent` or a `{ magnet }` body; same handling as the watched dir. |
| PUT | `/sources/{id}` | `{ platform_id?, seed_policy?, state? }` to bind, rebind, disable. |
| DELETE | `/sources/{id}` | Remove from mistarr and, if present, the client. Never deletes placed files. |
| GET | `/sources/{id}/files` | torrent_files with matched rom names. |

`/sources` items: `{ id, infohash, display_name, origin_file, platform_id,
bind_score, state, reason, seed_policy, file_count, matched_count,
total_size, client_id, added_at }`. `reason` says why a source is
`resolving` or `unbound` and is otherwise `null`; `client_id` is set once the
torrent is in the client. `seed_policy` is `"none"`, `"client"` or
`"ratio:N"` with N above 0.

`/sources/upload` takes `multipart/form-data` with one `.torrent` file part,
or a JSON body `{ magnet }`. A file that does not parse, or repeats a source
that is already loaded, is a 400. Otherwise the file is written into
`sources/` under its name, or `name (N)` when that is taken (409 `conflict`
when no such name is free), and the answer is 202 `{ file, job_id }`; the
import then emits `source.changed`.

`PUT /sources/{id}` body fields are all optional. `platform_id` binds or
rebinds the source to that platform, matching its files against it only, and
`null` unbinds it; both are a 400 while the source has no file list.
`state` is `"disabled"` or `"enabled"`, which returns the source to `bound`,
`unbound` or `resolving` as its files and platform say. A disabled source
stays disabled when rebound. The answer is the updated item.

`DELETE /sources/{id}` answers 204. It removes the torrent from the client
without deleting data; a client that does not answer is a 502 and the source
is kept. A source with a download that is queued, transferring, checking or
importing is a 400; its other downloads are kept with `source_id` `null`.

`/sources/{id}/files` items: `{ file_index, path, size, rom_id, rom_name,
title_id, confidence }`, where `path` is inside the torrent and `confidence`
is `"name"`, `"size"` or `null` when no rom matched.

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

Every event carries an `id:` except `resync` and the `status` sent on
connect and on the 30 s timer, so those never move the client's
`Last-Event-ID`. Each connection buffers up to 1024 undelivered events; a
connection that falls further behind is closed and, on reconnecting, gets
`resync` plus whatever the ring still holds.

| Event | Data |
|---|---|
| `status` | Same shape as `/system/status`, sent on change and every 30 s. |
| `job.progress` | `{ id, kind, state, progress }` |
| `dat.loaded` / `dat.rejected` | `{ dat_version_id, file, platform_id }` / `{ file, reason }`; one per DAT in a pack, `file` as dropped |
| `source.changed` | `{ source_id, state, platform_id? }` |
| `download.changed` | `{ download_id, state, progress }` |
| `import.done` | `{ title_id, file_id, action }`, one per file placed, kept or renamed; `action` as in `import_log` |
| `file.changed` | `{ file_id, state }` during scans, throttled to 10 per second |

## Art URLs

The server never fetches art. `/titles/{id}` and each browse item include
`art: { boxart, title, snap }` as URLs the browser loads directly from the
libretro thumbnail server, built from the platform's playlist name
(PLATFORMS.md "Thumbnail playlists") and the DAT name with the characters
`&*/:\`<>?\|"` replaced by `_`, as
`<server>/<playlist>/Named_Boxarts|Named_Titles|Named_Snaps/<name>.png` with
both path segments percent-encoded. The SPA treats a 404 as "no art" and shows
a placeholder.
