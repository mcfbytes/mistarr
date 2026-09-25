# DATs

A DAT is the catalogue of one platform: every entry it lists, with the name,
size and hashes of each file. mistarr verifies and organises files against the
DATs you give it and does nothing for a platform without one. This guide says
which DATs it reads, how to hand them over, and what it does with them. It
does not say where to obtain them (PRINCIPLES.md section 2).

The formal rules live in [VERIFICATION.md](VERIFICATION.md) "DAT parsing" and
"DAT families", [ARCHITECTURE.md](ARCHITECTURE.md) "DAT import" and
[PLATFORMS.md](PLATFORMS.md); this page is the user's view of them.

## Formats

| Form | Recognised by | Notes |
|---|---|---|
| Logiqx XML | a `.dat` or `.xml` file whose first element is `<datafile>` | The common XML DAT format: a `<header>` and one `<game>` per entry, each with its `<rom>` files. |
| No-Intro database export | a `.dat` or `.xml` file that starts with `<header>` followed by `<datafile>` | Loads into the same entries as a Logiqx DAT of the same list. Its name and version come from the file name (below). |
| Zipped pack | a `.zip` | Every `.dat` and `.xml` member is loaded as a DAT of its own, in either form above. Other members are ignored. |

Files are read as UTF-8, with or without a byte order mark; UTF-16 and
text-format (non-XML) DATs are not read. A DAT may be large: it is streamed,
never held whole in memory.

A minimal Logiqx DAT, with synthetic content:

```xml
<?xml version="1.0"?>
<datafile>
  <header>
    <name>Example Vendor - Example System</name>
    <version>20260101-000000</version>
  </header>
  <game name="Example Quest (World)">
    <description>Example Quest (World)</description>
    <rom name="Example Quest (World).bin" size="4" crc="0badc0de"/>
  </game>
</datafile>
```

A database export has no system name inside it, so its name comes from the
file (or zip member) name: `Example System (DB Export) (20260101-000000).xml`
loads as DAT `Example System (DB Export)`, version `20260101-000000`. Keep
that naming when you rename one. A file named without the `(DB Export)` marker
takes its name from the file name less a final version group, which becomes
its version.

## Adding DATs

Either way below ends in the same import.

- **Drop the file** into `dats/` under the data directory,
  `/media/fat/mistarr/dats/` by default. mistarr looks every 10 seconds and
  imports a file once it has stopped changing, so a copy in progress is never
  read half-written.
- **Upload it** on the DATs screen, or in the DATs step of the first-run
  wizard. Both take several `.dat`, `.xml` or `.zip` files at once, up to
  512 MiB each, and write them into `dats/`.

The DATs screen lists the files still in `dats/`: waiting (with the reason,
such as "Waiting for the file to stop changing."), importing (with progress)
or rejected (with the reason). Imports run while a core is loaded; only a
manual pause on the System screen holds them.

A loaded file moves to `dats/loaded/`, gaining ` (1)`, or the next free
number, before its extension when that name is taken. A rejected file moves to `dats/rejected/` with
`<file>.reason.txt` beside it. When some members of a zip load and others do
not, the zip moves to `dats/loaded/` and each failed member is reported as a
rejection on the screen.

## Which platform a DAT is for

mistarr binds a DAT to a platform by the `<name>` in its header, using the
"DAT name matches" column of [PLATFORMS.md](PLATFORMS.md). The name is
lowercased, bracketed groups such as `(Headered)` are dropped and runs of
punctuation read as one space, so `Example - Nintendo Entertainment System
(Headered)` binds to `nes`. When several patterns match, the longest wins:
`Game Boy Color` binds to `gbc`, not `gb`. A header without a name takes the
file's name, less its extension.

When no pattern matches, a DAT falls back to the platform an earlier version
of its family (below) is bound to. Failing that it is stored **Not bound**:
the DATs screen lists it, with the platforms its family is loaded for when
there are any, and its entries are not loaded yet. Binding it with
`POST /api/v1/platforms/{id}/dat` and body `{ "dat_version_id": N }`
([API.md](API.md) "Platforms") reads the file again from `dats/loaded/` and
loads its entries for that platform.

## Versions and families

A platform can have several DATs live at once, such as a main list and a
separate homebrew list. A new DAT replaces only an earlier version of the
**same list**, its family. The family is the header name with its format
markers and trailing version or date groups removed and case ignored, so:

| Header name | Family |
|---|---|
| `Example Vendor - Example System (Headered)` | `example vendor - example system` |
| `Example Vendor - Example System (DB Export)` | `example vendor - example system` |
| `Example Vendor - Example System (20260101)` | `example vendor - example system` |
| `Example Homebrew - Example System` | `example homebrew - example system` |

The first three are one family whichever form each arrived in; the fourth
stays live beside them. The format markers are `DB Export`, `Headered`,
`Headerless`, `Parent-Clone`, `Retool`, `BigEndian`, `ByteSwapped` and
`LittleEndian`.

Within a family on one platform, versions are compared by the numbers in
them, so `20260101-000000` and `20260102` compare as dates and `1.9` comes
before `1.10`. When a version has no digits, the one loaded later is newer.

- Loading a version that is not older than the current one replaces it. The
  replaced version shows "Replaced by version X" under "Older versions", or
  "Replaced by NAME version X" when the new one has a different DAT name
  in the same family.
  Entries the new version no longer lists are retired from the catalogue;
  files on the card are never touched.
- Loading an older version stores it without loading its entries, shown as
  "Older than version X, which stays current" (with the current DAT's name
  before "version" when it differs).
- Remove on a current version takes its entries out of the catalogue after a
  confirmation, shown as "Removed; its games are no longer listed". It does
  not bring back an older version; drop that file again to load it.

When two families on a platform list the same entry with the same files,
browse shows it once.

Every load, replacement or removal then matches the platform's files again
from their stored hashes, without reading them, so files already on the
card are recognised against the new list without a rescan. Arcade is the
exception; see below.

## Rejections

A rejected file stays in `dats/rejected/` until you act on it. On the DATs
screen, **Retry** moves it back into `dats/` so a file you fixed in place
loads again, and **Delete** removes it and its reason after a confirmation.
A reason that starts with a member name, as in `pack/list.dat: DAT contains
no games`, is about that member of a zip.

| Reason | Meaning and what to do |
|---|---|
| `not a DAT: expected a .dat, .xml or .zip file` | The extension is not one mistarr reads. Rename it if it is XML. |
| `root element is <…>; expected a Logiqx DAT (<datafile>) or a No-Intro DB export (<header> followed by <datafile>)` | The file is XML but neither form, such as some other XML document. |
| `invalid XML at byte N: …` | The XML is malformed at that byte offset, or a name or value there is not UTF-8. Comments and ignored elements may hold any bytes. |
| `file ends before </datafile>` | The file is cut short, usually by an interrupted copy or download. Copy it again. |
| `DAT contains no games` | The file parses but lists no entries. |
| `<rom> in game "…" has no name attribute` | A required attribute is missing on that entry. The element may also be `<file>`, or `<game>` with an empty game name when a game has no name. |
| `game "…" has invalid crc value "…"` | A size, hash or status is not in the expected form; a hash must have its full length in hex. |
| `invalid zip archive: …` | The zip cannot be read. |
| `zip archive contains no .dat or .xml files` | The zip holds nothing to load. |

A DAT whose entries are all fine but whose name binds to no platform is not
rejected: it is stored Not bound, as above.

## Headered and headerless DATs

Some cores need a header on the file that the DAT's hashes may or may not
cover. mistarr hashes files of these platforms by a header rule so a file on
the card matches whichever form the DAT lists:

| Rule | Platforms | What it does |
|---|---|---|
| `ines` | `nes` | A file starting with the iNES signature is also hashed from byte 16, to match a headerless DAT. |
| `a78` | `atari7800` | The 128-byte header is skipped when matching a headerless DAT. |
| `lnx` | `lynx` | The 64-byte header is skipped when present. |
| `smc` | `snes` | A 512-byte copier header is skipped when hashing, and stripped on placement. |
| `n64` | `n64` | Byte order is detected and hashed as big-endian; placement writes big-endian. |

For these platforms, prefer the headered form of a Logiqx DAT where
[PLATFORMS.md](PLATFORMS.md) says so, as for `nes`, `atari7800` and `lynx`.
From a database export of a platform whose rule strips a header, mistarr
uses the headerless entries, names each file `<game>.<ext>` with the
platform's extension, and on placement writes the header the export records
back onto the file, so the core gets the headered file it needs. Apart from
the SNES copier header, mistarr never removes a header from a file.

## Arcade and MRAs

Arcade on MiSTer runs from MRA files, and so does mistarr's arcade catalogue.
MRAs are not DATs: mistarr reads them from `_Arcade` on the card, the same
place MiSTer's menu does, at startup, when the Arcade platform or every
platform is scanned, and when cores are detected again. Each MRA
becomes one arcade entry; the zips it names are its files, checked against
the MRA's own `md5` where it carries one. Folders of links to the same MRAs,
such as `_Organized`, are skipped.

A DAT whose header name binds to `arcade` is optional. It adds a check only
when mistarr places a zip whose MRA has no `md5`: the zip's members are then
verified against the DAT entry of the same set name. A DAT alone, with no
MRAs, is not a supported arcade setup. Details are in
[PLATFORMS.md](PLATFORMS.md) "MRA catalogue" and "MRA import".

## BIOS entries

Entries a DAT marks as BIOS are hidden by default and can never be wanted.
For a core that needs a BIOS, mistarr reports "BIOS missing" and stops there;
it does not classify, fetch, verify or place BIOS files (PRINCIPLES.md
section 3).
