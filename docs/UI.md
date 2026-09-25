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
2. DATs: drop zone, the URL field and the watched-directory path, taking
   Logiqx DATs, No-Intro database exports and zipped packs. Lists the files still in
   `dats/`, each waiting (with the reason), importing (with progress) or
   rejected (with the reason), then this session's uploads with their
   outcome, then the loaded DATs and the platform each bound to.
3. Client: detection result. When no client answers but one is installed,
   Start Transmission and Start rtorrent buttons, with a note that starting
   Transmission through its init script makes it start at boot. Remote path
   map with a Remove button per row; blank rows are dropped and a row
   without a remote path or with a relative local path is refused before
   saving.
4. Sources: drop zone, the magnet box, the URL field and the
   watched-directory path, the files still in
   `sources/` listed as in step 2, and the added sources with their state and
   reason. Seed policy explained with its default shown, and, while the
   client pauses during games, that it does and where to turn that off. Nothing about where
   to obtain files.

The app opens the wizard on load only while `open_on_start` is true. Finish,
or leaving the wizard any other way, calls `POST /system/wizard/done`, so a
reload lands on Platforms afterwards; the "Setup not finished" card opens it
again.

**Held-jobs banner.** On every screen, and at the top of the wizard, while
`waiting` is not empty: how many jobs are held, why (the running core, which
holds scans and imports, or the user's Pause, which also holds DAT and source
imports), which ones, and a Run now button that calls `POST /system/resume`.

**Activity indicator.** On every screen but the wizard, at the end of the
nav: a pulse icon with the count of running, queued and waiting jobs, left
out of the count and the panel are the housekeeping kinds (client checks,
magnet lookups, transfer stops, source re-maps), which Activity still
lists. The icon takes the accent colour with a breathing dot while
something runs, the warning colour while work only waits, and the dimmed
text colour when idle; the button keeps its width either way, so nothing
moves. Its accessible name says the same in words ("Background work: 1
running, 2 waiting"), and a visually hidden `aria-live` line announces each
change. The button is a disclosure (`aria-expanded`, `aria-controls`)
opening a panel under it: Running, with each job's progress bar; Waiting,
with why each waits ("Waiting for the DAT import of a.dat to finish.",
"Paused while NES is running"); a URL fetch in either list has a Cancel
button named for it ("Cancel URL fetch: a.dat", "Cancelling URL fetch:
a.dat" while pending); and Finished, the last five from
`/system/jobs/recent` with their outcome and how long ago. Every row links to
the page that owns it (DATs, Sources, the platform, else Activity), and the
panel ends with Open Activity. Opening moves focus to the panel; Escape
closes it and returns focus to the button; a press outside, or focus
leaving it, closes it without moving focus. At 480 px and below the panel
spans the width less 8 px each side. The recent list is re-read only while
the panel is open.

**Platforms** (`/`). One card per platform with core present, counts, and a
scan button, below a banner of the platform's art. While the platform's scan
is open the card shows its status pill, with a progress bar of files done
while it runs or its reason while it waits. Scan shows a toast when
the scan is queued and another with its outcome when it finishes, such as
"Scan of Nintendo 64: 410 matched, 2 unmatched", followed by "3 not
identified" when there are any, on whichever page is open; counts reload when
a scan, a recompute or the CHD tracks job ends, and titles reload on
`file.changed`, which the CHD tracks job sends for each row it writes. A card with files not identified has "N not
identified" as a disclosure that lists them 50 at a time, each path with the
sentence for its reason ("Not identified: no loaded DAT entry has this number
and size of tracks.") and Show more, with an `aria-live` count of those
shown. Platforms whose core is absent
are in a collapsed section. While the wizard reports a missing DAT, client or
source, a "Setup not finished" card lists what is missing and links to the
wizard.

**Browse** (`/p/{id}`). A Start core button beside the platform name, except
on arcade, both below a banner of the platform's art. Poster grid of
`title_groups`, cover from the libretro URL. A title with no cover URL, or
whose cover fails to load, gets a generated poster
(`web/src/lib/PosterPlaceholder.svelte`): the platform's art as a faded band
over the theme's raised background, the title's name set in bold beneath it
and the name's parenthesised tags, such as the region, on a dimmed line at the
foot. A hash of the name picks the band's hue rotation (up to 30° either way),
horizontal shift and a zoom that covers it (`web/src/lib/poster.ts`), so neighbours differ
and a title always looks the same. It uses the theme tokens, makes no
request, and is `aria-hidden`, since the name is already on the card as text.
Each card is a column whose status line and Want button sit at its foot, so
they line up across a row whatever the name's length.
Filters: search, have / missing / wanted, region, a "Show hidden" checkbox,
and a flags multi-select that requires every checked flag. Each card shows
the 1G1R pick name, a have indicator, and a want toggle. Infinite scroll in
pages of 60. Search runs 250 ms after typing stops, and each request cancels
the one before it. A background reload, such as one a scan's `file.changed`
asks for, never cancels a page on its way: it runs once that page lands, so
a stream of events never keeps a slow search from answering. It rewrites
each loaded page in its place and drops any row that would then show twice.
Scrolling on loads the rows after those shown, from the row before them;
when that row is not the last one shown, the list moved and a background
reload rewrites the loaded pages, so no group shows twice or is skipped.
While a page is loading a thin progress bar shows and the current results
stay visible, dimmed and marked `aria-busy`; a failed load shows an alert
with Retry.

**Title** (`/t/{id}`). Every variant in the group with region, revision,
flags, file state (a CHD member named as "g.chd, track 2" or "g.chd, track
list"), and which sources have it: one line per file, as "nova.nes
in Example Pack (name guess)", with the confidence read as "name match",
"hash match", "name match", "name and size", "name guess" or "size only", or
"None available". Want per variant, except on a variant flagged `bios`, which the server
refuses (PRINCIPLES.md section 3). Play for a
variant whose files are all in the collection. Rename action for misnamed
files. Art tabs: boxart, title, snap; a missing boxart shows the generated
poster, a missing title or snap image nothing.

Play and Start core are disabled while launching is unavailable, with the
reason as a line of text on the page rather than a tooltip: launching is
turned off in settings, mistarr is not running on a MiSTer, or no core for
the platform is installed.

**Activity** (`/activity`). Downloads with their status pill and per-file
progress bars, imports log, queued and running jobs, each with its status
pill, what it is about linked to the page that owns it, its lane, its
progress bar while running and its reason while it waits, and a Recent list
of the last finished jobs from `/system/jobs/recent`, one line each with
its pill, outcome and time. A running `chd_tracks` job's bar follows the
image being decoded ("45% · g.chd · image 2 of 5") and its outcome reads "CHD
tracks: 3 verified, 1 unmatched, 1 not identified". Live over SSE.

**Sources** (`/sources`). Table of sources: name, platform, state as a
status pill (resolving runs, unbound waits, bound is done, disabled is
paused) with its reason beneath, file count,
matched count, seed policy, client status. Bind and disable actions.
Under the heading, the client-held pill while it applies, and under each
seed policy the note "Paused while a core runs" while the setting is on (see
"The client while a core runs").
Unbound sources have a platform picker and, when the names suggest one, a
"Bind to" button for the suggested platform. The name links to the source's
detail, and a "Set by you" tag beside the platform marks a binding the user
chose. Above the table, the upload control, the magnet box and the URL
field, then the files still in `sources/` and this session's uploads, as in
the wizard.

**URL field.** On DATs, Sources and the wizard's DAT and source steps: a
text box labelled "Add from a URL" with the placeholder
`https://example.invalid/…`, `autocomplete="off"` on it and its form,
`inputmode="url"`, no spell check or capitalisation, no `datalist`, and a
Fetch button; a note under it says it takes a DAT, a zipped DAT pack, a
.torrent file or a magnet link, fetched once and not remembered. The box
empties once the server takes the link and keeps it when the server refuses
it, so it can be corrected; the link is never written to storage, and a
reload shows the box empty (PRINCIPLES.md section 2). Whatever screen it is
on, the file goes where its content says. A magnet is received like the
magnet box's. An http(s) link says "Fetching the file. Its progress is under
Background work." and appears there and on Activity as "URL fetch: sent at
14:02:31", the time it was sent, with "(1)", "(2)" by job when several were
sent in the same second, and never any part of the URL; then "URL fetch:
<file>" once the first bytes name it, with a bar of the bytes received
("Receiving · 40% · 1.0 MiB of 2.3 MiB", or a moving band and "1.0 MiB
received" without a length), then "Checking the file" and "Writing to the
card", and a Cancel button, which says "Cancelling…" and is disabled until
the fetch ends. When it lands the toast is an upload's ("DAT received:
a.dat. Importing now.") and the file is followed as an upload; when it
fails the toast is "The fetch failed: <reason>", or "The fetch was
cancelled.".

A fetched DAT is not placed as the server sent it: the file that lands in
`dats/` is mistarr's rewrite of the DAT's entries, the header fields and the
games, releases and roms it reads, and for a DB export its archives and
files, with nothing else the server's file held. A fetched pack is rebuilt
from the rewrites of its members. An uploaded or dropped file is placed as
it is, since the user chose it.

A fetched zip must hold only `.dat` and `.xml` DATs ("This zip holds files
other than DATs."), where an upload loads the DATs of a mixed zip and skips
the rest. An uploaded zip is a file the user already has and chose; a
fetched one came from a server, and mistarr places only what it rewrote, so
a member it could not rewrite has no place to go.

**Source** (`/sources/{id}`). A link back to Sources, then the name; size,
file count, the infohash shortened with Copy (over plain http, where the
browser has no clipboard, Copy shows the whole infohash to copy by hand),
added date, dropped file, and whether the client has it, with a paused pill
("Client paused while NES is running", or "Client uploads paused while…")
while a `client_hold` applies, and a bar of the
selected files' transfer. The seed policy, the same control as on the list, saved through
the same `PUT /sources/{id}`, so a held or missing client defers it the same
way. Under
Classification: the state pill, a "Set by you" tag for a user's binding, one
sentence on how it was bound ("Bound to NES automatically. 94% of its files
match DAT entries.", "Bound to NES by you.", "Marked by you as not a game
set."), the DATs its matches come from, and counts of matched, possible,
unmatched, extra and wanted files. Re-classify is a disclosure that asks
`/sources/{id}/preview` once per page view, aborting it if the panel
closes first, and lists each platform with a DAT as a radio, "would match N
of M files" ("about N" when sampled), plus "Not a game set"; choosing one
says what applying does, and Apply queues the binding job, whose bar shows on
the page and in the activity panel until a toast gives its outcome. While the
choice waits, the page and the list say "Binding to NES…", "Marking as not a
game set…" or "Returning to automatic binding…". Reset to automatic, shown
only while the binding is the user's, hands the source back to the
classifier. Closing the panel, or Reset disappearing, returns focus to
Re-classify. The page is rebuilt for each source id, so nothing carries over
from one source to the next. The file table has filter chips (All, Matched, Unmatched, Wanted,
`aria-pressed`), a path search 250 ms after typing stops, and pages of 50
with Previous and Next; each row shows the path, size, kind, the matched
entry linked to its title with its confidence, a candidate as "Possibly …",
or the reason nothing matched, and the file's transfer state. At 600 px and
below each row stacks its cells.

**DATs** (`/dats`). Its own nav entry, between Sources and System, since
DATs arrive and fail on their own schedule like sources do. An upload
control taking several `.dat`, `.xml` or `.zip` files, the same upload as
the wizard's, and the URL field. The files still in `dats/` as in the wizard: waiting with the
reason, importing with a progress bar of the DAT read so far, its phase and
games read, rejected with the reason on its own line
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

**System** (`/system`). A grid of status tiles, three across and two
at phone width: the MiSTer (the running core, "At the menu" for `MENU`, the
launch state, and the client-held pill while the client is held), the
download client (kind, version, a Reachable or Not reachable pill, its
address, the client-held pill with the rtorrent 1 KiB/s line, and the same
start offer as the wizard), the
scheduler (a Running, Paused or Held for the core pill, the jobs waiting,
and Pause, Resume or Run now), memory (mistarr's RSS, with a meter of the
board's memory in use and `MemAvailable` of `MemTotal`), storage (free
space, with a meter of the data directory's filesystem in use) and uptime.
Meters warn past 85 % for memory and 90 % for storage. Live over the SSE
`status` event.

An About block shows the version with a Copy button, a Release or
Development build pill, the commit, a link to the release's notes on the
project's GitHub page for a release build only, and Copy diagnostics. That
copies a plain-text summary in the shape of `mistarr doctor`'s, built from
`/system/status` by `web/src/lib/system.ts`: version, commit, uptime, client
kind, version and reachability, which client programs exist, CORENAME,
scheduler state and the number of jobs waiting, whether the client is paused
while a core runs and how it is held now, launch state, memory, free
space, CHD decoding speed and the browser. It never holds a path, an
address, a file name or anything naming content. Over plain HTTP, where the
Clipboard API is missing, it copies through a selected text area, and shows
the text to copy by hand when the browser refuses.

Settings, for the runtime-editable subset, are in titled sections: Download
client (kind, address, and the shared path map editor as "Path map"),
Transfers and limits (download and upload at the menu and while a core
runs, in kB/s, with the line "0 keeps the client's own limit. Any other
value only ever lowers it."; an empty or non-whole value is marked invalid,
changes nothing and blocks Save with a message; then the checkbox "Pause the
download client while a core runs", on by default, with the line "Frees the
board for the game. Transfers resume at the menu, and each source's seed
policy applies again. A client on another machine only stops uploading."),
Title choice (1G1R region and language order,
hidden flags, prefer the highest revision), Launching (the switch that
allows launching) and Scanning. Each field has its label, control and help
text in the same two columns on wide screens and stacked on a phone. A
section list beside them, sticky on wide screens and a row of buttons on a
phone, moves focus to a section's heading and marks the section in view.
A note says these apply on save with no restart, and that settings found
only in `mistarr.toml` apply at the next start. A save bar with Save and Discard
appears, pinned to the bottom of the window (sticky where the browser
supports `overflow-x: clip`, fixed elsewhere), only while something differs
from what was loaded or saved, and lifts the toasts above itself. While it
shows, leaving for another screen by a link, Back, Forward or an edited
address asks first and stays on a no, through a guard in
`web/src/lib/router.svelte.ts`; a link opened in a new tab asks nothing, and
a reload is warned by the browser. Under Scanning, "Disc
images": the checkbox "Identify CHD images by their tracks" with a "Slow"
tag, a line saying it decodes each image once, pauses while a core runs and
keeps its results, and the measured speed as "about N minutes per 700 MB
image", or "Speed not measured yet."

## The client while a core runs

While `/system/status` reports a `client_hold`, the Sources screen, the
activity panel under its heading, and System's MiSTer and download client
tiles show a paused pill (`web/src/lib/ClientHeld.svelte`, without its
rtorrent line in the MiSTer tile): "Download client paused while
NES is running" for a stopped client, or "Uploads paused while NES is
running" for held uploads, naming the core as the held-jobs banner does. For
rtorrent, whose lowest held rate is 1 KiB/s, a line beside it and its title
say so. It follows the `status` event, so it appears when the client is held
and goes when it is let go at the menu. While
`pause_client_while_playing` is on, each source row, and the seed policy on
a source's detail, notes "Paused while a core runs", and the wizard's seed policy step says that
transfers pause while a core runs, that a client on another machine only
stops uploading, and that this can be turned off in System.

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
slate, sepia, grey-blue or teal tints. Handhelds sold in a range of bold
shell colours (gba, gbc, pokemini) take muted sage, brick and slate tones,
so no body reads as one of those shells. No hue is chosen to match a
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

## Status vocabulary

Every job, incoming file, source and download shows its state through one
component, `web/src/lib/StatusPill.svelte`, in one of six states, each with
its own shape as well as its colour from the theme tokens, so colour is never
the only cue:

| State | Shape | Colour | Means |
|---|---|---|---|
| queued | hollow ring | dimmed text | in line; runs when its turn comes |
| running | turning arc | accent | working now |
| waiting | clock | warning | held up by something named in its reason |
| paused | two bars | dimmed text | held by a running core or the user, or stopped |
| done | tick | ok | finished |
| failed | cross | danger | finished with an error, or rejected |

The pill's word is the state's, or the thing's own name for it: an
importing file is running as "Importing", a rejected one failed as
"Rejected", an unbound source waits as "Unbound", a cancelled download is
paused as "Cancelled". A queued job held by the gate shows as paused, and
one whose reason says what it waits for shows as waiting.
`web/src/lib/status.ts` maps each state to its pill; nothing else picks
colours for a state.

Progress is one component, `ProgressBar.svelte`, a `progressbar` with its
share done and a line of text such as "Reading games · 35% · 4,432 games".
When the share is unknown (storing, choosing preferred versions, refreshing
title groups, matching files on the card) it shows a moving band with the
phase name instead of a stuck percentage. The arc, the band and the dot
stand still under `prefers-reduced-motion`, the band as a static stripe.

## Feedback

Every button that starts work shows that it is sending until the server
answers: Scan reads "Queuing…", Add reads "Adding…", Fetch reads
"Sending…", and a file picker
shows an "Uploading <name>…" status line beneath it. Each is
`aria-disabled` and `aria-busy` and ignores further presses, but is never
`disabled`, so keyboard focus stays on it. Then a toast says what happened, in one short
sentence:

- received: "Torrent received: <file>. Waiting for the DAT import of <dat>
  to finish.", "Magnet received: …", "DAT received: …", ending with what
  the file waits for, or "Importing now.", also when a URL fetch lands;
- queued: "Fetching the file. Its progress is under Background work." for a
  URL fetch;
- finished: "<file> added as a source." once a source upload's import ends,
  "<file> loaded." on `dat.loaded` for an uploaded DAT, on whichever page is
  open, and a scan's outcome as above;
- failed: the file name and the server's message when the upload is
  refused, or "<file> was rejected: <reason>" when its import rejects it;
  "The fetch failed: <reason>" or "The fetch was cancelled." for a URL
  fetch, and the server's message alone when it refuses the link.

Toasts stack at the bottom right, full width on a phone, at most four. Each
has a close button; information and success leave after 6 s and errors after
10 s. Screen readers hear each once through two hidden live regions that
exist before any toast, a polite one for information and success and an
assertive one for errors; the toasts themselves are not live regions.

## State handling

One store per API resource, hydrated on navigation and patched by SSE events.
The jobs store is loaded on start by the activity indicator and patched by
`job.progress`; live progress of a job it knows to be running patches the
job in place and triggers no re-read.
The incoming-file lists re-read `/dats/incoming` or `/sources/incoming` at
most once per burst of `job.progress` (other than such live progress),
`dat.loaded`, `dat.rejected` or `source.changed` events, and `/sources` is re-read at most once per burst of
`source.changed`. The outcomes of this session's uploads come from the last
50 finished jobs; a resync forgets them, and an upload whose job is no longer
known then says to look at the lists. An upload answered before its job was
recorded is followed by file name until the queued `job.progress` whose
`detail` names it. `/system/jobs/recent` is re-read once
per burst of finished jobs while Activity is open, and on a resync while
Activity is open or a scan the user queued awaits its outcome, which the
re-read list then supplies.
No polling from the browser. The SSE connection shows a banner when
disconnected and reconnects with backoff.

## Language

Follow PRINCIPLES.md section 5. Buttons say "Want", "Scan", "Bind",
"Rename", "Play", "Start core". The empty state on Sources says: "No sources yet. Place a .torrent
or .magnet file in `<path>` or drop one here." and nothing more.
