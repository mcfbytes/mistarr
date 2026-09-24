# Web UI

Svelte 5, Vite, TypeScript, no component library. Plain CSS with custom
properties, dark by default with a light theme following
`prefers-color-scheme`. Built with `npm run build` into `web/dist` and
embedded in the server binary by `rust-embed`. Gzipped bundle must stay under
200 KiB; check in CI.

The UI is used from a phone on the couch as often as from a desktop. Every
screen works at 360 px wide with a 16 px gutter and no horizontal scroll.

## Screens

**Wizard** (first run, or `/wizard`). Four steps, each skippable:
1. Paths: confirm root and games directory, show detected cores.
2. DATs: drop zone and the watched-directory path. Shows loaded DATs and the
   platform each bound to.
3. Client: detection result. Start rtorrent offer on stock. Remote path map.
4. Sources: drop zone and the watched-directory path. Seed policy explained
   with its default shown. Nothing about where to obtain files.

**Platforms** (`/`). One card per platform with core present, counts, and a
scan button. Platforms whose core is absent are in a collapsed section.

**Browse** (`/p/{id}`). A Start core button beside the platform name,
except on arcade. Poster grid of `title_groups`, cover from the libretro
URL with a placeholder on 404. Filters: search, have / missing / wanted,
region, a "Show hidden" checkbox, and a flags multi-select that requires
every checked flag. Each card shows the 1G1R pick name, a have indicator, and
a want toggle. Infinite scroll in pages of 60.

**Title** (`/t/{id}`). Every variant in the group with region, revision,
flags, file state, and which sources have it. Want per variant. Play for a
variant whose files are all in the collection. Rename action for misnamed
files. Art tabs: boxart, title, snap.

Play and Start core are disabled while launching is unavailable, with the
reason as a line of text on the page rather than a tooltip: launching is
turned off in settings, mistarr is not running on a MiSTer, or no core for
the platform is installed.

**Activity** (`/activity`). Downloads with per-file progress bars, imports
log, and running jobs. Live over SSE.

**Sources** (`/sources`). Table of sources: name, platform, state, file count,
matched count, seed policy, client status. Bind and disable actions.
Unbound sources have a platform picker.

**System** (`/system`). Status, client, CORENAME, paused indicator with manual
override, launch state, settings form for the runtime-editable subset
including the switch that allows launching, log tail.

## State handling

One store per API resource, hydrated on navigation and patched by SSE events.
No polling from the browser. The SSE connection shows a banner when
disconnected and reconnects with backoff.

## Language

Follow PRINCIPLES.md section 5. Buttons say "Want", "Scan", "Bind",
"Rename", "Play", "Start core". The empty state on Sources says: "No sources yet. Place a .torrent
or .magnet file in `<path>` or drop one here." and nothing more.
