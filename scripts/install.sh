#!/bin/sh
# Installs or upgrades mistarr on the board from a GitHub release. POSIX sh
# for BusyBox ash: run from the Scripts menu, over SSH, or piped from curl.

set -u

ROOT="${MISTARR_ROOT:-/media/fat}"
API="${MISTARR_RELEASE_API:-https://api.github.com/repos/mcfbytes/mistarr/releases}"
DOWNLOAD_BASE="${MISTARR_RELEASE_BASE:-https://github.com/mcfbytes/mistarr/releases/download}"
TTY="${MISTARR_TTY:-/dev/tty}"
INSTALL_DIR="$ROOT/mistarr"
SCRIPTS_DIR="$ROOT/Scripts"
BIN="$INSTALL_DIR/mistarr"
PREV="$BIN.prev"
LAUNCHER="$SCRIPTS_DIR/mistarr.sh"
PREV_LAUNCHER="$LAUNCHER.prev"
DB="$INSTALL_DIR/mistarr.db"
DB_PREV="$DB.prev"
# Set once this run has saved the database set, so a restore never uses a stale one.
db_saved=0

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Fetches $1 into $2 with curl, falling back to wget when curl is absent.
http_get() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
        return $?
    fi
    if command -v wget >/dev/null 2>&1; then
        wget -q -O "$2" "$1"
        return $?
    fi
    echo "neither curl nor wget is available" >&2
    return 1
}

# True when $1 is a 32-bit little-endian ARM ELF executable, checked by hand
# since the board has no `file` command.
is_arm_elf() {
    [ -f "$1" ] || return 1
    # od's -t and -A are optional in BusyBox, so fall back to hexdump.
    if command -v od >/dev/null 2>&1 && od -An -tx1 -v /dev/null >/dev/null 2>&1; then
        bytes=$(head -c 20 "$1" 2>/dev/null | od -An -tx1 -v | tr -d '\n')
    elif command -v hexdump >/dev/null 2>&1; then
        bytes=$(head -c 20 "$1" 2>/dev/null | hexdump -v -e '1/1 "%02x "')
    else
        return 1
    fi
    echo "$bytes" | awk '
        BEGIN { ok = 0 }
        {
            n = split($0, b, " ")
            if (n < 20) { next }
            if (b[1] != "7f" || b[2] != "45" || b[3] != "4c" || b[4] != "46") { next }
            if (b[5] != "01") { next }
            if (b[19] != "28" || b[20] != "00") { next }
            ok = 1
        }
        END { exit !ok }'
}

# Size of file $1 in KiB, rounded up; awk keeps large sizes out of shell arithmetic.
size_kib() {
    wc -c < "$1" | awk '{ printf "%d\n", ($1 + 1023) / 1024 }'
}

# Free KiB on the filesystem holding $1, or nothing when df cannot say.
free_kib() {
    df -Pk "$1" 2>/dev/null | awk 'NR == 2 && $4 ~ /^[0-9]+$/ { print $4 }'
}

# Removes the saved database set.
remove_db_prev() {
    rm -f "$DB_PREV" "$DB_PREV-wal" "$DB_PREV-shm"
}

# Copies mistarr.db and any -wal/-shm beside it to the mistarr.db.prev set,
# so a rollback across a migration has the matching database. Streams with cp.
backup_db() {
    if [ ! -f "$DB" ]; then
        echo "no database yet; nothing to back up"
        return 0
    fi
    need=1024
    for f in "$DB" "$DB-wal" "$DB-shm"; do
        [ -f "$f" ] && need=$((need + $(size_kib "$f")))
    done
    avail=$(free_kib "$INSTALL_DIR")
    if [ -n "$avail" ]; then
        # The old set is removed before copying, so its space counts as free.
        for f in "$DB_PREV" "$DB_PREV-wal" "$DB_PREV-shm"; do
            [ -f "$f" ] && avail=$((avail + $(size_kib "$f")))
        done
        if [ "$avail" -lt "$need" ]; then
            echo "not enough free space to back up the database: need $need KiB, have $avail KiB" >&2
            return 1
        fi
    fi
    remove_db_prev
    for part in "" -wal -shm; do
        [ -f "$DB$part" ] || continue
        if ! cp "$DB$part" "$DB_PREV$part"; then
            echo "failed to copy $DB$part to $DB_PREV$part" >&2
            remove_db_prev
            return 1
        fi
    done
    db_saved=1
    echo "saved the database as $DB_PREV"
}

# Puts the saved database set back in place of the current one, dropping any
# -wal/-shm the saved set lacks so a newer WAL is never replayed onto it.
restore_db() {
    [ "$db_saved" = 1 ] || return 0
    rm -f "$DB-wal" "$DB-shm"
    for part in "" -wal -shm; do
        [ -f "$DB_PREV$part" ] || continue
        if ! cp "$DB_PREV$part" "$DB$part"; then
            echo "failed to restore $DB$part from $DB_PREV$part" >&2
            return 1
        fi
    done
    echo "restored the database from $DB_PREV"
}

# Restarts the installed version after an install that stopped it and then
# aborted before changing anything.
restart_current() {
    [ -x "$LAUNCHER" ] || return 0
    if MISTARR_ROOT="$ROOT" "$LAUNCHER" start; then
        echo "restarted the installed version of mistarr"
    else
        echo "failed to restart the installed version of mistarr" >&2
    fi
}

# Restores the previous binary, launcher and database after a failed install,
# if they were saved, and restarts that previous version.
restore_prev() {
    if [ -x "$LAUNCHER" ]; then
        MISTARR_ROOT="$ROOT" "$LAUNCHER" stop >/dev/null 2>&1 || true
    fi
    if ! restore_db; then
        echo "not restarting; copy the $DB_PREV set back by hand before starting mistarr" >&2
        return 1
    fi
    restored=0
    if [ -f "$PREV" ]; then
        cp "$PREV" "$BIN" && chmod +x "$BIN"
        restored=1
    fi
    if [ -f "$PREV_LAUNCHER" ]; then
        cp "$PREV_LAUNCHER" "$LAUNCHER" && chmod +x "$LAUNCHER"
        restored=1
    fi
    [ "$restored" = 1 ] || return 0
    echo "restored the previous binary and launcher after a failed install"
    if [ -x "$LAUNCHER" ] && MISTARR_ROOT="$ROOT" "$LAUNCHER" start; then
        echo "restarted the previous version of mistarr"
    else
        echo "failed to restart the previous version of mistarr" >&2
    fi
}

resolve_release() {
    version="$1"
    json="$tmp/release.json"
    if [ -n "$version" ]; then
        url="$API/tags/$version"
    else
        url="$API/latest"
    fi
    echo "resolving the release from $url"
    if ! http_get "$url" "$json"; then
        echo "could not reach the GitHub API to resolve the release" >&2
        exit 1
    fi
    tag=$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$json" | head -n1)
    if [ -z "$tag" ]; then
        echo "release not found: $url" >&2
        exit 1
    fi
    echo "resolved release $tag"
}

download_and_verify() {
    dl="$tmp/dl"
    mkdir -p "$dl"
    tarball_url="$DOWNLOAD_BASE/$tag/mistarr-armv7.tar.gz"
    echo "downloading $tarball_url"
    if ! http_get "$tarball_url" "$dl/mistarr-armv7.tar.gz"; then
        echo "failed to download the release tarball" >&2
        exit 1
    fi
    if ! http_get "$tarball_url.sha256" "$dl/mistarr-armv7.tar.gz.sha256"; then
        echo "failed to download the checksum file" >&2
        exit 1
    fi
    if ! (cd "$dl" && sha256sum -c mistarr-armv7.tar.gz.sha256) >/dev/null 2>&1; then
        echo "checksum verification failed; refusing to install" >&2
        exit 1
    fi
    echo "checksum verified"
}

extract_and_check() {
    ex="$tmp/extract"
    mkdir -p "$ex"
    # BusyBox tar may lack -z on the board; gunzip is always present.
    if ! gunzip -c "$tmp/dl/mistarr-armv7.tar.gz" | tar -C "$ex" -xf - mistarr mistarr.sh; then
        echo "failed to extract the release tarball" >&2
        exit 1
    fi
    if ! is_arm_elf "$ex/mistarr"; then
        echo "downloaded binary is not an ARM ELF executable; refusing to install" >&2
        exit 1
    fi
    echo "binary verified as an ARM ELF executable"
}

# Runs the launcher with its stdin attached to a real terminal when one is
# reachable, so the start-at-boot prompt works even when install.sh itself
# was read from a pipe. Falls back to /dev/null and a printed note.
run_launcher() {
    if true 2>/dev/null < "$TTY"; then
        MISTARR_ROOT="$ROOT" "$LAUNCHER" < "$TTY"
        return $?
    fi
    echo "no terminal available; skipping the start-at-boot prompt"
    MISTARR_ROOT="$ROOT" "$LAUNCHER" < /dev/null
}

install_release() {
    mkdir -p "$INSTALL_DIR" "$SCRIPTS_DIR"

    if [ -x "$LAUNCHER" ]; then
        echo "stopping the running mistarr before installing"
        MISTARR_ROOT="$ROOT" "$LAUNCHER" stop || true
    fi

    if ! backup_db; then
        echo "aborting the install; nothing was changed" >&2
        restart_current
        exit 1
    fi

    if [ -f "$BIN" ] && ! cp "$BIN" "$PREV"; then
        echo "failed to save the previous binary; aborting the install" >&2
        restart_current
        exit 1
    fi
    if [ -f "$LAUNCHER" ] && ! cp "$LAUNCHER" "$PREV_LAUNCHER"; then
        echo "failed to save the previous launcher; aborting the install" >&2
        restart_current
        exit 1
    fi

    if ! cp "$ex/mistarr" "$BIN"; then
        echo "failed to install the new binary" >&2
        restore_prev
        exit 1
    fi
    chmod +x "$BIN"

    if ! cp "$ex/mistarr.sh" "$LAUNCHER"; then
        echo "failed to install mistarr.sh" >&2
        restore_prev
        exit 1
    fi
    chmod +x "$LAUNCHER"

    echo "installed mistarr $tag to $INSTALL_DIR"
    echo "starting mistarr"
    if ! run_launcher; then
        echo "mistarr failed to start after installing" >&2
        restore_prev
        exit 1
    fi
    print_rollback
}

# Says what was saved and how to go back to it by hand.
print_rollback() {
    [ -f "$PREV" ] || return 0
    echo "the previous version is saved as $PREV"
    if [ "$db_saved" = 1 ]; then
        echo "and its database as $DB_PREV (with any -wal and -shm beside it)"
    fi
    echo "to roll back by hand, in $INSTALL_DIR: run $LAUNCHER stop, copy mistarr.prev to mistarr,"
    if [ "$db_saved" = 1 ]; then
        echo "delete mistarr.db-wal and mistarr.db-shm, copy mistarr.db.prev to mistarr.db and any"
        echo "mistarr.db.prev-wal and mistarr.db.prev-shm to mistarr.db-wal and mistarr.db-shm,"
    fi
    echo "then run $LAUNCHER start; see Rollback in docs/DEPLOYMENT.md"
}

main() {
    version="${1:-}"
    case "$version" in
        "" | v[0-9]*.[0-9]*.[0-9]*) ;;
        *)
            echo "usage: install.sh [vX.Y.Z]" >&2
            exit 1
            ;;
    esac

    resolve_release "$version"
    download_and_verify
    extract_and_check
    install_release
}

main "$@"
