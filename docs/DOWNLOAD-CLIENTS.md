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
   `127.0.0.1:5000`, then the unix socket `rtorrent.sock` in the data
   directory.
   The probe only checks that the connection opens. An SCGI address is
   `host:port`, `scgi://host:port`, an absolute socket path or `scgi:///path`.
4. Detection also records which clients are installed, running or not:
   `transmission-daemon` or `rtorrent` on `PATH`, Buildroot_MiSTer's
   Transmission init script `/etc/init.d/S92transmission`, and its opt-in
   directory `/media/fat/linux/transmission`, whose presence makes that
   script start the daemon at boot.
5. While no client answers, detection runs again every minute, and once
   more when the poller marks the client unreachable after three failed
   polls, so a daemon started after mistarr is picked up without the
   wizard. Detection runs one at a time, and a result whose probe began
   before the stored one was taken is dropped. The client mistarr talks to
   changes only when a probe finds a different client that answers, or
   when the settings change it; a probe that finds nothing keeps it.

## Starting a stopped client

When no client answers and one is installed, the wizard's client step and
the System screen offer to start it. Nothing is started without that
explicit action, which is `POST /system/client/start` with `{ kind }`; the
server then re-detects for up to ten seconds and answers with the status.
Only one start runs at a time. A start command's output goes to
`client-start.log` in the data directory, and one still running after 30
seconds is killed and reported.

"Start Transmission" on Buildroot_MiSTer creates
`/media/fat/linux/transmission` if it is absent and runs
`/etc/init.d/S92transmission start`. The script seeds its own
`settings.json` there, with RPC on `127.0.0.1:9091`, and because the
directory now exists the image also starts the daemon at every boot;
removing the directory undoes that. Without the init script,
`transmission-daemon` is run with `--config-dir /media/fat/mistarr/transmission`
and `--download-dir /media/fat/mistarr/staging`, and daemonizes itself.

"Start rtorrent" writes the managed rc to `rtorrent.rc` in the data
directory, creates `rtorrent-session/` beside it and starts
`rtorrent -n -o system.daemon.set=true -o import="<data>/rtorrent.rc"` in its
own process group, under `nice` where the board has it. The rc is rewritten
on every start while its first line is the marker below; an `rtorrent.rc`
without that line belongs to the user and is used as it is. Paths in the rc
are quoted, so a data directory with spaces works; one containing a double
quote is refused. SCGI listens on `127.0.0.1:5000`, which detection probes,
since a unix socket cannot be created on the exFAT card. Daemon mode needs
rtorrent 0.9.7 or newer; an older rtorrent rejects the option and exits,
and a start whose rtorrent exits within two seconds is reported as failed
with the last line it logged. The rc, with the board's data directory:

```
# Written by mistarr on every start. Delete this line to keep your own edits.
directory.default.set = "/media/fat/mistarr/staging"
session.path.set = "/media/fat/mistarr/rtorrent-session"
network.scgi.open_port = 127.0.0.1:5000
network.xmlrpc.size_limit.set = 8M
dht.mode.set = auto
protocol.pex.set = yes
throttle.global_down.max_rate.set_kb = 0
throttle.global_up.max_rate.set_kb = 0
```

mistarr never edits an rc file the user owns. If one exists without
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
| status | `torrent-get` fields `id, hashString, status, leftUntilDone, error, errorString, fileStats, rateDownload, rateUpload, uploadRatio, isFinished`; not `files`, whose names would put a 100 000-file torrent's reply over the 16 MiB body limit on every poll |
| files | `torrent-get` fields `name, files`; an empty `files` list is "metadata pending". A multi-file torrent's file names start with `<name>/`, which is stripped |
| remove | `torrent-get` `id`, then `torrent-remove` with `delete-local-data` |
| rate limits | `session-set` `speed-limit-down`, `speed-limit-down-enabled`, same for up; no limit or 0 sends only `*-enabled: false` |
| seed policy | On add and `set_seed_policy` (after a `torrent-get` `id` existence check): `torrent-set` `seedRatioMode: 1` (use this torrent's limit) with `seedRatioLimit` N for "until ratio N". "None" sends `seedRatioMode: 2` (unlimited), so a session ratio limit never stops the torrent and only the poller does. "Client default" sends `seedRatioMode: 0` (session default), skipped on a fresh add where it is already 0. |

`fileStats[i].bytesCompleted` divided by the file's size from the metainfo
(`torrent_files.size`) is the per-file progress. A file is complete when
equal and the torrent is not in `status = 1` or `2` (waiting to check,
checking). The status carries no sizes, except that with `leftUntilDone = 0`
every wanted file is whole and reports its `bytesCompleted` as its size,
which is what the seed-policy stop reads.

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

rtorrent creates only the last level of a download directory, so mistarr
creates `staging/<infohash>/` before every add; a torrent loaded into a
missing parent fails with "Could not create directory".

Every command after `load.*` takes the uppercase hex infohash as its target,
and file commands take `<HASH>:f<index>`. `load.*` does not return the hash,
so mistarr computes it: SHA-1 of the metainfo's `info` dictionary, or the
`xt=urn:btih:` value (hex or base32) of a magnet. rtorrent answers an unknown
hash with a fault naming the info-hash, which maps to "not found".

| Operation | commands |
|---|---|
| add | `d.hash` to see whether rtorrent already has it. If not, `load.raw` (`""`, base64 bytes, `d.directory.set="<dir>"`) or `load.normal` (`""`, magnet, `d.directory.set="<dir>"`), both of which leave the torrent stopped, then `d.directory.set` directly, retried for up to 2 s while it answers "not found", since rtorrent may finish a load on its next tick. The trailing command matters for magnets: when metadata arrives rtorrent erases the meta-download and creates the real torrent, replaying only the commands given to `load.*`. The file count comes from the metainfo on a fresh add, else from `d.is_meta` and `d.size_files`; a magnet still fetching metadata gets no selection. Then `f.priority.set` 0 (off) or 1 (normal) for every file and `d.update_priorities`. An existing torrent skips the load and directory and gets the selection and seed policy, so a retried add repairs a half-applied one. |
| set_wanted | `d.is_meta` and `d.size_files`, then `f.priority.set` for every index and `d.update_priorities` |
| start / stop | `d.start` / `d.stop` |
| status | One `system.multicall` on the hash: `d.state, d.is_active, d.complete, d.is_hash_checking, d.hashing, d.ratio, d.down.rate, d.up.rate, d.message, d.is_meta`, and `f.multicall` for `f.size_bytes, f.completed_chunks, f.size_chunks, f.priority`. `d.multicall2` is not used because it lists every torrent in a view on each call. |
| files | One `system.multicall`: `d.is_meta` (non-zero is "metadata pending") and `f.multicall` for `f.path, f.size_bytes`, whose paths are already relative to the torrent's directory |
| remove | With data: `d.directory`, `d.is_multi_file` and `f.multicall` `f.path` first, then delete the listed files ourselves, since rtorrent does not, through the remote path map; a multi-file torrent's emptied directories go too. Deletion carries on past a failed file. Then `d.erase`, and only after it the first deletion error, if any, so the torrent is never left erased with an unreadable file list. |
| rate limits | `throttle.global_down.max_rate.set_kb`, `throttle.global_up.max_rate.set_kb` with `""` and KiB/s; no limit sends 0 |
| seed policy | rtorrent has no per-torrent ratio. The client keeps each torrent's policy in memory and `status` sends `d.stop` when a seeding torrent's `d.ratio` (thousandths) reaches it; "none" and "client default" never stop in the client, since under "none" the poller does. `is_finished` is derived on every poll, never stored: a stopped torrent whose wanted files are complete and whose ratio meets a ratio policy is finished, a seeding one is not, and one restarted outside mistarr is stopped again if it still meets the policy. `set_seed_policy` checks the torrent with `d.hash` and replaces the policy. The poller re-applies policies after a restart. |

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
`checking`, 60 s otherwise. A poll asks for every torrent with a transferring
or checking download and diffs per-file progress against `downloads`,
emitting `download.changed` only for rows that moved. The first poll of a
torrent after a new client handle, such as after a restart, re-applies its
seed policy, since rtorrent keeps policies in memory. Under seed policy
"none" the poll that hands a source's last open download to the importer
stops the torrent, provided every file selected in the client is complete and
the torrent is not already stopped; no client stops it on its own, so a want
that arrives while a file finishes extends a running torrent instead of
restarting a stopped one. A torrent the client no longer has fails its
downloads. Client errors set the client `unreachable` on
the status screen after three consecutive failed polls and back off to 5
minutes; the next answered poll sets it reachable again.

## Core gate

When CORENAME is not `MENU` the poller applies the `*_core` rate limits from
config; when it returns to `MENU` it restores the `*_menu` limits, once per
transition. A missing CORENAME counts as the menu. Limits the client refuses,
or that find no client, are retried at the CORENAME poll interval. It does not
stop torrents, because stopping and starting a large set torrent is expensive
in rtorrent.
