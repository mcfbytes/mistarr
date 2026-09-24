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

A new connection first receives the ring events newer than `Last-Event-ID`
(the whole ring if the id is newer than any the server issued, as after a
restart), then one `status`, then live events. Every event carries an `id:`
except `status` sent on connect and on the 30 s timer, so those never move
the client's `Last-Event-ID`. A connection that falls more than 256 events
behind is closed; reconnecting replays what it missed from the ring.

| Event | Data |
|---|---|
| `status` | Same shape as `/system/status`, sent on change and every 30 s. |
| `job.progress` | `{ id, kind, state, progress }` |
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
