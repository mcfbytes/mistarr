#!/bin/sh
# MiSTer Scripts-menu launcher for mistarr. POSIX sh, runs under BusyBox ash.
# start|stop|status|restart; no argument runs the Scripts-menu flow.

ROOT="${MISTARR_ROOT:-/media/fat}"
BIN="$ROOT/mistarr/mistarr"
PIDFILE="$ROOT/mistarr/mistarr.pid"
# The supervisor that restarts the daemon after a crash.
SUPERFILE="$ROOT/mistarr/supervisor.pid"
# Held by one start at a time; in /tmp so a reboot clears it.
STARTLOCK="${MISTARR_RUNDIR:-/tmp}/mistarr.start.lock"
LOGFILE="$ROOT/mistarr/mistarr.log"
STARTUP="$ROOT/linux/user-startup.sh"
PORT="${MISTARR_PORT:-8420}"
# Where mistarr records a download client it stopped while a core runs.
FROZEN="${MISTARR_FROZEN:-${MISTARR_TEMP_DIR:-/tmp/mistarr}/client.frozen}"
PROCDIR="${MISTARR_PROC:-/proc}"
# Resolved absolute path to this script, wherever it was invoked from.
SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
NAME=$(basename "$0")
STARTUP_LINE="[ -x $SELF ] && $SELF start &"
# Restart backoff in seconds, and how many crashes within CRASH_WINDOW end it.
BACKOFF_FIRST="${MISTARR_BACKOFF:-5}"
BACKOFF_MAX=300
CRASH_LIMIT="${MISTARR_CRASH_LIMIT:-5}"
CRASH_WINDOW=600

# Appends a line from this script to the log.
note() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') mistarr.sh: $*" >>"$LOGFILE"
}

# True when pid $1 is alive and its command line contains $2. Without /proc
# or ps a live pid is trusted.
runs() {
    [ -n "$1" ] && kill -0 "$1" 2>/dev/null || return 1
    if [ -r "/proc/$1/cmdline" ]; then
        tr '\0' '\n' < "/proc/$1/cmdline" | grep -qF "$2"
        return
    fi
    if command -v ps >/dev/null 2>&1; then
        ps -p "$1" -o args= 2>/dev/null | grep -qF "$2"
        return
    fi
    return 0
}

# True when $PIDFILE names a live process running $BIN. A pidfile naming a
# dead pid, or one still not running $BIN a second later, is removed.
is_running() {
    [ -f "$PIDFILE" ] || return 1
    pid=$(cat "$PIDFILE" 2>/dev/null)
    runs "$pid" "$BIN" && return 0
    # A daemon just forked has not exec'd $BIN yet; look once more.
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        sleep 1
        runs "$pid" "$BIN" && return 0
    fi
    [ "$(cat "$PIDFILE" 2>/dev/null)" = "$pid" ] && rm -f "$PIDFILE"
    return 1
}

# Prints the supervisor's pid when one of ours is alive.
supervisor_pid() {
    sup=$(cat "$SUPERFILE" 2>/dev/null)
    runs "$sup" "$NAME" && echo "$sup"
}

# True when pid $1 is a live start of this script.
is_starter() {
    runs "$1" "$NAME"
}

# Creates the lock file holding our pid; noclobber makes the create exclusive.
create_start_lock() {
    (set -C; echo $$ > "$STARTLOCK") 2>/dev/null
}

# Takes the start lock. A lock whose holder is gone is replaced by renaming
# our own file over it, then kept only if it still names us a second later.
take_start_lock() {
    mkdir -p "$(dirname "$STARTLOCK")" 2>/dev/null
    create_start_lock && return 0
    holder=$(cat "$STARTLOCK" 2>/dev/null)
    # The holder writes its pid just after creating the file; give it that moment.
    if [ -z "$holder" ]; then
        sleep 1
        holder=$(cat "$STARTLOCK" 2>/dev/null)
    fi
    is_starter "$holder" && return 1
    echo $$ > "$STARTLOCK.$$"
    mv -f "$STARTLOCK.$$" "$STARTLOCK" || return 1
    sleep 1
    [ "$(cat "$STARTLOCK" 2>/dev/null)" = "$$" ]
}

release_start_lock() {
    [ "$(cat "$STARTLOCK" 2>/dev/null)" = "$$" ] && rm -f "$STARTLOCK"
}

do_start() {
    if ! take_start_lock; then
        echo "mistarr is already starting"
        return 0
    fi
    start_locked
    status=$?
    release_start_lock
    return "$status"
}

# Runs the daemon, restarting it after an abnormal exit with a backoff that
# doubles from BACKOFF_FIRST to BACKOFF_MAX, until CRASH_LIMIT crashes fall
# within CRASH_WINDOW seconds. A clean exit or a stop ends it.
supervise() {
    # Let go of the caller's output, or a `$(mistarr.sh start)` never returns.
    exec </dev/null >/dev/null 2>&1
    stopping=0
    child=""
    sleeper=""
    trap 'stopping=1; [ -n "$child" ] && kill "$child" 2>/dev/null; [ -n "$sleeper" ] && kill "$sleeper" 2>/dev/null' TERM INT
    delay="$BACKOFF_FIRST"
    crashes=""
    # nice may be absent from the board's BusyBox. The daemon sets its own
    # I/O class, idle only while a core runs.
    prio=""
    command -v nice >/dev/null 2>&1 && prio="nice -n 10"
    while [ "$stopping" -eq 0 ]; do
        # shellcheck disable=SC2086
        $prio "$BIN" </dev/null >>"$LOGFILE" 2>&1 &
        child=$!
        echo "$child" > "$PIDFILE"
        wait "$child"
        code=$?
        # A trapped signal ends `wait` early; wait again for the exit status.
        while kill -0 "$child" 2>/dev/null; do
            wait "$child"
            code=$?
        done
        [ "$(cat "$PIDFILE" 2>/dev/null)" = "$child" ] && rm -f "$PIDFILE"
        child=""
        [ "$stopping" -eq 1 ] && break
        if [ "$code" -eq 0 ]; then
            note "mistarr exited cleanly; not restarting"
            break
        fi
        now=$(date +%s)
        recent=""
        n=0
        for t in $crashes "$now"; do
            if [ $((now - t)) -lt "$CRASH_WINDOW" ]; then
                recent="$recent $t"
                n=$((n + 1))
            fi
        done
        crashes="$recent"
        if [ "$n" -ge "$CRASH_LIMIT" ]; then
            note "mistarr crashed $n times within $CRASH_WINDOW s (last status $code); giving up"
            break
        fi
        note "mistarr exited with status $code; restarting in $delay s"
        sleep "$delay" &
        sleeper=$!
        wait "$sleeper"
        sleeper=""
        delay=$((delay * 2))
        [ "$delay" -gt "$BACKOFF_MAX" ] && delay="$BACKOFF_MAX"
    done
    [ "$(cat "$SUPERFILE" 2>/dev/null)" = "$(sh -c 'echo $PPID')" ] && rm -f "$SUPERFILE"
}

start_locked() {
    if is_running; then
        echo "mistarr running (pid $(cat "$PIDFILE"))"
        return 0
    fi
    sup=$(supervisor_pid)
    if [ -n "$sup" ]; then
        echo "mistarr is restarting after a crash (supervisor pid $sup)"
        return 0
    fi
    if [ ! -x "$BIN" ]; then
        echo "mistarr binary not found at $BIN"
        return 1
    fi
    rm -f "$PIDFILE"
    (supervise) </dev/null >/dev/null 2>&1 &
    sup=$!
    echo "$sup" > "$SUPERFILE"
    sleep 2
    if ! is_running; then
        kill "$sup" 2>/dev/null
        rm -f "$SUPERFILE"
        echo "mistarr failed to start"
        tail -n 20 "$LOGFILE" 2>/dev/null
        return 1
    fi
    echo "mistarr started (pid $(cat "$PIDFILE"))"
}

# Waits up to $2 seconds for pid $1 to exit, then kills it outright.
reap() {
    i=0
    while kill -0 "$1" 2>/dev/null; do
        i=$((i + 1))
        if [ "$i" -ge "$2" ]; then
            kill -9 "$1" 2>/dev/null
            break
        fi
        sleep 1
    done
}

# Resumes a download client mistarr stopped for a running core, when the
# recorded pid still names that process; see docs/DOWNLOAD-CLIENTS.md.
thaw_client() {
    [ -e "$FROZEN" ] || [ -L "$FROZEN" ] || return 0
    fdir=$(dirname "$FROZEN")
    me=$(id -u)
    # Only a record mistarr wrote: a regular file of this user in its private directory.
    if [ -L "$FROZEN" ] || [ ! -f "$FROZEN" ] || [ -L "$fdir" ] \
        || [ "$(stat -c %u "$FROZEN")" != "$me" ] || [ "$(stat -c %u "$fdir")" != "$me" ] \
        || [ "$(stat -c %a "$fdir")" != 700 ]; then
        echo "ignoring $FROZEN: not a record mistarr wrote" >&2
        return 0
    fi
    if read -r fpid fstart < "$FROZEN" && [ -n "${fpid##*[!0-9]*}" ] \
        && [ -r "$PROCDIR/$fpid/stat" ]; then
        now=$(sed 's/.*) //' "$PROCDIR/$fpid/stat" | cut -d' ' -f20)
        exe=$(readlink "$PROCDIR/$fpid/exe" 2>/dev/null)
        exe=${exe% (deleted)}
        case "${exe##*/}" in
            rtorrent | transmission-daemon) ;;
            *) now="" ;;
        esac
        if [ -n "$fstart" ] && [ "$now" = "$fstart" ] && kill -CONT "$fpid" 2>/dev/null; then
            echo "download client resumed"
        fi
    fi
    rm -f "$FROZEN"
}

do_stop() {
    sup=$(supervisor_pid)
    [ -n "$sup" ] && kill "$sup" 2>/dev/null
    if is_running; then
        pid=$(cat "$PIDFILE")
        kill "$pid" 2>/dev/null
        reap "$pid" 20
        [ "$(cat "$PIDFILE" 2>/dev/null)" = "$pid" ] && rm -f "$PIDFILE"
        stopped=1
    else
        stopped=0
    fi
    [ -n "$sup" ] && reap "$sup" 5
    rm -f "$SUPERFILE"
    # A killed daemon cannot resume the client itself.
    thaw_client
    if [ "$stopped" -eq 1 ] || [ -n "$sup" ]; then
        echo "mistarr stopped"
    else
        echo "mistarr not running"
    fi
}

do_status() {
    if is_running; then
        echo "mistarr running (pid $(cat "$PIDFILE"))"
    elif [ -n "$(supervisor_pid)" ]; then
        echo "mistarr not running; restarting after a crash"
    else
        echo "mistarr not running"
    fi
}

# First non-loopback IPv4, tried with whatever address tool the board has.
first_ipv4() {
    addr=""
    if command -v ip >/dev/null 2>&1; then
        addr=$(ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n1)
    fi
    if [ -z "$addr" ] && command -v ifconfig >/dev/null 2>&1; then
        addr=$(ifconfig 2>/dev/null | awk '/inet /{print $2}' | sed 's/^addr://' \
            | grep -v '^127\.' | head -n1)
    fi
    if [ -z "$addr" ] && command -v hostname >/dev/null 2>&1; then
        addr=$(hostname -I 2>/dev/null | awk '{print $1}')
    fi
    [ -n "$addr" ] && printf '%s\n' "$addr"
}

offer_autostart() {
    if [ -f "$STARTUP" ] && grep -qF "$STARTUP_LINE" "$STARTUP"; then
        return 0
    fi
    printf 'Enable start-at-boot? [y/N] '
    read -r answer
    case "$answer" in
        y | Y | yes | YES)
            [ -f "$STARTUP" ] || : > "$STARTUP"
            echo "$STARTUP_LINE" >> "$STARTUP"
            echo "start-at-boot enabled"
            ;;
        *)
            echo "start-at-boot not enabled"
            ;;
    esac
}

menu_run() {
    if ! do_start; then
        return 1
    fi
    ip=$(first_ipv4)
    if [ -n "$ip" ]; then
        echo "mistarr is running at http://$ip:$PORT/"
    else
        echo "mistarr is running; could not detect an IPv4 address"
    fi
    offer_autostart
}

case "${1:-}" in
    start) do_start ;;
    stop) do_stop ;;
    status) do_status ;;
    restart)
        do_stop
        do_start
        ;;
    "") menu_run ;;
    *)
        echo "usage: $0 {start|stop|status|restart}"
        exit 1
        ;;
esac
