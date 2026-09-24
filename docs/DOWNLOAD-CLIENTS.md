# Download clients

mistarr does not embed a torrent client. It drives the one that is already on
the image through the client's RPC. The stock MiSTer image ships **rtorrent**;
Buildroot_MiSTer ships **transmission-daemon**. Both are behind one trait so
the rest of the system never knows which is in use.

## Detection

At startup, and again when the user presses "re-detect":

1. If `client.kind` and `client.url` are both set, use them without probing.
   If only `client.kind` is set, run the probes below for that kind only.
2. Probe Transmission at `client.url` if it is an `http://` URL, else at
   `http://127.0.0.1:9091/transmission/rpc`. A 409 with
   `X-Transmission-Session-Id` counts as alive, as does a 401 whose
   `WWW-Authenticate` challenge names Transmission.
3. Probe rtorrent SCGI at `client.url` if it is an SCGI address, then
   `127.0.0.1:5000`, then the unix socket `/media/fat/mistarr/rtorrent.sock`.
   The probe only checks that the connection opens. An SCGI address is
   `host:port`, `scgi://host:port`, an absolute socket path or `scgi:///path`.
4. If nothing answers and `rtorrent` is on `PATH`, the status screen offers
   "Start rtorrent". Accepting writes a minimal rc to
   `/media/fat/mistarr/rtorrent.rc` and starts it detached under `nice`:

```
directory.default.set = /media/fat/mistarr/staging
session.path.set = /media/fat/mistarr/rtorrent-session
network.scgi.open_local = /media/fat/mistarr/rtorrent.sock
network.xmlrpc.size_limit.set = 8M
dht.mode.set = auto
protocol.pex.set = yes
throttle.global_down.max_rate.set_kb = 0
throttle.global_up.max_rate.set_kb = 0
system.daemon.set = true
```

mistarr never edits an rc file the user already has. If one exists without
SCGI enabled, the status screen says which line to add.

## Transmission

JSON-RPC over HTTP. Handle the 409 session-id handshake and retry once.
Serialize all calls through one client instance, holding the lock for the
whole operation. The torrent id is the lowercase `hashString`.

Transmission reads an empty `files-wanted`, `files-unwanted` or `priority-*`
array as "all files". An empty list is never sent to mean "none".

| Operation | RPC |
|---|---|
| add | `torrent-add` with `metainfo` (base64 .torrent) or `filename` (magnet), `download-dir`, `paused: true`, `files-unwanted` = all indices except wanted, file count taken from the metainfo. For torrents with more than 2000 files, add without a selection, then `torrent-set` `files-unwanted: []` (all off), then `torrent-set` `files-wanted` with the wanted list, then verify the `wanted` field with `torrent-get`. For a magnet, read `wanted` after adding and apply the selection the same way if metadata is present. A `torrent-duplicate` reply still gets the selection (via `torrent-set`) and the seed policy, so a retried add repairs a half-applied one. |
| set_wanted | `torrent-get` `wanted` for the file count, then `torrent-set` with `files-wanted` / `files-unwanted`, using the large-torrent sequence above past 2000 files |
| start / stop | `torrent-get` `id` to confirm the torrent exists, then `torrent-start` / `torrent-stop` |
| status | `torrent-get` fields `id, hashString, status, percentDone, error, errorString, files, fileStats, rateDownload, rateUpload, uploadRatio, isFinished` |
| files | `torrent-get` fields `name, files`; an empty `files` list is "metadata pending". A multi-file torrent's file names start with `<name>/`, which is stripped |
| remove | `torrent-get` `id`, then `torrent-remove` with `delete-local-data` |
| rate limits | `session-set` `speed-limit-down`, `speed-limit-down-enabled`, same for up; no limit or 0 sends only `*-enabled: false` |
| seed policy | On add and `set_seed_policy` (after a `torrent-get` `id` existence check): `torrent-set` `seedRatioMode: 1` (use this torrent's limit) with `seedRatioLimit` N for "until ratio N", or 0 for "none". "Client default" sends `seedRatioMode: 0` (session default), skipped on a fresh add where it is already 0. |

`fileStats[i].bytesCompleted` divided by `files[i].length` is the per-file
progress. A file is complete when equal and the torrent is not in
`status = 1` or `2` (waiting to check, checking).

Status mapping: 0 stopped, 1 and 2 checking, 3 and 5 queued, 4 downloading,
6 seeding. Only `error = 3` (local error, which stops the torrent) becomes the
error state; tracker warnings and errors do not.

## rtorrent

XML-RPC over SCGI. The SCGI framing is implemented by hand: a netstring
header of `CONTENT_LENGTH` (first) and `SCGI=1`, then the body; the reply's
CGI-style headers are stripped. XML-RPC is a small encoder and decoder on
quick-xml, since the `xmlrpc` crate pulls in a blocking HTTP client.
rtorrent's XML-RPC size limit is configurable and defaults small; the
generated rc raises it with `network.xmlrpc.size_limit.set`, and file
commands are always batched with `system.multicall` in chunks of 500 so a
user's own rtorrent accepts them too.

Every command after `load.*` takes the uppercase hex infohash as its target,
and file commands take `<HASH>:f<index>`. `load.*` does not return the hash,
so mistarr computes it: SHA-1 of the metainfo's `info` dictionary, or the
`xt=urn:btih:` value (hex or base32) of a magnet. rtorrent answers an unknown
hash with a fault naming the info-hash, which maps to "not found".

| Operation | commands |
|---|---|
| add | `d.hash` to see whether rtorrent already has it. If not, `load.raw` (`""`, base64 bytes, `d.directory.set="<dir>"`) or `load.normal` (`""`, magnet, `d.directory.set="<dir>"`), both of which leave the torrent stopped, then `d.directory.set` directly. The trailing command matters for magnets: when metadata arrives rtorrent erases the meta-download and creates the real torrent, replaying only the commands given to `load.*`. The file count comes from the metainfo on a fresh add, else from `d.is_meta` and `d.size_files`; a magnet still fetching metadata gets no selection. Then `f.priority.set` 0 (off) or 1 (normal) for every file and `d.update_priorities`. An existing torrent skips the load and directory and gets the selection and seed policy, so a retried add repairs a half-applied one. |
| set_wanted | `d.is_meta` and `d.size_files`, then `f.priority.set` for every index and `d.update_priorities` |
| start / stop | `d.start` / `d.stop` |
| status | One `system.multicall` on the hash: `d.state, d.is_active, d.complete, d.is_hash_checking, d.hashing, d.ratio, d.down.rate, d.up.rate, d.message, d.is_meta`, and `f.multicall` for `f.size_bytes, f.completed_chunks, f.size_chunks, f.priority`. `d.multicall2` is not used because it lists every torrent in a view on each call. |
| files | One `system.multicall`: `d.is_meta` (non-zero is "metadata pending") and `f.multicall` for `f.path, f.size_bytes`, whose paths are already relative to the torrent's directory |
| remove | With data: `d.directory`, `d.is_multi_file` and `f.multicall` `f.path` first, then delete the listed files ourselves, since rtorrent does not, through the remote path map; a multi-file torrent's emptied directories go too. Deletion carries on past a failed file. Then `d.erase`, and only after it the first deletion error, if any, so the torrent is never left erased with an unreadable file list. |
| rate limits | `throttle.global_down.max_rate.set_kb`, `throttle.global_up.max_rate.set_kb` with `""` and KiB/s; no limit sends 0 |
| seed policy | rtorrent has no per-torrent ratio. The client keeps each torrent's policy in memory and `status` sends `d.stop` when a seeding torrent's `d.ratio` (thousandths) reaches it; "none" stops as soon as it seeds, "client default" never. `is_finished` is derived on every poll, never stored: a stopped torrent whose wanted files are complete and whose ratio meets the current policy is finished, a seeding one is not, and one restarted outside mistarr is stopped again if it still meets the policy. `set_seed_policy` checks the torrent with `d.hash` and replaces the policy. The poller re-applies policies after a restart. |

Per-file progress is `f.size_bytes` prorated by `f.completed_chunks` over
`f.size_chunks`, so a file reads complete only when all its chunks are.
A file is wanted when `f.priority` is above 0. Status mapping: a check in
progress (`d.is_hash_checking`) or queued (`d.hashing` non-zero) is checking;
an inactive torrent with a `d.message` not starting `Tracker:` is
the error state; otherwise `d.state = 0` or inactive is stopped, every wanted
file complete (or `d.complete`) is seeding, anything else is downloading.

The add sequence for a selective download must be: load paused, set every
priority, then start. Loading started grabs the first pieces of every file
before priorities apply.

## Remote path mapping

When the client runs on another machine, its `download-dir` differs from
where mistarr sees the same bytes. `client.remote_path_map` is a list of
`{ remote, local }` prefixes applied to paths reported by the client. This is
the same model as the *arr apps. The common MiSTer case is a NAS writing to
the SD card over SMB, mapped to `/media/fat/mistarr/staging`.

## Polling

One poll task. Interval 5 s while any download is `transferring` or
`checking`, 60 s otherwise. A poll asks for every torrent mistarr added and
diffs per-file progress against `downloads`, emitting `download.changed` only
for rows that moved. Client errors set the client `unreachable` on the status
screen after three consecutive failures and back off to 5 minutes.

## Core gate

When CORENAME is not `MENU` the poller applies the `*_core` rate limits from
config; when it returns to `MENU` it restores the `*_menu` limits. It does not
stop torrents, because stopping and starting a large set torrent is expensive
in rtorrent.
