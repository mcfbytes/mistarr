# Download clients

mistarr does not embed a torrent client. It drives the one that is already on
the image through the client's RPC. The stock MiSTer image ships **rtorrent**;
Buildroot_MiSTer ships **transmission-daemon**. Both are behind one trait so
the rest of the system never knows which is in use.

## Detection

At startup, and again when the user presses "re-detect":

1. If `client.kind` is set, use it and `client.url`.
2. Probe Transmission at `http://127.0.0.1:9091/transmission/rpc`. A 409 with
   `X-Transmission-Session-Id` counts as alive.
3. Probe rtorrent SCGI at `client.url` if set, then `127.0.0.1:5000`, then the
   unix socket `/media/fat/mistarr/rtorrent.sock`.
4. If nothing answers and `rtorrent` is on `PATH`, the status screen offers
   "Start rtorrent". Accepting writes a minimal rc to
   `/media/fat/mistarr/rtorrent.rc` and starts it detached under `nice`:

```
directory.default.set = /media/fat/mistarr/staging
session.path.set = /media/fat/mistarr/rtorrent-session
network.scgi.open_local = /media/fat/mistarr/rtorrent.sock
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
Serialize all calls through one client instance.

| Operation | RPC |
|---|---|
| add | `torrent-add` with `metainfo` (base64 .torrent) or `filename` (magnet), `download-dir`, `paused: true`, `files-unwanted` = all indices except wanted. For torrents with more than 2000 files send `files-wanted` with the wanted list and `priority-low` for the rest instead, then verify with `torrent-get`. |
| set_wanted | `torrent-set` with `files-wanted` / `files-unwanted` |
| start / stop | `torrent-start` / `torrent-stop` |
| status | `torrent-get` fields `id, hashString, status, percentDone, error, errorString, files, fileStats, rateDownload, rateUpload, uploadRatio, isFinished` |
| remove | `torrent-remove` with `delete-local-data` |
| rate limits | `session-set` `speed-limit-down`, `speed-limit-down-enabled`, same for up |
| seed policy | `torrent-set` `seedRatioLimit`, `seedRatioMode` (1 = use torrent limit, 2 = unlimited off) |

`fileStats[i].bytesCompleted` divided by `files[i].length` is the per-file
progress. A file is complete when equal and the torrent is not in
`status = 2` (checking).

## rtorrent

XML-RPC over SCGI. Implement the SCGI framing by hand; it is a netstring
header plus body. Use the `xmlrpc` crate for encoding. rtorrent's XML-RPC size
limit is configurable and defaults small; set `network.xmlrpc.size_limit.set`
in the generated rc and, when talking to a user's rtorrent, batch file
commands with `system.multicall` in chunks of 500.

| Operation | commands |
|---|---|
| add | `load.raw` (bytes) or `load.start`-less `load.normal` for magnets, with `d.directory.set` and `d.priority.set`; then for every file `f.priority.set` 0 (off) or 1 (normal); then `d.check_hash` is not needed. |
| set_wanted | `f.priority.set` per index, `d.update_priorities` |
| start / stop | `d.start` / `d.stop` |
| status | `d.multicall2` for `d.hash, d.name, d.state, d.complete, d.bytes_done, d.size_bytes, d.ratio, d.message`; `f.multicall` for `f.path, f.size_bytes, f.completed_chunks, f.size_chunks, f.priority` |
| remove | `d.erase`, and delete data ourselves if requested, since rtorrent does not |
| rate limits | `throttle.global_down.max_rate.set_kb`, `throttle.global_up.max_rate.set_kb` |
| seed policy | rtorrent has no per-torrent ratio; use `d.stop` when `d.ratio` passes the policy, evaluated on each poll. |

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
