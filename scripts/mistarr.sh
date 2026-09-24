#!/bin/sh
# MiSTer Scripts-menu launcher for mistarr. POSIX sh, runs under BusyBox ash.
# start|stop|status|restart; no argument runs the Scripts-menu flow.

ROOT="${MISTARR_ROOT:-/media/fat}"
BIN="$ROOT/mistarr/mistarr"
PIDFILE="$ROOT/mistarr/mistarr.pid"
LOGFILE="$ROOT/mistarr/mistarr.log"
STARTUP="$ROOT/linux/user-startup.sh"
PORT="${MISTARR_PORT:-8420}"
# Resolved absolute path to this script, wherever it was invoked from.
SELF=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
STARTUP_LINE="[ -x $SELF ] && $SELF start &"

# True when $PIDFILE names a live process that is actually running $BIN.
# Removes the pidfile when it is stale (dead pid, or pid reused by another process).
is_running() {
    [ -f "$PIDFILE" ] || return 1
    pid=$(cat "$PIDFILE" 2>/dev/null)
    if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
        rm -f "$PIDFILE"
        return 1
    fi
    if [ -r "/proc/$pid/cmdline" ]; then
        if tr '\0' '\n' < "/proc/$pid/cmdline" | grep -qF "$BIN"; then
            return 0
        fi
        rm -f "$PIDFILE"
        return 1
    fi
    if command -v ps >/dev/null 2>&1; then
        if ps -p "$pid" -o args= 2>/dev/null | grep -qF "$BIN"; then
            return 0
        fi
        rm -f "$PIDFILE"
        return 1
    fi
    # No way to check the command line; trust a pid that answers kill -0.
    return 0
}

do_start() {
    if is_running; then
        echo "mistarr running (pid $(cat "$PIDFILE"))"
        return 0
    fi
    if [ ! -x "$BIN" ]; then
        echo "mistarr binary not found at $BIN"
        return 1
    fi
    # Either applet may be absent from the board's BusyBox; use what is there.
    prio=""
    command -v nice >/dev/null 2>&1 && prio="nice -n 10"
    command -v ionice >/dev/null 2>&1 && prio="$prio ionice -c 3"
    # shellcheck disable=SC2086
    $prio "$BIN" </dev/null >>"$LOGFILE" 2>&1 &
    pid=$!
    sleep 1
    if ! kill -0 "$pid" 2>/dev/null; then
        echo "mistarr failed to start"
        tail -n 20 "$LOGFILE" 2>/dev/null
        rm -f "$PIDFILE"
        return 1
    fi
    echo "$pid" > "$PIDFILE"
    echo "mistarr started (pid $pid)"
}

do_stop() {
    if ! is_running; then
        echo "mistarr not running"
        rm -f "$PIDFILE"
        return 0
    fi
    pid=$(cat "$PIDFILE")
    kill "$pid" 2>/dev/null
    i=0
    while kill -0 "$pid" 2>/dev/null; do
        i=$((i + 1))
        if [ "$i" -ge 20 ]; then
            kill -9 "$pid" 2>/dev/null
            break
        fi
        sleep 1
    done
    rm -f "$PIDFILE"
    echo "mistarr stopped"
}

do_status() {
    if is_running; then
        echo "mistarr running (pid $(cat "$PIDFILE"))"
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
