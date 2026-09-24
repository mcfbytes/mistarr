# HTTP API

Base path `/api/v1`. JSON in and out. Optional `X-Api-Key` header when
`server.api_key` is set. All list endpoints take `?limit=&offset=` and return
`{ items: [...], total: n }`. Errors are `{ error: { code, message } }` with an
appropriate status. The SPA is served from `/` and every unknown non-API path
returns `index.html`.

## System

| Method | Path | Purpose |
|---|---|---|
| GET | `/system/status` | Version, uptime, client kind and reachability, CORENAME, paused state, disk free, RSS. |
| GET | `/system/wizard` | Which first-run steps are complete. |
| POST | `/system/scan` | Enqueue a library scan. Body `{ platform_id? }`. |
| POST | `/system/pause` / `/system/resume` | Manual scheduler gate, overrides CORENAME until cleared. |
| GET | `/system/jobs` | Running and queued jobs with progress. |
| GET | `/system/settings` / PUT | The config subset that is editable at runtime. |

## Platforms

| Method | Path | Purpose |
|---|---|---|
| GET | `/platforms` | All platforms with core_present, counts (titles, have, wanted, unverified). |
| PUT | `/platforms/{id}` | `{ enabled }`. |
| POST | `/platforms/{id}/dat` | Bind an unbound dat_version: `{ dat_version_id }`. |

## Catalog

| Method | Path | Purpose |
|---|---|---|
| GET | `/platforms/{id}/titles` | Rows from `title_groups`. Filters: `q`, `have` (yes/no/any), `wanted`, `region`, `flags`, `sort` (name/have/recent). |
| GET | `/titles/{id}` | The group: every variant with its roms, file states, available torrent_files, the 1G1R pick and its art URL. |
| POST | `/titles/{id}/want` | Mark the 1G1R pick wanted, or a specific variant with `{ variant_id }`. |
| DELETE | `/titles/{id}/want` | Unmark. Cancels a not-yet-started download. |
| POST | `/titles/{id}/rename` | Apply the canonical name to a `misnamed` file. |

## DATs

| Method | Path | Purpose |
|---|---|---|
| GET | `/dats` | Loaded and unbound dat_versions. |
| POST | `/dats/upload` | multipart; same handling as dropping into `dats/`. |
| DELETE | `/dats/{id}` | Retire; files keep their provenance. |

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

| Event | Data |
|---|---|
| `status` | Same shape as `/system/status`, sent on change and every 30 s. |
| `job.progress` | `{ id, kind, progress }` |
| `dat.loaded` / `dat.rejected` | `{ dat_version_id?, file, reason? }` |
| `source.changed` | `{ source_id, state, platform_id? }` |
| `download.changed` | `{ download_id, state, progress }` |
| `import.done` | `{ title_id, file_id, action }` |
| `file.changed` | `{ file_id, state }` during scans, throttled to 10 per second |

## Art URLs

The server never fetches art. `/titles/{id}` includes `art: { boxart, title,
snap }` as URLs the browser loads directly from the libretro thumbnail server,
built from the platform's playlist name and the DAT name with the characters
`&*/:\`<>?\|"` replaced by `_`. The SPA treats a 404 as "no art" and shows a
placeholder.
