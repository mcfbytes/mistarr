#!/bin/sh
# MiSTer Scripts-menu launcher for mistarr. POSIX sh, runs under BusyBox ash.
# start|stop|status|restart; no argument runs the Scripts-menu flow.

ROOT="${MISTARR_ROOT:-/media/fat}"
BIN="$ROOT/mistarr/mistarr"
PIDFILE="$ROOT/mistarr/mistarr.pid"
STARTUP="$ROOT/linux/user-startup.sh"
PORT="${MISTARR_PORT:-8420}"
STARTUP_LINE="[ -x $ROOT/Scripts/mistarr.sh ] && $ROOT/Scripts/mistarr.sh start &"

is_running() {
    [ -f "$PIDFILE" ] || return 1
    pid=$(cat "$PIDFILE" 2>/dev/null)
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
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
    nice -n 10 ionice -c 3 "$BIN" </dev/null >/dev/null 2>&1 &
    pid=$!
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
    if command -v ip >/dev/null 2>&1; then
        ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -n1
        return
    fi
    if command -v ifconfig >/dev/null 2>&1; then
        ifconfig 2>/dev/null | awk '/inet /{print $2}' | sed 's/^addr://' \
            | grep -v '^127\.' | head -n1
        return
    fi
    if command -v hostname >/dev/null 2>&1; then
        hostname -I 2>/dev/null | awk '{print $1}'
    fi
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
    do_start
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
