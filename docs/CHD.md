# CHD images

mistarr reads CHD v5 CD images far enough to rebuild each track's `.bin`
exactly as a Redump-style DAT lists it, and to hash those bytes. This page
describes the format as `mistarr_core::chd` reads it and as the test writer
in `mistarr-fixture` writes it. It was written from a description of MAME's
format and checked against chdman's output in tests; no third-party code is
copied. How the scan uses it is in [VERIFICATION.md](VERIFICATION.md) "CHD
images".

All integers are big-endian unless stated.

## Header

A v5 header is 124 bytes.

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `MComprHD` |
| 8 | 4 | header length, 124 |
| 12 | 4 | version, 5 |
| 16 | 4 × 4 | codec slots 0 to 3 as FourCCs; 0 is empty |
| 32 | 8 | logical bytes: the decoded size |
| 40 | 8 | map offset |
| 48 | 8 | first metadata entry, 0 for none |
| 56 | 4 | hunk bytes |
| 60 | 4 | unit bytes, 2448 for a CD |
| 64 | 20 | raw SHA1: SHA1 of the logical bytes |
| 84 | 20 | SHA1: the combined SHA1 below |
| 104 | 20 | parent SHA1, all zero without a parent |

The combined SHA1 is SHA1 over the raw SHA1 followed by one 24-byte record
per metadata entry whose flags have bit 0 set: the entry's tag and the SHA1
of its payload, the records sorted bytewise. Two images with the same
combined SHA1 hold the same data and the same track list, so the combined
SHA1 and the file size are the image's identity (`ChdId`).

`read_header` reads 16 bytes, then the other 108 only when the version is 5
and the length is 124. It never reads past byte 124.

### Header checks

| Check | Reason |
|---|---|
| magic is not `MComprHD` | `not_chd` |
| version is not 5 | `version` |
| length is not 124 | `corrupt` |
| parent SHA1 is set | `parent` |
| unit bytes is not 2448 | `not_cd` |
| hunk bytes is 0, not a multiple of 2448, or over 214 frames (523,872 bytes) | `corrupt` |
| logical bytes is 0 or not a multiple of 2448 | `corrupt` |
| more than 450,000 frames | `too_large` |
| an empty codec slot before a used one | `corrupt` |

Hunks number `ceil(logical / hunk bytes)`; the last may be partial, and only
its bytes below the logical size count.

## Metadata

Entries form a chain from the header's metadata offset. Each has a 16-byte
header: tag (4), flags (1, bit 0 = in the combined SHA1), payload length
(3), next entry offset (8, 0 ends the chain). The walk stops with `corrupt`
at more than 128 entries, a repeated offset or an entry past the end of the
file. A CD track payload over 4096 bytes, or more than 64 KiB of other
payloads, is `corrupt`.

| Tag | Meaning | Result |
|---|---|---|
| `CHT2` | one CD track, current format | parsed |
| `CHGD`, `CHGT` | GD-ROM track | `gdrom` |
| `CHTR`, `CHCD` without `CHT2` | older track formats with no pregap | `old_layout` |
| `CHSE` | session marker | ignored |
| `GDDD`, `DVD `, `AVAV`, or no track tag | not a CD | `not_cd` |

A `CHT2` payload is text ending in one NUL:
`TRACK:n TYPE:t SUBTYPE:s FRAMES:f PREGAP:p PGTYPE:g PGSUB:s POSTGAP:q`.
The n-th `CHT2` entry is track n and its `TRACK` must say n.

## Tracks

| `TYPE` | Stored as | Rebuilt |
|---|---|---|
| `MODE1_RAW`, `MODE1/2352` | 2352-byte sectors | as stored |
| `MODE2_RAW`, `MODE2/2352`, `CDI/2352` | 2352-byte sectors | as stored |
| `AUDIO`, `CDG` | 2352-byte samples, big-endian | each byte pair swapped to little-endian |
| `MODE1`, `MODE1/2048`, `MODE2_FORM1`, `MODE2/2048`, `MODE2_FORM2`, `MODE2/2324`, `MODE2`, `MODE2_FORM_MIX`, `MODE2/2336` | cooked sectors | not rebuilt: `cooked` |
| anything else | | `corrupt` |

Every CHD frame is 2448 bytes: 2352 bytes of sector and 96 of subcode. A
track's `.bin` is the first 2352 bytes of each of its frames. Each track is
followed by zero frames up to a multiple of 4 (`pad4(n) = (4 - n % 4) % 4`),
which belong to no track. Track t starts at frame
`Σ_{i<t} (frames_i + pad4(frames_i))`, and the tracks must cover the logical
size exactly or the image is `corrupt`. There are 1 to 99 tracks, each with
at least one frame.

`FRAMES` counts a stored pregap. A `PGTYPE` starting with `V` means the
pregap is stored; without the `V` and with `PREGAP` above 0 the pregap is
virtual: its sectors are not in the image. A virtual pregap on track 1 is
harmless, since a Redump track 1 never holds its lead-in; on a later track it
is `pregap`. `SUBTYPE`, `PGSUB` and `POSTGAP` do not change the bytes.

### How chdman fills CHT2 from a cue

chdman's cue reader, for each track:

- with `INDEX 00`: `PREGAP` is INDEX 01 less INDEX 00, `PGTYPE` is `V` plus
  the track type, and the track runs from its INDEX 00 to the next track's
  INDEX 00 (or the end of its file). That is Redump's split, so a split-bin
  and a single-bin cue give the same tracks;
- with a `PREGAP` command: a virtual pregap, not stored;
- with no `INDEX 00`: no pregap;
- `AUDIO`: samples are byte-swapped to big-endian as they are stored.

Worked example, the two entries

```
TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:1001 PREGAP:0 PGTYPE:MODE1 PGSUB:RW POSTGAP:0
TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:1350 PREGAP:150 PGTYPE:VAUDIO PGSUB:RW POSTGAP:0
```

with logical bytes `(1004 + 1352) × 2448`: track 1 starts at frame 0 and is
1001 frames, 2,354,352 bytes; three pad frames follow. Track 2 starts at
frame 1004 and is 1350 frames, 3,175,200 bytes, its 150 stored pregap frames
first. Two pad frames end the image.

## Map

### Uncompressed images

When codec slot 0 is empty the map at the map offset is one u32 per hunk.
The hunk is stored at file offset `entry × hunk bytes`, `hunk bytes` long,
with no CRC. Entry 0 is a hunk of zeros (a parent hunk when there is a
parent). The raw SHA1 covers these hunks.

### Compressed map header

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | bytes of compressed map that follow |
| 4 | 6 | file offset of the first stored hunk |
| 10 | 2 | CRC16 of the expanded map |
| 12 | 1 | length bits |
| 13 | 1 | self bits |
| 14 | 1 | parent bits |
| 15 | 1 | reserved |

A compressed map longer than `8 × hunks + 64` bytes, or a field wider than
32 bits, is `corrupt`. The bit stream is read most significant bit first;
reading past its end yields zero bits and makes the map `corrupt`.

### Map tree

The stream starts with a Huffman tree over 16 symbols with codes of at most
8 bits, as code lengths in 4-bit fields: a value `v` other than 1 is the next
length; `1, 1` is a length of 1; `1, w, r` is `r + 3` lengths of `w`. The
lengths must fill exactly 16 symbols.

Codes are assigned in MAME's canonical order, which differs from DEFLATE's:
walk lengths from 32 down to 1 with `start = 0`; for each length L,
`next = (start + count[L]) / 2`, which must satisfy `next × 2 = start +
count[L]` for every L but 1; the first code of length L is `start`, then
`start = next`. Symbols then take codes in symbol order within each length.
Longer codes get numerically smaller values: lengths `1, 2, 3, 3` give
`1`, `01`, `000` and `001`, where DEFLATE would give `0`, `10`, `110`, `111`.
A zero length has no code. The lengths `1, 4, 13` escape give every symbol a
4-bit code equal to itself.

### Hunk types

One type per hunk, decoded with the tree, `last = 0` and `repeat = 0`:

- while `repeat > 0`, the hunk takes `last` and `repeat` drops by one;
- 7 (`RLE_SMALL`): this hunk takes `last` and `repeat = 2 + next symbol`, a
  run of 3 to 18 hunks in all;
- 8 (`RLE_LARGE`): this hunk takes `last` and
  `repeat = 18 + (symbol << 4) + symbol`, a run of 19 to 274;
- anything else is the type, and becomes `last`.

### Hunk fields

Then, per hunk, with `offset = first stored hunk` and `last_self = 0`:

| Type | Bits read | Entry |
|---|---|---|
| 0 to 3 (codec slot) | length (length bits), CRC16 (16) | at `offset`, which then grows by the length |
| 4 (`NONE`) | CRC16 (16) only | `hunk bytes` at `offset`, which then grows by them |
| 5 (`SELF`) | target (self bits) | a copy of that hunk; `last_self = target` |
| 9 (`SELF_0`) | none | a copy of `last_self` |
| 10 (`SELF_1`) | none | `last_self += 1`, a copy of it |
| 6, 11, 12, 13 | | a parent hunk: `parent` |

Stored hunks are therefore contiguous in hunk order. The map CRC16 covers a
12-byte expanded entry per hunk: type (0 to 6, the `SELF_0` and `SELF_1`
forms as 5 and the parent forms as 6), length (3), offset (6; the target for
a copy) and CRC16 (2; 0 for a copy). A stored hunk past the end of the file
or longer than a hunk, a copy of itself or a later hunk, or an unknown type
is `corrupt`; a slot that is empty or names a codec mistarr does not read is
`codec`.

CRC16 is CRC-16/IBM-3740: polynomial 0x1021, initial value 0xFFFF, not
reflected; `123456789` gives 0x29B1.

## Codecs

| FourCC | Sector data | Subcode |
|---|---|---|
| `zlib` | raw deflate (no zlib header) of the whole hunk | |
| `lzma` | raw LZMA of the whole hunk | |
| `zstd` | a zstd frame of the whole hunk | |
| `flac` | `L` or `B`, then FLAC frames of the hunk as 16-bit stereo in that byte order | |
| `cdzl` | raw deflate | raw deflate |
| `cdlz` | raw LZMA | raw deflate |
| `cdzs` | zstd | zstd |
| `cdfl` | FLAC frames, samples big-endian | raw deflate |

`huff`, `avhu` and other codecs are `codec`. Each decoded hunk must match its
map CRC16, or the image fails with `checksum`.

- Deflate output must be exactly the expected length.
- LZMA streams have no header and no end marker: properties lc 3, lp 0, pb 2,
  with the unpacked size known. mistarr sets the dictionary to the output
  length, which is at least what chdman used.
- zstd frames are checked before decoding: a window over 8 MiB, a dictionary,
  or a content size other than the expected length is `corrupt`. chdman
  writes single-segment frames with a content size; other encoders write
  windowed frames without one, and both are read.
- FLAC frames are bare, with no stream header: 44.1 kHz, 2 channels, 16-bit.
  `cdfl` uses blocks of `bytes / 4` samples halved until at most 2352, `flac`
  until at most 2048; the subcode of a `cdfl` hunk starts exactly where the
  last FLAC frame ends.

### The CD front end

`cdzl`, `cdlz` and `cdzs` hunks hold, for `frames = hunk bytes / 2448`:

```
[ECC flags: (frames + 7) / 8 bytes, frame i is bit i % 8 of byte i / 8]
[base length: 2 bytes, or 3 when hunk bytes ≥ 65536]
[base: frames × 2352 sector bytes, compressed]
[subcode: frames × 96 bytes, compressed, to the end of the hunk]
```

The frames are rebuilt as sector then subcode. For a flagged frame the
encoder had found the sync pattern and valid P and Q parity, and zeroed the
12 sync bytes and the parity (0x81C to 0x92F); the decoder restores the sync
`00 FF×10 00` and regenerates the parity. The EDC, header and subheader were
kept. `cdfl` hunks have no flags or base length.

## ECC

P and Q parity follow ECMA-130 over GF(2^8) with polynomial 0x11D, computed
over the sector from byte 12 (the header) on; in Mode 2 (sector byte 15 is 2)
the four header bytes count as zero, as they do in MAME and in chdman's
check. P is 86 columns of 24 bytes at offsets `b + 86c`, written to `0x81C +
b` and `0x81C + 86 + b`. Q is 52 diagonals of 43 bytes at offsets
`2 × ((43 × (b / 2) + 44c) mod 1118) + b mod 2`, written to `0x8C8 + b` and
`0x8C8 + 52 + b`, and covers the P bytes. For each row, with `low[x] = 2x` in
the field and `high` the inverse of `x ↦ x ⊕ low[x]`:

```
v1 = v2 = 0
for each source byte s: v1 ^= s; v2 ^= s; v1 = low[v1]
v1 = high[low[v1] ^ v2]; v2 ^= v1
```

writes `v1` then `v2`. The tables are computed at compile time.

## Decoding

`Decoder` reads the map, then decodes hunks in order in `step(n)` calls that
return at hunk boundaries. Each decoded hunk, clipped to the logical size,
feeds a running raw SHA1; each frame inside a track feeds that track's CRC32,
MD5 and SHA1 (audio byte-swapped), and pad frames are skipped. One track's
hashers are alive at a time. Copies are resolved to their first stored hunk,
with the last one kept. `finish` checks that every hunk was decoded, that
the raw SHA1 matches the header, and that the combined SHA1 over it and the
metadata matches too, so the hashed bytes are the bytes the image's author
hashed. How tracks are split and their byte order are not covered by any
checksum; the chdman test in `mistarr-fixture` pins them.

Every error names a field, track or hunk, never a digest. No input panics:
reads that end early are `corrupt`, slices of file data are bounds-checked
and arithmetic on file values is checked.

### Memory

`decode_budget(header)` bounds the decoder's heap: three hunk buffers and
the CD scratch, 16 bytes of map per hunk, the compressed map and one type
byte per hunk while the map is read, and codec state (LZMA dictionary of one
hunk, an 8 MiB zstd window, an inflater, and a FLAC block of up to 65535
samples of 8 channels). At the header limits it is under 24 MiB: 450,000
one-frame hunks come to about 21 MiB, 214-frame hunks to about 13 MiB. A
700 MB image in chdman's default 8-frame hunks holds under 1 MiB of buffers
and map. One image is decoded at a time and nothing is written to disk.

### Speed

Measured with `cargo run --example chd_bench` in `mistarr-fixture` on a 64 MB
image of half Mode 1 and half audio in chdman's default codecs, built for
armv7 with the release profile and run under `qemu-arm` on a desktop host.
The numbers compare builds; the board is slower, and the UI shows the rate
measured there.

| Release profile overrides | Decode rate |
|---|---|
| none | 7.1 MB/s |
| lzma-rs, claxon, ruzstd at opt-level 3 | 7.2 MB/s |
| those and sha1, md-5, crc32fast at opt-level 3 | 8.4 MB/s |
| those and mistarr-core at opt-level 3 | 23 MB/s |
| sha1, md-5, crc32fast and mistarr-core only | 20 MB/s |

The workspace `Cargo.toml` keeps all of them. The raw SHA1 pass costs one
SHA1 over the image: 0.16 s of the 2.8 s decode, about 6 %.
