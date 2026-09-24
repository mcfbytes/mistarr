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
2. DATs: drop zone and the watched-directory path. Lists the files still in
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
scan button. Platforms whose core is absent are in a collapsed section. While
the wizard reports a missing DAT, client or source, a "Setup not finished"
card lists what is missing and links to the wizard.

**Browse** (`/p/{id}`). A Start core button beside the platform name,
except on arcade. Poster grid of `title_groups`, cover from the libretro
URL with a placeholder on 404. Filters: search, have / missing / wanted,
region, a "Show hidden" checkbox, and a flags multi-select that requires
every checked flag. Each card shows the 1G1R pick name, a have indicator, and
a want toggle. Infinite scroll in pages of 60.

**Title** (`/t/{id}`). Every variant in the group with region, revision,
flags, file state, and which sources have it: one line per file, as "nova.nes
in Example Pack (name guess)", with the confidence read as "name match",
"name and size", "name guess" or "size only", or "None available". Want per variant. Play for a
variant whose files are all in the collection. Rename action for misnamed
files. Art tabs: boxart, title, snap.

Play and Start core are disabled while launching is unavailable, with the
reason as a line of text on the page rather than a tooltip: launching is
turned off in settings, mistarr is not running on a MiSTer, or no core for
the platform is installed.

**Activity** (`/activity`). Downloads with per-file progress bars, imports
log, and queued and running jobs with their lane and hold reason. Live over
SSE.

**Sources** (`/sources`). Table of sources: name, platform, state, file count,
matched count, seed policy, client status. Bind and disable actions.
Unbound sources have a platform picker and, when the names suggest one, a
"Bind to" button for the suggested platform. Above the table, the files still
in `sources/` and this session's uploads, as in the wizard.

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
known then says to look at the lists.
No polling from the browser. The SSE connection shows a banner when
disconnected and reconnects with backoff.

## Language

Follow PRINCIPLES.md section 5. Buttons say "Want", "Scan", "Bind",
"Rename", "Play", "Start core". The empty state on Sources says: "No sources yet. Place a .torrent
or .magnet file in `<path>` or drop one here." and nothing more.
