# Non-root plan: mistarr, Transmission and rtorrent

A proposal for running mistarr and its BitTorrent clients as non-root users on
MiSTer. It is not implemented. File and line references are to mistarr at
`c08f4d3`, Buildroot_MiSTer at `ba25ce1`, minijail `main` at
`linux-v2026.05.18`, Main_MiSTer at `57276f0` and Linux-Kernel_MiSTer at
`e24da58`. "Critic gap N" refers to the adversarial review this plan absorbed;
section 15 records how each was handled.

Markers:
- **[V]** verified, with file:line or URL.
- **[I]** inferred.
- **[U]** unknown. Section 13 gives the read-only board command that settles it.

## 1. Summary

**Permissions cannot do this on the SD card.** The card is exFAT, mounted once with no `uid=`/`gid=` and with `fmask=0022,dmask=0022`:
- Buildroot_MiSTer: `board/mister/common/initramfs-overlay/init:27` [V].
- Stock: the kernel mounts it with an empty option string ([do_mounts.c#L456-L474](https://github.com/MiSTer-devel/Linux-Kernel_MiSTer/blob/e24da58/init/do_mounts.c#L456-L474)) [V].

Every file is therefore `root:root 0755`. exFAT refuses a remount that changes owner or masks ([super.c#L751-L782](https://github.com/torvalds/linux/blob/v6.18/fs/exfat/super.c#L751-L782)) [V]. Idmapped mounts would need `CONFIG_USER_NS`, which is off on every MiSTer kernel [V]. So a plain non-root uid can write nothing on the card, and any card-wide grant lets it write everything. Writing anything on the card is root code execution (ADR 0031:58-61) [V].

**Recommended design.** Each daemon runs as its own numeric uid in a minijail0 mount-namespace jail that a root launcher builds:
- **Capabilities.** The only capability is `CAP_DAC_OVERRIDE`, held as an ambient capability, with `no_new_privs`.
- **Mount view.** It is an allow-list: no `/` bind, and no `/sys`.
- **Processes.** Each jail has its own PID namespace with the daemon as init (`-I`) and a fresh read-only procfs.
- **Bind sources.** They are anchored once at boot, before any jail exists, so a jailed process cannot redirect them.

Four small root pieces stay outside every jail:
1. **The launcher**: `mistarr.sh` on stock, plus `S92transmission` on Buildroot.
2. **`mistarr broker`**: core launches and client start, stop, freeze and thaw. It talks to the jail over an inherited socketpair.
3. **cgroup v2**: freezes the client, and freezes mistarr itself while the broker validates a launch.
4. **A fail-closed marker**: once a board has run jailed, nothing ever runs the daemons as root over state that jailed code wrote.

**Prerequisites that are now requirements, not decisions:**
- **API key on loopback.** A non-empty API key is required for loopback callers whenever a client runs on the board. Otherwise a jailed client is mistarr without needing any exploit (`http/mod.rs:38`, `config.rs:120`) [V].
- **"Isolated" requires a safe board posture.** The jail status reads "isolated" only while sshd refuses passwords, proftpd is not running, and Samba exports no public share. Root checks these at every boot. ADR 0031 records the threats: the root password is reset by each update, FTP runs as root with an anonymous block, and `[sdcard] public = yes, writable = yes` [V].

**Two residual risks the user must accept on stock.** Main follows symlinks under `games/` and opens mounted images read-write as root. That is true for broker launches and for images the user picks in the OSD browser (`file_io.cpp:1660-1674`, `user_io.cpp:2223-2224`) [V]. The stock kernel lets any process that can write a directory create exFAT symlinks (`fs/exfat/namei.c:624` at `e24da58`) [V]. A compromised mistarr could therefore make Main, as root, write sector data through a planted link. Buildroot can close this with a kernel patch (section 9). On stock it needs an upstream change to Main.

**rtorrent on stock is probably unusable today.** The stock binary very likely lacks XML-RPC [I, strong]. Confirm that before any rtorrent jail work.

## 2. Threat model

**Attackers:**
- **A** owns the client through hostile peer data.
- **B** owns mistarr through its HTTP API.
- B includes A by design. Control of the client's RPC is code execution as the client's uid: rtorrent `execute.*`, Transmission `script-torrent-done-filename` [V].

**What the design stops:**

| Stops | How |
|---|---|
| Writing outside a jail's read-write binds: `MiSTer`, `linux/`, `Scripts/`, `config/`, `_*`, `saves/`, the rootfs | Allow-list mount view. Bind sources are anchored at boot, so B cannot redirect a bind by swapping a path component |
| Reading secrets outside the view (`/etc/shadow`, host keys, `wpa_supplicant.conf`) | `/etc` is a tmpfs with single-file binds. `CAP_DAC_OVERRIDE` bypasses read checks too [V capabilities(7)], so anything visible is readable. The view is the only boundary |
| Driving the FPGA | `/dev/mem` needs `CAP_SYS_RAWIO` [V mem.c:600-605]. `/dev/MiSTer_cmd` is never in a jail. The broker accepts only installed cores |
| Signalling or ptracing Main, init or other daemons | Non-root uid, no `CAP_KILL`/`CAP_SYS_PTRACE`, own PID namespace |
| Sysctls and `/sys` writes (`uevent_helper` is 0644 and `CONFIG_UEVENT_HELPER=y` [V]) | No `/sys` in any jail. `/proc` is a fresh read-only procfs. A non-root euid fails `test_perm` [V proc_sysctl.c:425-434] |
| Moving root processes into a cgroup and freezing or killing them | No cgroupfs in any jail. Only the root broker writes `cgroup.freeze` |
| setuid escalation | `no_new_privs` |
| Client (A) reading mistarr's DB, config or API key | A's view holds only staging and its own state |

**What it does not stop:**

| Does not stop | Mitigation |
|---|---|
| Kernel exploits (no seccomp on any kernel [V]) | Boot sysctls: `io_uring_disabled=2`, `dmesg_restrict=1`, `kptr_restrict=2`. On Buildroot, `CONFIG_SECCOMP` (ADR 0031 T1 #5), then `minijail -S` |
| Network reach: LAN, and loopback services | API key required for loopback callers. Client RPC on unix sockets. Posture gate (section 1) |
| **A altering files after mistarr verified them.** An fd or a shared mapping opened in staging follows the inode through `rename(2)` [I], and placement never copies (`place.rs:356`) [V] | Freeze the client cgroup across hash, verify and rename. The broker scans the client's `/proc/*/fd` and `/proc/*/maps` for the staged inodes before placement (section 10, F3). Until that lands, this is residual |
| **Staged peer data sits under `games/.mistarr/`**, where Samba, Main's search and attract scripts can see it | Name it `.mistarr`, because Main's browser hides dot entries [V file_io.cpp:1714-1761]. Main search, SAM, update_all and Downloader are [U] (section 13) |
| **B, or the user via the OSD, making Main write through a symlink in `games/`** | Buildroot: restrict exFAT symlink creation to `CAP_SYS_ADMIN`. Stock: the broker refuses symlinks and freezes B while Main opens the file, plus a boot-time root report of symlinks under `games/`. The OSD path stays residual on stock |
| RAM exhaustion | rlimits; client `oom_score_adj` 800. No memcg [V] |
| B destroying the library or DB | By design |
| Root tools that read `games/` as root (Main's parsers, update_all) | Residual |

## 3. Current privileged needs

| Need | Code [V] | Root today | Route under the jail |
|---|---|---|---|
| Write data dir (DB, WAL, swap, dats, sources, log, lock, migrating) | config.rs:165-236; db/ram.rs:773-876; logging.rs:64-84 | yes | Anchored read-write bind of `<data>` plus DAC override |
| Staging, `.import`, quarantine; rename into `games/` | transfer.rs:254-256; place.rs:93-370 | yes | Staging moves to `<games>/.mistarr/staging` so the rename stays inside one mount (EXDEV [V rename.2]). The `Roots::new` invariant must change (place.rs:95, 209 reject staging under `games/` today) |
| Library scan, placement, rename | `[paths] games` | yes | Recursive anchored bind of `games/` |
| Read cores and MRAs (`_*`) | corename.rs:115-121; arcade.rs | no | Read-only binds |
| `load_core` to `/dev/MiSTer_cmd` plus an MGL in `/tmp` | mistarr-mister launch.rs:20,301-308,347-381 | yes | Broker |
| SIGSTOP/SIGCONT of the client; `/proc/<pid>/exe` and `fd` | freeze.rs:126-330; core_limits.rs:477-560 | same uid | Broker writes the client cgroup's `cgroup.freeze` |
| Start Transmission (`mkdir linux/transmission`, run `S92transmission`) | clients launch.rs:128-131 | yes | Broker verb |
| Start rtorrent as mistarr's child, with `<data>/rtorrent.rc` | launch.rs:142-172 | inherits | Root launcher or broker starts it in its own jail. Never as root with an rc from the data dir |
| Fallback `transmission-daemon` start (upstream defaults: RPC on 0.0.0.0, UPnP) | launch.rs:133-139 | inherits | Pass `--rpc-bind-address 127.0.0.1 --no-portmap` now |
| `/tmp/mistarr` RAM dir | db/mod.rs:868-926 | no | Private jail tmpfs, sized by the launcher |
| Thread names via `/proc/thread-self/comm` | threads.rs:209 | no | `prctl(PR_SET_NAME)` through rustix [I: rustix `thread::set_name`, check the docs] |
| Autostart line, install, upgrade, rollback | mistarr.sh:21,303-319; install.sh | yes | Stays root, outside the jails, with no writes through jail-writable paths |
| Supervisor pidfiles and `>> mistarr.log` written as root in `<data>` | mistarr.sh:7-12,30,130 | yes | Move to root-only `/run/mistarr/`. The daemon owns its log |
| ionice, nice, `RLIMIT_DATA`, port 8420 | io_priority.rs:118; memory.rs | no | Unchanged. `/usr/bin/ionice` is bound read-only |
| Transmission state in `/media/fat/linux/transmission` and the pidfile | S92transmission:11-20,75-76 | yes | Client jail. minijail `-f` writes the pidfile as root into `/run` |
| rtorrent SCGI on TCP `127.0.0.1:5000` | rtorrent.rs:37-48 | inherits | `open_local` socket in a tmpfs directory created once by root |

## 4. Recommended design and why

**Why minijail.** It is the only mechanism that confines writes by path on every kernel in play: stock 6.18.38 (and 5.15) and Buildroot 6.18.55 / RT 7.2.9. All of them have mount, PID, IPC and UTS namespaces, cgroups, capabilities, ambient capabilities and `no_new_privs`, and none has seccomp, user namespaces, Landlock or memcg [V]. The user's preference for minijail is therefore not optional hardening: it is the confinement. A non-root uid with DAC override is strictly better than capability-less uid 0, which still passes the euid-0 sysctl check (`core_pattern`) and shares a uid with Main [V].

**Rejected alternatives:**
- **Card mount `uid=`/`gid=` or group-writable masks.** The whole card becomes writable, which is root. It breaks sshd StrictModes. It is impossible on stock.
- **Plain uid drop** (`start-stop-daemon -c`, `su`). The uid cannot write exFAT at all. BusyBox `setpriv` cannot change uid [V].
- **uid 0 with no capabilities.** It passes the euid-0 sysctl test and owner-writes every visible root file and device.
- **`CONFIG_USER_NS` plus idmapped binds.** Buildroot only, and it opens user namespaces to every uid. Kept as a possible later refinement.
- **ext4 loop image for staging.** It breaks the same-filesystem rename, and the image has a fixed size.
- **bubblewrap.** `--uid` needs user namespaces, and it installs setuid-root.
- **firejail.** setuid-root, with a local-root history (CVE-2022-31214).
- **util-linux `unshare` + `setpriv`.** Buildroot only, and the namespace would be built in shell.
- **In-process Rust jail.** `unshare` is deprecated or unsafe in rustix, against `#![forbid(unsafe_code)]`.
- **Deny-list jail over a read-write `/media/fat`.** It cannot protect root-read paths that do not exist yet (`downloader.ini`, `Filters`, `Presets`, `savestates`, ...).
- **Binding `/`, even read-only.** With DAC override that exposes `/etc/shadow` (mode 0640 [V output/target/etc/shadow]).
- **Same uid for mistarr and the client.** The cgroup freeze makes it unnecessary.
- **Granting `CAP_KILL`.** It could kill Main.
- **Binding `/dev/MiSTer_cmd` into a jail.** It means FPGA control, and Main recreates the FIFO on every restart [V].
- **Bind-mounting cgroupfs into a jail.** v2 migration checks only file permission, which DAC override passes [V cgroup.c:5292-5337].
- **A socket path for the broker in a tmpfs that a jail can write.** Root-created objects in jail-writable directories can be raced.
- **Handing Main `/proc/<broker>/fd/N` paths.** Main derives OSD names, and possibly save names, from the path [I]. Rejected in favour of freezing B while Main opens the file.

## 5. Filesystem and ownership changes on the board

**exFAT.** Nothing is chowned. Write access comes from DAC override, bounded by the binds.

**Staging moves into `games/`.** The new key `[paths] staging` defaults to `<games>/.mistarr/staging`. `.import` and `quarantine` go under `<games>/.mistarr/` too. The same-mount rename then works for card, USB and CIFS libraries alike. Code that must follow (critic gap 11, verified):
- `Roots::new` and `discard` reject staging under `games/` (place.rs:95,209). The invariant becomes "inside staging".
- `library_path`/`is_plain` must refuse any target under `games/.mistarr`.
- The library scan, the arcade ZipIndex and `rename_in_library` must skip `.mistarr`.

**Anchors: fixing bind sources once per boot.** A root launcher validates each source while no jail is running. It walks the path component by component and refuses any symlink or non-directory (`[ -L ]` on each component, and `realpath` must equal the literal path). It then makes a root-only bind:

`mount --rbind <src> /run/mistarr/anchor/<name>`

- Anchor names: `games`, `data`, `staging`, `tstate`.
- `/run/mistarr` is mode 0700 and is never bound into a jail.
- Every jail binds from an anchor, never from a card path.
- A bind follows the directory's dentry, so B renaming a component later cannot redirect any jail [I, rig test].
- Restarting a jail reuses its anchors.
- Anchors are created only when no jail runs: at boot in S92 or `mistarr.sh start`, or after `mistarr.sh stop`.
- The re-check "immediately before each minijail call" that the critic proposed would itself be a TOCTOU race against B. Anchoring replaces it.

**Allow-list for anchor sources.** The values come from `jail.env`, written only from an interactive `install.sh` answer and never from `mistarr.toml`, which B can change through `PUT /system/settings` (http/system.rs:40 [V]).
- **games:** `/media/fat/games`, `/media/usbN/games`, or a CIFS mount whose type is checked against `/proc/mounts`.
- **data:** `/media/fat/mistarr` or a path below it.
- **staging:** `<games>/.mistarr/staging`.
- **Transmission state:** the fixed path `/media/fat/linux/transmission`.

**mistarr jail view** (illustrative; flags verified against `minijail0_cli.c`, behaviour [I, rig test]):

```
sh -c 'echo $$ > /sys/fs/cgroup/mistarr/cgroup.procs; exec /path/minijail0 -T static \
 -u 8420 -g 8420 -c 0x2 --ambient -n -I -l --uts -v -P /run/mistarr/root/mistarr \
 -b /bin -b /sbin -b /lib -b /usr \
 -k tmpfs,/etc,tmpfs,MS_NOSUID|MS_NODEV|MS_NOEXEC,size=64k \
 -b /etc/passwd -b /etc/group -b /etc/hosts -b /etc/ssl \
 -b /run/mistarr/net/resolv.conf,/etc/resolv.conf -b /media/fat/linux/timezone,/etc/localtime \
 -k proc,/proc,proc,MS_RDONLY|MS_NOSUID|MS_NODEV|MS_NOEXEC -d \
 -k tmpfs,/tmp,tmpfs,MS_NOSUID|MS_NODEV|MS_NOEXEC,size=<host /tmp size> \
 -b /tmp/CORENAME \
 -k /run/mistarr/anchor/games,/media/fat/games,none,MS_BIND|MS_REC \
 -b /run/mistarr/anchor/data,/media/fat/mistarr,1 \
 -b /media/fat/_Console -b /media/fat/_Computer -b /media/fat/_Arcade -b /media/fat/_Other \
 --preserve-fd <broker fd> -R RLIMIT_NPROC,64,64 -R RLIMIT_CORE,0,0 -- /path/mistarr'
```

Notes on this command line:
- **`-I`, always.** Without it, minijail's PID-namespace init handles SIGTERM with `_exit()` (libminijail.c:3250-3260 [V]), which kills the daemon with SIGKILL. That loses rtorrent's session and skips mistarr's graceful shutdown (main.rs:121-124).
- **Fresh read-only procfs via `-k`.** With `-I`, minijail skips its own `/proc` remount (`if (pid_namespace && !do_init)`, libminijail.c:3894,4221 [V]). The critic's `-b /proc,/proc` is wrong: it would show every host pid. Without `-I`, `-p` dies on `umount2("/proc")` in an empty view (libminijail.c:2573-2620,3160 [V]). The fresh procfs is mounted in the child after it has joined the new PID namespace [I, rig test].
- **`/tmp` via `-k tmpfs`, not `-t`.** minijail processes `-b`/`-k` mounts before `pivot_root` and `-t` after it (libminijail.c:3150-3160 [V]), so `-t` would cover the `/tmp/CORENAME` bind. `-t` also defaults to 64 MiB [V], which would push RAM-copy migrations onto the card (ram.rs:180-188).
- **`/etc/localtime`** is a symlink into `linux/` [V], hence the explicit bind of `/media/fat/linux/timezone`. Use `TZ` when that file is absent.
- **`resolv.conf`.** On Buildroot, `resolv.conf` → `/run/resolv.conf` [V]. The launcher keeps a copy at `/run/mistarr/net/resolv.conf` and refreshes it in place (`cat > existing`), so the bound inode stays valid.
- **No `/sys`, no cgroupfs, no `linux/`, `Scripts/`, `config/` or `saves/`.**

**Client jail view:** the same skeleton, plus:
- the staging anchor (read-write, `MS_NOSUID|MS_NODEV|MS_NOEXEC`), mounted at the client's existing path (section 11);
- its own state (Transmission: the `tstate` anchor; rtorrent: `<session>` and the rc file read-only);
- its RPC socket directory (`/run/transmission-rpc` or `/run/mistarr-rt`), created once by root and never touched by root again;
- `/dev/log` for syslog [U whether syslogd recreates it].

**Mount propagation (critic gap 13, accepted with a correction).** minijail0's CLI defaults `-v` to `MS_SLAVE` (minijail0_cli.c:1035-1067 [V]). The `MS_PRIVATE` default the critic cited is the library default (libminijail.c:527). Either way, host mounts are private unless something marks them shared [I], so a later CIFS mount or USB plug does not reach a running jail. Fix:
- The launcher waits for every mount that `jail.env` expects (`mountpoint -q` plus the filesystem type in `/proc/mounts`).
- It refuses to anchor when `games/` is still the bare card directory but a CIFS or USB filesystem is expected.
- It marks the anchors `--make-rshared`, so umount events reach the jails [I, rig test].
- It restarts the jails on usbmount events.
- `cifs_mount.sh` runs from `user-startup.sh`, so `mistarr.sh start` must run after it there.

**USB and CIFS.** usbmount mounts `sync,noexec,nodev` [V]. DAC override covers vfat, exfat and ext4 USB drives. On a CIFS share the server enforces access.

**Root-only state.** Pidfiles (minijail `-f`), the launcher log, anchors, posture results and the `resolv.conf` copy all live under `/run/mistarr/` (mode 0700), which no jail sees. On stock, if `/run` is not tmpfs [U], use `/tmp/.mistarr-root` instead. No jail sees the host `/tmp`.

## 6. Users, groups and boot persistence

- **Ids:** mistarr 8420/8420, rtorrent 8421/8421, Transmission 8422/8422 (pinned on Buildroot). No supplementary groups: under DAC override they only widen access (critic gap 21). The ids sit outside stock's 1000-1007 and Buildroot's auto 100-999 [V]. minijail takes numeric ids, so no passwd entry is needed [V].
- **Stock:** `/etc/passwd` lives in `linux.img`, which every update replaces [V Downloader constants.py#L102-L110]. Add no users there. Persistence comes from `user-startup.sh`, which survives updates [V]. It runs `Scripts/mistarr.sh`, which rebuilds the jails each boot.
- **Buildroot_MiSTer:** pin `transmission` (8422) and add `mistarr` (8420) in a `BR2_ROOTFS_USERS_TABLES` file; it is empty today [V].
- **Shutdown (critic gap 17, verified).** `S99user` passes `"$1"` to `user-startup.sh` on both images (Buildroot S99user:6 and the stock inventory [V]). mistarr's line is `[ -x X ] && X start &` (mistarr.sh:21 [V]), which ignores `stop`. Then `rcK` runs, and `umount -a -r` follows while the jails still hold the card (inittab:95-97 [V]). New line:

  `case "$1" in stop) [ -x X ] && X stop;; *) [ -x X ] && X start & ;; esac`

  `install.sh` rewrites the old line in place.
- **cgroups:** mount cgroup2 at `/sys/fs/cgroup`, or at `/run/cgroup2` if v1 occupies that path [U]. Create `mistarr/` and `torrent/` and never remove them. Enter each one with `sh -c 'echo $$ > .../cgroup.procs; exec minijail0 ...'`. Inside `sh -c`, `$$` is that new process. In a subshell `( ... )` it would be the parent's pid (critic gap 10, POSIX [V]), and mistarr.sh's supervisor is such a subshell (mistarr.sh:187 [V]). Nothing chowns any cgroup file, because only root writes them.

## 7. Transmission configuration (Buildroot_MiSTer, a separate change)

**`S92transmission start`, as root:**
1. Read the staging path from `/media/fat/linux/transmission.jail`. mistarr's `install.sh` writes it; no jail sees it.
2. Validate and anchor the state and staging directories (section 5). If no valid staging anchor exists (for example a CIFS library that is not mounted yet), start Transmission without mistarr staging, or defer to `mistarr.sh` through the broker [decision].
3. **Seed settings inside the jail**, as uid 8422 with the same view (critic gap 9; the current root `cat > "$SETTINGS.new"` follows a planted symlink, S92transmission:39-62 [V]):

   ```
   minijail0 <view> -- /bin/sh -c 'umask 077; set -C; [ -e settings.json ] || cat > settings.json <<EOF ... EOF'
   ```

   `set -C` uses `O_EXCL`, which refuses a symlink.
4. `sh -c 'echo $$ > /sys/fs/cgroup/torrent/cgroup.procs; echo 800 > /proc/self/oom_score_adj; exec minijail0 -T static -i -f /run/transmission/jail.pid -u 8422 -g 8422 -c 0x2 --ambient -n -I -l --uts -v -P ... <view> -R RLIMIT_NOFILE,1024,1024 -R RLIMIT_CORE,0,0 -- /usr/bin/transmission-daemon --foreground --config-dir /media/fat/linux/transmission'`

**Seed keys:**
- `rpc-bind-address: "unix:/run/transmission-rpc/rpc.sock"` with `rpc-socket-mode: "0750"` (4.1.3 [V rpc-server.cc#L68]).
- `script-torrent-done-enabled: false`, `script-torrent-added-enabled: false`, `blocklist-enabled: false`, `port-forwarding-enabled: false`.
- Optional: `TRANSMISSION_WEB_HOME` pointing at an empty directory [I].

**`stop`.** Thaw the `torrent` cgroup first: a frozen task does not handle SIGTERM. Then SIGTERM the outer pid from `-f` and wait for that pid to exit, matching its start time. Do not wait for the pidfile, because the daemon no longer writes one. Never SIGKILL inside the existing 20 s window.

**Downgrade safety (critic gap 4).** On its first jailed start, the new S92 renames the state directory to `/media/fat/linux/transmission-jailed/` and gates on that name, after checking that it is not a symlink. An older image's root S92 then finds no `linux/transmission` and does not start: it fails closed instead of running root over a settings file that A wrote. The `transmission.jail` file and `S92` document this. This is an alternative to sanitising `settings.json` from root, which would mean root parsing A's JSON.

**mistarr side:**
- HTTP-over-unix connector, or wire `[client]` credentials to `with_credentials` (client.rs:84-86 [V]).
- Wire `remote_path_map` for Transmission replies. Only rtorrent gets it today (client.rs:87-93 [V]), while the add path uses `to_remote` for both (transfer.rs:255 [V]).
- Whether `transmission-remote` can use a unix socket is [U].

## 8. rtorrent configuration (stock)

**Step 0.** Run the XML-RPC board check. If XML-RPC is absent, mistarr detects and reports it (`XMLRPC not supported.` on 0.9.x, fault -501 on 0.15.x), and "rtorrent on stock" means a user-installed build [decision].

**Managed rc:**
- `system.umask.set = 0007`
- `network.scgi.open_local = "/run/mistarr-rt/rtorrent.sock"`
- No `execute`, `schedule2` or watch directories.
- Fix `DEFAULT_RTORRENT_SOCKET` and its doc comment (detect.rs:12-16 [V]).

**Never as root over a data-dir rc.** `write_rc` keeps any rc without the marker as "the user's own" (launch.rs:160-172 [V]), so B can leave `execute` lines in it. In jailed mode:
- the rc is regenerated by the launcher from a root-only template and bound read-only;
- a user rc is used only if `jail.env` names it, and it then runs jailed like everything else;
- in root mode, see section 11 (fail closed).

**Start.** `mistarr.sh` or the broker:

```
sh -c 'echo $$ > .../torrent/cgroup.procs; echo 800 > /proc/self/oom_score_adj; exec minijail0 -T static -i -f /run/mistarr/rt.pid -u 8421 -g 8421 -c 0x2 --ambient -n -I -l --uts -v -P ... <view> -- /usr/bin/rtorrent -n -o system.daemon.set=true -o import=<rc>'
```

- rtorrent does not fork in daemon mode [I, from its source; rig test], so `-I` is right.
- `system.pid` becomes a namespace pid. That no longer matters, because freezing goes through the cgroup.
- **Stop:** thaw, SIGTERM the outer pid, wait for a clean session save (rig test: `kill -TERM` produces up-to-date `.rtorrent` session files).

## 9. Sandboxing: the minijail wrapper, kernel options and fallbacks

| Feature (flag) | Kernel option | Stock 6.18.38 | BR 6.18.55 / RT 7.2.9 | If missing |
|---|---|---|---|---|
| uid/gid, caps, ambient, `no_new_privs`, rlimits | MULTIUSER / core | y [V] | y [V] | — |
| mount ns, binds, `pivot_root`, tmpfs, procfs | NAMESPACES, TMPFS, PROC_FS | y [V] | y [V] | — |
| PID ns (`-I`) | PID_NS | y [V] | y [V] | **Refuse to jail.** Without a PID namespace the client could see and race other jails' processes. Not optional |
| IPC/UTS ns | IPC_NS/UTS_NS | y [V] | y [V] | Drop the flags |
| cgroup v2 freeze | CGROUPS | y [V] | y [V] | Held-uploads fallback (DOWNLOAD-CLIENTS.md:200-204) |
| seccomp (`-S`) | SECCOMP(_FILTER) | n [V] | n [V] (ADR 0031 T1 #5) | Never pass `-S`. Build minijail without soft-fail, so a requested policy fails closed |
| Landlock | SECURITY_LANDLOCK | n [V] | n [V] | The mount view is the boundary. minijail silently ignores `--fs-path-*` [V] |
| user ns | USER_NS | n [V] | n [V] | DAC override plus binds |
| memory/pids caps | MEMCG/CGROUP_PIDS | n [V] | n [V] | `-R RLIMIT_DATA` (measured), `RLIMIT_NPROC` > WORKERS 2 + BLOCKING 4 + named threads (memory.rs:8-11 [V]), `oom_score_adj` |
| io_uring off | IO_URING=y [V] | sysctl | sysctl | `kernel.io_uring_disabled=2`, after checking Main does not use it [U]. The sysctl exists from 6.6, so not on 5.15 |

**Kernel hardening set at boot** by `mistarr.sh` on stock and by `/etc/sysctl.d/` on Buildroot:
- `kernel.io_uring_disabled=2`
- `kernel.dmesg_restrict=1`
- `kernel.kptr_restrict=2`

Buildroot can also turn off `CONFIG_UEVENT_HELPER`, since eudev does the job [V; check no caller]. On every image a jail sees no `/sys`, and the self-test enforces that.

**Stock.** mistarr ships a static armv7 musl `minijail0`: 243 KB stripped in a zig 0.16 trial [V]; zig 0.15.2 is [U]. It must always run with `-T static`, because preload mode fails with `dlopen(): Dynamic loading not supported` [V]. Build it without `BLOCK_SYMLINKS_IN_BINDMOUNT_PATHS` (a compile-time ChromeOS option, util.h:236-243 [V]); the launcher does its own checks.

**Buildroot_MiSTer.** Add `package/minijail` (dynamic, against the image's libcap), and still invoke it with `-T static` for one code path. `mistarr.sh` prefers `/usr/bin/minijail0` when present.

**Kernel patch for symlinks (Buildroot).** Change patch 0031 so `exfat_symlink` requires `capable(CAP_SYS_ADMIN)`:
- The arcade organizer runs as root, so `_Arcade/_Organized` keeps working (ADR 0019 [V]).
- mistarr creates no symlinks outside its tests [V grep].
- Scope: a jail can no longer create symlinks on the card.
- It does not cover ext4 USB or CIFS `games/`. Those remain residual and are reported by the boot scan.

**Self-test** (every boot, before declaring "isolated"), run inside each jail:
- `cat /etc/shadow` fails;
- `/sys` is absent;
- `/proc/sys` is read-only;
- `/proc/self/status` shows `NoNewPrivs: 1` and `CapEff` = `0x2`;
- `mountinfo` lists only allow-listed mount points.

**Fallback (critic gap 4).**
- A board that has never been jailed keeps today's root mode, with a "not sandboxed" warning.
- Once `jail.env` has existed, a root-owned marker (`jailed=1` in `jail.env`) makes the launcher refuse to start anything when minijail is missing or the self-test fails.
- The new mistarr binary refuses to run at euid 0 when the marker exists, unless given `--unsafe-root`.

## 10. mistarr code, install.sh and mistarr.sh changes

**Code (mistarr):**
1. **Staged-file safety** (gap 2, needed now). Open the staging root as an `O_DIRECTORY` fd and walk every relative path with `openat`/`statat(AT_SYMLINK_NOFOLLOW)` (rustix `fs`, no `unsafe`). Refuse anything that is not a regular file or a directory at any depth. Rename with `renameat` on the verified directory fds. Apply the same rule to `discard` (place.rs:205-221 [V]), which calls `remove_dir_all` on a path. Today there is no lstat or no-follow check anywhere in import, place, transfer or scan [V grep]. The previous draft wrongly claimed place.rs already refused non-regular files.
2. **rtorrent delete confinement** to staging (rtorrent.rs:470-477,771-829 [V]).
3. **`[paths] staging`**, plus the `Roots::new`, `library_path` and scan changes in section 5, and a proptest that no library path resolves into `.mistarr`.
4. **Broker** (`mistarr broker`, synchronous, in mistarr-mister):
   - It runs as root and is the parent of the mistarr jail. It hands the jail one end of a socketpair (`minijail0 --preserve-fd`, minijail0_cli.c:567 [V]). Nothing exists in a filesystem or as an abstract name that a jail could squat. The fallback is an abstract socket with mutual `SO_PEERCRED` checks (rustix `net`).
   - Verbs: `launch_core`, `launch_game`, `launch_mra`, `start_client`, `stop_client`, `freeze_client`, `thaw_client`, `quiesce_client`, `posture`.
   - **Launch validation** (gap 3):
     1. freeze the `mistarr` cgroup;
     2. resolve the core under `_*` (symlinks allowed only if the target stays inside `_*` roots, which no jail can write);
     3. resolve the game beneath `games/` component by component with no symlinks, and refuse Mount-mode launches if any component is a link;
     4. write its own MGL in `/run/mistarr/mgl`;
     5. write the FIFO;
     6. poll `/proc/<Main>/fd` until Main holds the inode (timeout);
     7. thaw.
   - **`quiesce_client`** (gap 12): freeze the `torrent` cgroup, then scan every client task's `/proc/*/fd` and `/proc/*/maps` for the staged inodes. Placement proceeds only while the client stays frozen and holds none of them. The cost is paused downloads during hash, verify and rename [decision].
5. **Freeze backend.** freeze.rs and core_limits.rs call the broker. Keep SIGSTOP only in root mode.
6. **Transports and client start** as in sections 7-8. "Start" goes through the broker. The fallback transmission start passes `--rpc-bind-address 127.0.0.1 --no-portmap`.
7. **API key** (gap 6). In jailed mode with a client on the board, refuse an empty `api_key` for loopback callers. `install.sh` generates the key and stores it in `mistarr.toml`, which no client jail sees. The web UI already supports entering a key (App.svelte:21-72 [V]).
8. **Startup self-check.**
   - Log euid, `NoNewPrivs` and `CapEff`.
   - Refuse euid 0 when the marker exists.
   - Thread names via `prctl`.
   - `held_elsewhere` (ram.rs:490-531) is blind across PID namespaces. Rely on the flock in lock.rs.

**mistarr.sh:**
- Pidfiles and the launcher log go to `/run/mistarr/`.
- The autostart line passes `"$1"` (section 6).
- `start` does, in order:
  1. load and validate `jail.env` against the allow-list;
  2. wait for the expected mounts;
  3. run the posture checks (sshd `PasswordAuthentication`, proftpd running, smbd with a public share) and set boot sysctls;
  4. mount cgroup2;
  5. create the anchors;
  6. refresh the `resolv.conf` copy;
  7. start the rtorrent jail (stock);
  8. run the broker, which supervises the mistarr jail.
- `stop`: thaw the clients, SIGTERM the broker, which stops the mistarr jail and then rtorrent; wait for clean exits.
- `tail` of the log filters control characters (gap 21).
- `doctor` runs jailed.

**install.sh:**
- The binary, `minijail0`, `jail.env` and the marker live in a root-only directory outside every bind [decision on location].
- Data-dir copies (`save_prev`, `restore_db`, `commit_new`) run inside a minijail as 8420 with the same view, not as root through `cp` (install.sh:107,181,198 [V]).
- Run `S92transmission stop` and confirm the daemon exited before moving staging.
- Generate `api_key`.
- Write `[paths] staging` explicitly.
- Rewrite the autostart line.
- Write `transmission.jail` on Buildroot.

**Docs:**
- New `docs/SANDBOX.md`: views, anchors, broker protocol, cgroup contract, self-test, board checks.
- Updates to DEPLOYMENT.md, DOWNLOAD-CLIENTS.md and ARCHITECTURE.md (budgets, `[paths] staging`).
- A `minijail0` dependency justification.

## 11. Migration of existing installs

1. **Opt in, then default.** Jailing is opt-in (interactive `install.sh`) until rig-tested on both kernels, then becomes the default.
2. **Upgrade order** (root, no jail running):
   1. stop mistarr, thawing the old way;
   2. stop a root rtorrent and a root Transmission (`S92transmission stop`, then check the pid is gone);
   3. scrub symlinks under `<data>` (exFAT can hold them [V]);
   4. move the binary;
   5. remove a root-owned `/tmp/mistarr` (db/mod.rs:920 [V]).
3. **Staging move without re-verify (gap 19).**
   - Rename `<data>/staging` to `<games>/.mistarr/staging`; this works when both are on one filesystem, which today's placement already requires (ARCHITECTURE.md:606-608).
   - Inside the client jail, bind the staging anchor at the old path `/media/fat/mistarr/staging`. Set `remote_path_map = [{remote="/media/fat/mistarr/staging", local="<games>/.mistarr/staging"}]`.
   - No torrent is re-pointed and nothing is re-verified. Transmission reply mapping needs the wiring noted in section 7.
4. **Rollback across the jail boundary.** `install.sh` refuses unless given `--unsafe-root`. If given, it first:
   - replaces `rtorrent.rc` with the managed one;
   - scrubs symlinks under `<data>` and `<games>/.mistarr`;
   - moves staging back and drops the path map;
   - verifies the restored binary against its recorded hash.

   Old binaries know nothing of the marker, so this check lives in the new `install.sh`.
5. **Transmission (Buildroot).** The image update brings the jailed S92. The old daemon is stopped by the old S92 at shutdown: `S92transmission` is an init script, so `rcK` stops it, unlike mistarr. The state directory is renamed once (section 7). Torrents outside the views go to error, and the release notes say so.

## 12. Risks and what could break

- **The broker is new root code.** Keep it single-threaded with no DB and no network. Use proptest and fuzzing on request parsing.
- **Launch latency.** Launches freeze mistarr for up to the Main-open timeout. Placement with quiesce pauses downloads during hashing; large CHDs on a `sync` card take minutes.
- **Anchors and propagation** are [I] until rig-tested: dentry-following binds, `--make-rshared` with the minijail0 default `MS_SLAVE`, and stale `/tmp/CORENAME` or `resolv.conf` file binds.
- **DNS.** A file bind goes stale if dhcpcd replaces the file by rename; hence the refreshed copy.
- **Fail-closed can strand users.** A board where minijail breaks after an update refuses to start. Doctor output and `--unsafe-root` are the escape hatch.
- **Ambient `CAP_DAC_OVERRIDE` through `-u`** is untested on the board (qemu-user cannot test capabilities).
- **Main symlink chain on stock** stays open through the OSD browser.
- **Placed files altered by A** stay possible until `quiesce` lands.
- **`.mistarr` under `games/`** is visible to Samba, Main's search and attract scripts [U].
- **`kernel.io_uring_disabled=2`** may break a component that uses io_uring [U].
- **minijail0 is a C dependency** with security fixes to track. The zig 0.15.2 build is untested.
- **Stock rtorrent may lack XML-RPC.**
- **RAM.** Each jail's tmpfs counts against 488 MiB. Size caps are only caps, but must be set.

## 13. Board checks needed before implementation

All are read-only and none were run. The full list is in `board_checks`. The decisive ones:
- card mount options;
- FIFO mode and Main's umask;
- the running kernel config;
- whether cgroup2 is mountable, and any v1 mounts;
- whether `/run` is tmpfs;
- mount propagation flags (`mountinfo`);
- the `resolv.conf` and `localtime` targets;
- rtorrent XML-RPC;
- sshd, proftpd and smb posture;
- the `user-startup.sh` content;
- io_uring use by Main;
- the sysctl defaults;
- symlinks present under `games/`;
- client memory and thread use;
- the stock SSH host key location.

## 14. Open decisions

See `decisions_for_user`.

## 15. Disposition of the critic's findings

All 21 gaps were checked; 19 are accepted as stated. Four are accepted with corrections:
- **Gap 1.** `-I` is correct. Do not add `-b /proc,/proc`: with `-I`, minijail skips the `/proc` remount (libminijail.c:3894,4221 [V]), and a host `/proc` bind would expose every host pid. Mount a fresh procfs with `-k` instead.
- **Gap 8.** The proposed re-check before each minijail call is still a TOCTOU race against B. It is replaced by anchoring bind sources while no jail runs.
- **Gap 13.** The minijail0 CLI already defaults to `MS_SLAVE` (minijail0_cli.c:1035-1067 [V]). `-K slave` alone changes nothing unless the host mounts are made shared; the outcome the critic described is right.
- **Gap 14.** Resolved by never exposing cgroupfs to any jail (the broker writes `cgroup.freeze`), instead of `nsdelegate`. `-I` is mandatory anyway.

Two additions beyond the critic:
- **The OSD browser path.** Main lists symlinks as regular files (file_io.cpp:1660-1674 [V]), so broker checks alone cannot close the Main symlink chain.
- **minijail mount ordering.** `-t` mounts after `pivot_root` (libminijail.c:3150-3160 [V]) and would cover a `/tmp/CORENAME` bind.

None of the critic's findings is rejected outright.
