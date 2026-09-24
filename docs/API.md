# HTTP API

Base path `/api/v1`. JSON in and out. When `server.api_key` is set every API
request needs it in the `X-Api-Key` header or, for `EventSource`, which cannot
set headers, the `apikey` query parameter; otherwise the answer is 401. All
list endpoints take `?limit=&offset=` (default 100, capped at 1000) and return
`{ items: [...], total: n }`. Errors are `{ error: { code, message } }` with an
appropriate status; codes are `bad_request`, `unauthorized`, `not_found`,
`method_not_allowed`, `not_implemented` and `internal`. A documented route whose
work package has not landed answers 501 `not_implemented`. The SPA is served
from `/` and every unknown non-API path returns `index.html`; unknown paths
under `/api` return 404 JSON.

## System

| Method | Path | Purpose |
|---|---|---|
| GET | `/system/status` | Version, uptime, client kind and reachability, CORENAME, paused state, disk free, RSS. |
| GET | `/system/wizard` | Which first-run steps are complete. |
| POST | `/system/scan` | Enqueue a library scan. Body `{ platform_id? }`. |
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
| GET | `/platforms/{id}/titles` | Rows from `title_groups`. Filters: `q`, `have` (yes/no/any), `wanted`, `region`, `flags`, `sort` (name/have/recent). |
| GET | `/titles/{id}` | The group: every variant with its roms, file states, available torrent_files, the 1G1R pick and its art URL. |
| POST | `/titles/{id}/want` | Mark the 1G1R pick wanted, or a specific variant with `{ variant_id }`. |
| DELETE | `/titles/{id}/want` | Unmark. Cancels a not-yet-started download. |
| POST | `/titles/{id}/rename` | Apply the canonical name to a `misnamed` file. |

Browse filters: `q` is a case-insensitive substring of the base name; `have`
and `wanted` take `yes`, `no` or `any` (also `true` and `false`); `region`
keeps groups with a variant of that region; `flags` is a comma list and keeps
groups with a variant carrying every listed flag. Variants carrying a flag in
`prefs.hide` do not count unless `flags` names it, so BIOS and beta entries
appear only when asked for. `sort=recent` puts groups whose newest entry was
added last first. Items are the `title_groups` row `{ parent_id, platform_id,
base_name, name, pick_id, pick_name, variants, have_verified, wanted,
has_pick }` plus `art` for the pick, or the parent without one.

`/titles/{id}` takes any title of the group and answers `{ parent_id,
platform_id, base_name, pick_variant_id, art, variants }`. Each variant is `{
id, name, regions, languages, revision, flags, is_1g1r_pick, wanted, retired,
inferred, dat_version_id, torrent_files_available, roms }`, live variants
first, and each rom is `{ id, name, size, crc32, md5, sha1, status, file_id,
file_state, file_path }` for its best file, verified first. `want` and
`DELETE want` answer with the same body. `want` is a 400 when the variant is
not in the group, is retired or is a BIOS entry, or when no variant is
selectable and none was named.

## DATs

| Method | Path | Purpose |
|---|---|---|
| GET | `/dats` | Loaded and unbound dat_versions. |
| POST | `/dats/upload` | multipart; same handling as dropping into `dats/`. |
| DELETE | `/dats/{id}` | Retire; files keep their provenance. |

`/dats` items are `dat_versions` rows, newest first: `{ id, platform_id,
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

## Downloads

| Method | Path | Purpose |
|---|---|---|
| GET | `/downloads` | Active and recent, with progress. `?state=` filter. |
| POST | `/downloads/{id}/retry` | `failed` back to `queued`. |
| DELETE | `/downloads/{id}` | Cancel. |
| GET | `/imports` | import_log, newest first. |

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
| `import.done` | `{ title_id, file_id, action }` |
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
