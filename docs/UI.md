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
scan button, below a banner of the platform's art. Platforms whose core is
absent are in a collapsed section. While the wizard reports a missing DAT,
client or source, a "Setup not finished" card lists what is missing and
links to the wizard.

**Browse** (`/p/{id}`). A Start core button beside the platform name,
except on arcade, both below a banner of the platform's art. Poster grid of
`title_groups`, cover from the libretro URL with a placeholder on 404.
Filters: search, have / missing / wanted, region, a "Show hidden" checkbox,
and a flags multi-select that requires every checked flag. Each card shows
the 1G1R pick name, a have indicator, and a want toggle. Infinite scroll in pages of 60. Search runs 250 ms after
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
log, and queued and running jobs with their lane and hold reason. Live over
SSE.

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

## Platform art

Each platform has an abstract backdrop with a stylised drawing of its
hardware in front, drawn as inline SVG by `web/src/lib/art/generate.ts` and
`web/src/lib/art/hardware.ts` and shown by `web/src/lib/PlatformArt.svelte`.
The art is original: the hardware is recognisable by form alone and carries
no logo, wordmark, badge, model name or printed text, no manufacturer artwork
and no brand colours (PRINCIPLES.md section 8). It ships in the bundle and
never loads an image or touches the network.

A platform id maps to a motif family that evokes its era or medium; an id
without a mapping falls back to its `kind`, and anything else to contours.
Mapped siblings take the family's compositions in turn, so neighbours in a
family differ in layout as well as colour.

| Family | Motif | Compositions | Platforms |
|---|---|---|---|
| pixel | tile mosaic, ordered dither through a four-step ramp | diagonal sweep, centre burst, corner fade, wave, checker falloff | nes, fds, sms, sg1000, atari7800, coleco, intv, pce, sgx, sv; kind `cartridge` |
| parallax | banded sun behind stepped parallax ridges | sun position and size | snes, megadrive, s32x |
| bands | bold horizontal bands, blocky shapes, scanlines | band weights | atari2600, atari5200 |
| vector | twisting wireframe tunnel on black | sides and twist | vectrex |
| lcd | abstract lit shapes on a dot matrix, with ghosting and backlight | bars, blocky landscape, rings, geometric glyphs, dot lattice | gb, ngp, gbc, ws, pokemini, gg, gba, lynx, wsc |
| disc | concentric tracks, thin-film sheen, light sweep | centre and sheen angle | psx, saturn, megacd, pcecd, neocd; kind `disc` |
| poly | flat-shaded low-poly terrain | height field | n64 |
| marquee | perspective grid to a glowing horizon, starfield, bulb chase | vanishing point | arcade, neogeo; kinds `arcade`, `romset` |
| phosphor | glyph-like blocks on a phosphor raster | text layout | kind `computer` |
| contour | drifting contour lines | wave field | kind `other`, unknown ids |

Colours come from `web/src/lib/art/palette.ts`: the hues of the theme tokens
in `app.css` (accent, ok, warn) and a few that harmonise with them. Each
family has a base hue, and most mapped ids set their own hue so siblings read
apart: the colour handhelds get distinct hues, the monochrome ones muted
slate, sepia, grey-blue or teal tints. No hue is chosen to match a
manufacturer's branding, and no LCD uses a pea-green tint. A PRNG seeded by
the id shifts the hue slightly and drives every other choice, so the same id
always draws the same art. Every colour slot runs from a dark value to a
light one through `--l`, which the component sets from `prefers-color-scheme`.
There is no motion.

The hardware is the focal element: flat, filled with the palette's body
tone, outlined by a thin rim in the family's light tone, with slots, vents,
ports, screens and keys built from shared rectangles, circles and short
paths. One key colour serves every button. It stands centred on a soft floor
shadow in front of a glow, fitted to about half the banner height. Its forms:

| Hardware | Platforms |
|---|---|
| front-loading box with a lid flap and ribs | nes |
| drive with a disk in its slot | fds |
| angled wedge with a card slot | sms |
| stepped box with a rear slot | sg1000 |
| wedge with a front band and two buttons | atari7800 |
| keypad controllers docked on the body | coleco, intv |
| compact box with a front card slot | pce, sgx |
| rounded box with a raised slot and sliders | snes |
| body with a round central dome; with a stacked add-on | megadrive; s32x |
| raised centre slot above four ports | n64 |
| low base under a raised slot block with switches | atari2600 |
| long wedge with a wide slot | atari5200 |
| portrait cabinet whose screen shows a vector tunnel, with its controller | vectrex |
| portrait handheld; the classic has an angled grille | gb, gbc |
| landscape handheld with a centred screen | gba, gg, lynx |
| landscape handheld with a thumb stick | ngp |
| landscape handheld with a side screen and key diamonds | ws, wsc |
| small rounded handheld | pokemini, sv |
| top-down disc console with a round lid | psx, saturn, megacd, pcecd, neocd |
| home console with a joystick controller | neogeo |
| upright arcade cabinet | arcade; kinds `arcade`, `romset` |
| keyboard computer | kind `computer` |
| generic console box | every other id |

A compact focal element (the hardware, a sun, burst, lit shape or vanishing
point) is centred where every banner aspect in use shows it: between 32% and 68% of the
width in the card format, whose banner crops its sides on desktop, and
between 48% and 52% in the wide format, of which a phone shows only the
middle 358 of 960 units. The disc and tunnel are wider than a card banner and
sit off centre there.

On a Platforms card and in the Browse header the art fills a 150 px banner
that fades into the background, and the text starts below it where the scrim
is at least 85% opaque, so text keeps AA contrast over any art. The Browse
header uses a wider format. Disabled cards and cards without a core show the
art desaturated and dimmed. The art is `aria-hidden`; the platform name stays
the accessible name. Each tile stays under 120 SVG elements, its hardware
under 60, and art is memoised per id, kind and format.

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
