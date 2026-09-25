# Web UI

Svelte 5, Vite, TypeScript, no component library. Plain CSS with custom
properties, dark by default with a light theme following
`prefers-color-scheme`. Built with `npm run build` into `web/dist` and
embedded in the server binary by `rust-embed`. Gzipped bundle must stay under
200 KiB; check in CI.

The UI is used from a phone on the couch as often as from a desktop. Every
screen works at 360 px wide with a 16 px gutter and no horizontal scroll.

## Screens

**Wizard** (`/wizard`). Four steps, each skippable:
1. Paths: confirm root and games directory, show detected cores.
2. DATs: drop zone and the watched-directory path, taking Logiqx DATs, No-Intro
   database exports and zipped packs. Lists the files still in
   `dats/`, each waiting (with the reason), importing (with progress) or
   rejected (with the reason), then this session's uploads with their
   outcome, then the loaded DATs and the platform each bound to.
3. Client: detection result. When no client answers but one is installed,
   Start Transmission and Start rtorrent buttons, with a note that starting
   Transmission through its init script makes it start at boot. Remote path
   map with a Remove button per row; blank rows are dropped and a row
   without a remote path or with a relative local path is refused before
   saving.
4. Sources: drop zone and the watched-directory path, the files still in
   `sources/` listed as in step 2, and the added sources with their state and
   reason. Seed policy explained with its default shown. Nothing about where
   to obtain files.

The app opens the wizard on load only while `open_on_start` is true. Finish,
or leaving the wizard any other way, calls `POST /system/wizard/done`, so a
reload lands on Platforms afterwards; the "Setup not finished" card opens it
again.

**Held-jobs banner.** On every screen, and at the top of the wizard, while
`waiting` is not empty: how many jobs are held, why (the running core, which
holds scans and imports, or the user's Pause, which also holds DAT and source
imports), which ones, and a Run now button that calls `POST /system/resume`.

**Platforms** (`/`). One card per platform with core present, counts, and a
scan button. Scan shows a toast when the scan is queued and another with its
outcome when it finishes, such as "Scan of Nintendo 64: 410 matched, 2
unmatched", on whichever page is open; counts reload when a scan or
recompute ends. Platforms whose core is absent are in a collapsed section. While
the wizard reports a missing DAT, client or source, a "Setup not finished"
card lists what is missing and links to the wizard.

**Browse** (`/p/{id}`). A Start core button beside the platform name,
except on arcade. Poster grid of `title_groups`, cover from the libretro
URL with a placeholder on 404. Filters: search, have / missing / wanted,
region, a "Show hidden" checkbox, and a flags multi-select that requires
every checked flag. Each card shows the 1G1R pick name, a have indicator, and
a want toggle. Infinite scroll in pages of 60. Search runs 250 ms after
typing stops, and each request cancels the one before it. While a page is
loading a thin progress bar shows and the current results stay visible,
dimmed and marked `aria-busy`; a failed load shows an alert with Retry.

**Title** (`/t/{id}`). Every variant in the group with region, revision,
flags, file state, and which sources have it: one line per file, as "nova.nes
in Example Pack (name guess)", with the confidence read as "name match",
"hash match", "name match", "name and size", "name guess" or "size only", or
"None available". Want per variant. Play for a
variant whose files are all in the collection. Rename action for misnamed
files. Art tabs: boxart, title, snap.

Play and Start core are disabled while launching is unavailable, with the
reason as a line of text on the page rather than a tooltip: launching is
turned off in settings, mistarr is not running on a MiSTer, or no core for
the platform is installed.

**Activity** (`/activity`). Downloads with per-file progress bars, imports
log, queued and running jobs with their lane and hold reason, and a Recent
list of the last finished jobs from `/system/jobs/recent`, one line each with
its outcome and time. Live over SSE.

**Sources** (`/sources`). Table of sources: name, platform, state, file count,
matched count, seed policy, client status. Bind and disable actions.
Unbound sources have a platform picker and, when the names suggest one, a
"Bind to" button for the suggested platform. Above the table, the files still
in `sources/` and this session's uploads, as in the wizard.

**DATs** (`/dats`). Its own nav entry, between Sources and System, since
DATs arrive and fail on their own schedule like sources do. An upload
control taking several `.dat`, `.xml` or `.zip` files, the same upload as
the wizard's. The files still in `dats/` as in the wizard: waiting with the
reason, importing with progress, rejected with the reason on its own line
under the name, plus this session's uploads with their outcome. Each
rejected file has Retry, which moves it back into `dats/` so a file fixed
in place loads again, and Delete, which asks once more before removing it.
The upload note names the watched directory from `/system/status`
`dats_dir`. Then the loaded DATs with the count of every stored version, one
entry per DAT family on a platform (VERIFICATION.md "DAT families"), current
ones first and sorted by platform: name and file, platform or "Not bound",
version, game count, loaded time and state, with the family's other versions
folded under "Older versions" beside the reason each is not current. A
current version has Remove, which asks once more, saying its games leave the
catalogue while files on the card stay, before `DELETE /dats/{id}`. Focus
moves to the confirming button and back to Remove on Keep; every button's
label names its file or version, and an `aria-live` line reports each
result. Live over SSE, as in the wizard.

**System** (`/system`). Status, client with the same start offer as the
wizard, CORENAME, paused indicator with manual override, launch state,
settings form for the runtime-editable subset with the shared path map editor
and the switch that allows launching, log tail.

## State handling

One store per API resource, hydrated on navigation and patched by SSE events.
The incoming-file lists re-read `/dats/incoming` or `/sources/incoming` at
most once per burst of `job.progress`, `dat.loaded`, `dat.rejected` or
`source.changed` events, and `/sources` is re-read at most once per burst of
`source.changed`. The outcomes of this session's uploads come from the last
50 finished jobs; a resync forgets them, and an upload whose job is no longer
known then says to look at the lists. `/system/jobs/recent` is re-read once
per burst of finished jobs while Activity is open, and on a resync while
Activity is open or a scan the user queued awaits its outcome, which the
re-read list then supplies.
No polling from the browser. The SSE connection shows a banner when
disconnected and reconnects with backoff.

## Language

Follow PRINCIPLES.md section 5. Buttons say "Want", "Scan", "Bind",
"Rename", "Play", "Start core". The empty state on Sources says: "No sources yet. Place a .torrent
or .magnet file in `<path>` or drop one here." and nothing more.
