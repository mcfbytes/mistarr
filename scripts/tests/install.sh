#!/bin/sh
# Exercises scripts/install.sh against a fake GitHub API and release server.
set -u

here=$(cd "$(dirname "$0")" && pwd)
install_script="$here/../install.sh"
srv=$(mktemp -d)
work=$(mktemp -d)

fail=0
srv_pid=""

# Stands in for running the fake binary's `listen-addr`: reads [server] listen
# with a real TOML parser, else prints TEST_LISTEN as the default address.
runner="$work/runner"
cat > "$runner" <<'EOS'
#!/bin/sh
data=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --data) data="$2"; shift 2 ;;
        *) shift ;;
    esac
done
exec python3 -c '
import os, sys, tomllib
path, listen = sys.argv[1], sys.argv[2]
if os.path.exists(path):
    with open(path, "rb") as f:
        listen = tomllib.load(f).get("server", {}).get("listen", listen)
print(listen)' "$data/mistarr.toml" "$TEST_LISTEN"
EOS
chmod +x "$runner"

cleanup() {
    [ -n "$srv_pid" ] && kill "$srv_pid" 2>/dev/null
    rm -rf "$srv" "$work"
}
trap cleanup EXIT

# Stand in for the board's BusyBox: tar without gzip support, od without -t/-A.
# The shims enforce those limits themselves over busybox applets or host tools.
shims="$work/bin"
mkdir -p "$shims"
backing=""
for tool in tar od hexdump; do
    if command -v busybox >/dev/null 2>&1 && busybox --list 2>/dev/null | grep -qx "$tool"; then
        real="busybox $tool"
        backing="$backing $tool=busybox"
    elif real=$(command -v "$tool"); then
        real="'$real'"
        backing="$backing $tool=host"
    else
        echo "scripts/tests/install.sh needs $tool from busybox or the host" >&2
        exit 1
    fi
    printf '#!/bin/sh\nrun_real() { %s "$@"; }\n' "$real" > "$shims/$tool"
done
cat >> "$shims/tar" <<'EOS'
# BusyBox tar without gzip: compression options and gzip input fail as on the board.
refuse() { echo "tar: $1" >&2; exit 1; }
extract=0 file="" queue="" first=1
for a in "$@"; do
    if [ -n "$queue" ]; then
        [ "${queue%"${queue#?}"}" = f ] && file=$a
        queue=${queue#?}
        continue
    fi
    cluster=""
    case "$a" in
        --gzip|--gunzip|--ungzip|--bzip2|--xz|--lzma|--zstd|--compress|--uncompress|--auto-compress|--use-compress-program*)
            refuse "unrecognized option '$a'" ;;
        --extract|--get) extract=1 ;;
        --file=*) file=${a#--file=} ;;
        --file) queue=f ;;
        --directory) queue=C ;;
        --*) ;;
        -*) cluster=${a#-} ;;
        *) if [ "$first" = 1 ]; then cluster=$a; fi ;;
    esac
    first=0
    while [ -n "$cluster" ]; do
        c=${cluster%"${cluster#?}"}
        cluster=${cluster#?}
        case "$c" in
            z|j|J|Z|a|I) refuse "invalid option -- '$c'" ;;
            x) extract=1 ;;
            f|C|T|X|b)
                # A dashed cluster's remainder is the value; old-style values follow in order.
                if [ -n "$cluster" ] && [ "${a#-}" != "$a" ]; then
                    [ "$c" = f ] && file=$cluster
                    cluster=""
                else
                    queue="$queue$c"
                fi
                ;;
        esac
    done
done
magic=$(printf '\037\213')
if [ "$extract" = 1 ] && { [ -z "$file" ] || [ "$file" = - ]; }; then
    t=$(mktemp) || exit 1
    cat > "$t"
    if [ "$(head -c 2 "$t")" = "$magic" ]; then rm -f "$t"; refuse "invalid tar magic"; fi
    run_real "$@" < "$t"
    rc=$?
    rm -f "$t"
    exit "$rc"
fi
if [ "$extract" = 1 ] && [ "$(head -c 2 "$file" 2>/dev/null)" = "$magic" ]; then
    refuse "invalid tar magic"
fi
run_real "$@"
EOS
cat >> "$shims/od" <<'EOS'
# BusyBox od without -t/-A: those options fail as on the board.
for a in "$@"; do
    case "$a" in
        --) break ;;
        -t*|-A*|--format*|--address-radix*|-[!-]*[tA]*) echo "od: invalid option -- '$a'" >&2; exit 1 ;;
    esac
done
run_real "$@"
EOS
echo 'run_real "$@"' >> "$shims/hexdump"
chmod +x "$shims/tar" "$shims/od" "$shims/hexdump"
board_path="$shims:$PATH"
echo "install tests: board BusyBox limits enforced over$backing"

port=$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()')

(cd "$srv" && exec python3 -m http.server "$port" --bind 127.0.0.1 >/dev/null 2>&1) &
srv_pid=$!

i=0
while ! curl -fsS -o /dev/null "http://127.0.0.1:$port/" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -ge 50 ] && { echo "fake server never came up" >&2; exit 1; }
    sleep 0.1
done

api="http://127.0.0.1:$port/repos/mcfbytes/mistarr/releases"
dl_base="http://127.0.0.1:$port/download"

expect() {
    got="$1"
    want="$2"
    name="$3"
    if [ "$got" != "$want" ]; then
        fail=$((fail + 1))
        echo "FAIL: $name (want [$want] got [$got])"
    fi
}

expect_contains() {
    got="$1"
    needle="$2"
    name="$3"
    case "$got" in
        *"$needle"*) ;;
        *)
            fail=$((fail + 1))
            echo "FAIL: $name (want [$got] to contain [$needle])"
            ;;
    esac
}

# The shims reject what the board rejects, whichever tools back them.
mkdir -p "$work/shimcheck"
printf 'x' > "$work/shimcheck/member"
tar -C "$work/shimcheck" -czf "$work/shimcheck/a.tar.gz" member
for args in "-xzf a.tar.gz" "xzf a.tar.gz" "-x -z -f a.tar.gz" "--gzip -xf a.tar.gz" "-xf a.tar.gz"; do
    # shellcheck disable=SC2086
    if (cd "$work/shimcheck" && PATH="$board_path" tar $args) >/dev/null 2>&1; then
        fail=$((fail + 1))
        echo "FAIL: the tar stand-in refuses gzip (tar $args)"
    fi
done
if PATH="$board_path" tar -C "$work/shimcheck" -xf - < "$work/shimcheck/a.tar.gz" >/dev/null 2>&1; then
    fail=$((fail + 1))
    echo "FAIL: the tar stand-in refuses gzip on stdin"
fi
if ! gunzip -c "$work/shimcheck/a.tar.gz" | PATH="$board_path" tar -C "$work/shimcheck" -xf - member; then
    fail=$((fail + 1))
    echo "FAIL: the tar stand-in extracts a plain tar from stdin"
fi
for args in "-An" "-tx1" "-vtx1" "--format=x1"; do
    if PATH="$board_path" od "$args" /dev/null >/dev/null 2>&1; then
        fail=$((fail + 1))
        echo "FAIL: the od stand-in refuses $args"
    fi
done
expect "$(printf 'AB' | PATH="$board_path" hexdump -v -e '1/1 "%02x "')" "41 42 " "the hexdump stand-in formats bytes"

# Fake ARM32 ELF header (20 bytes) followed by arbitrary payload.
write_arm_binary() {
    printf '\177ELF\001\001\001\000\000\000\000\000\000\000\000\000\002\000\050\000%s' "$2" > "$1"
}

# A launcher stub standing in for mistarr.sh: supports start/stop/status and
# a bare invocation that prints a URL and asks the start-at-boot question.
write_launcher_stub() {
    cat > "$1" <<'STUB'
#!/bin/sh
# stub-kind: normal
log="$MISTARR_ROOT/launcher.log"
case "${1:-}" in
    stop)
        echo "stop prev=$(find "$MISTARR_ROOT" -name '*.prev*' | wc -l) db=$(cat "$MISTARR_ROOT/mistarr/mistarr.db" 2>/dev/null)" >> "$log"
        echo "mistarr stopped"
        ;;
    start)
        echo "start" >> "$log"
        echo "mistarr started"
        ;;
    status) echo "mistarr not running" ;;
    "")
        echo "start" >> "$log"
        echo "mistarr started"
        echo "mistarr is running at http://127.0.0.1:8420/"
        printf 'Enable start-at-boot? [y/N] '
        read -r answer
        echo "answer=$answer"
        ;;
    *)
        exit 1
        ;;
esac
STUB
    chmod +x "$1"
}

# Publishes a release under $tag at the fake server: the API JSON that
# resolves it, and the tarball plus checksum at the download base.
publish_release() {
    tag="$1"
    binary_marker="$2"
    good_checksum="$3"
    mkdir -p "$srv/repos/mcfbytes/mistarr/releases/tags" "$srv/download/$tag"
    printf '{"tag_name": "%s"}' "$tag" > "$srv/repos/mcfbytes/mistarr/releases/tags/$tag"

    stage=$(mktemp -d)
    write_arm_binary "$stage/mistarr" "$binary_marker"
    write_launcher_stub "$stage/mistarr.sh"
    echo "install stub" > "$stage/install.sh"
    tar -C "$stage" -czf "$srv/download/$tag/mistarr-armv7.tar.gz" mistarr mistarr.sh install.sh
    if [ "$good_checksum" = yes ]; then
        (cd "$srv/download/$tag" && sha256sum mistarr-armv7.tar.gz) \
            > "$srv/download/$tag/mistarr-armv7.tar.gz.sha256"
    else
        echo "0000000000000000000000000000000000000000000000000000000000000  mistarr-armv7.tar.gz" \
            > "$srv/download/$tag/mistarr-armv7.tar.gz.sha256"
    fi
    rm -rf "$stage"
}

# Publishes a release whose binary is not an ELF file at all.
publish_bad_binary_release() {
    tag="$1"
    mkdir -p "$srv/repos/mcfbytes/mistarr/releases/tags" "$srv/download/$tag"
    printf '{"tag_name": "%s"}' "$tag" > "$srv/repos/mcfbytes/mistarr/releases/tags/$tag"

    stage=$(mktemp -d)
    printf '#!/bin/sh\necho not an elf\n' > "$stage/mistarr"
    write_launcher_stub "$stage/mistarr.sh"
    echo "install stub" > "$stage/install.sh"
    tar -C "$stage" -czf "$srv/download/$tag/mistarr-armv7.tar.gz" mistarr mistarr.sh install.sh
    (cd "$srv/download/$tag" && sha256sum mistarr-armv7.tar.gz) \
        > "$srv/download/$tag/mistarr-armv7.tar.gz.sha256"
    rm -rf "$stage"
}

# Publishes a release whose binary is a genuinely empty file.
publish_empty_binary_release() {
    tag="$1"
    mkdir -p "$srv/repos/mcfbytes/mistarr/releases/tags" "$srv/download/$tag"
    printf '{"tag_name": "%s"}' "$tag" > "$srv/repos/mcfbytes/mistarr/releases/tags/$tag"

    stage=$(mktemp -d)
    : > "$stage/mistarr"
    write_launcher_stub "$stage/mistarr.sh"
    echo "install stub" > "$stage/install.sh"
    tar -C "$stage" -czf "$srv/download/$tag/mistarr-armv7.tar.gz" mistarr mistarr.sh install.sh
    (cd "$srv/download/$tag" && sha256sum mistarr-armv7.tar.gz) \
        > "$srv/download/$tag/mistarr-armv7.tar.gz.sha256"
    rm -rf "$stage"
}

# A launcher stub whose bare invocation always fails, simulating mistarr
# failing to start after an upgrade.
write_crashing_launcher_stub() {
    cat > "$1" <<'STUB'
#!/bin/sh
# stub-kind: crashing
log="$MISTARR_ROOT/launcher.log"
case "${1:-}" in
    stop)
        echo "stop prev=$(find "$MISTARR_ROOT" -name '*.prev*' | wc -l) db=$(cat "$MISTARR_ROOT/mistarr/mistarr.db" 2>/dev/null)" >> "$log"
        echo "mistarr stopped"
        ;;
    start) echo "mistarr started" ;;
    status) echo "mistarr not running" ;;
    "")
        # Stands in for a new binary that migrates the database, then dies.
        [ -f "$MISTARR_ROOT/drop-marker" ] && rm -f "$MISTARR_ROOT/mistarr/mistarr.prev.ok"
        db="$MISTARR_ROOT/mistarr/mistarr.db"
        if [ -f "$db" ]; then
            echo "MIGRATED" > "$db"
            echo "NEW-WAL" > "$db-wal"
        fi
        echo "mistarr failed to start"
        exit 1
        ;;
    *)
        exit 1
        ;;
esac
STUB
    chmod +x "$1"
}

# Publishes a release whose binary and checksum are fine but whose launcher
# fails to start, to exercise the rollback path.
publish_crashing_release() {
    tag="$1"
    binary_marker="$2"
    mkdir -p "$srv/repos/mcfbytes/mistarr/releases/tags" "$srv/download/$tag"
    printf '{"tag_name": "%s"}' "$tag" > "$srv/repos/mcfbytes/mistarr/releases/tags/$tag"

    stage=$(mktemp -d)
    write_arm_binary "$stage/mistarr" "$binary_marker"
    write_crashing_launcher_stub "$stage/mistarr.sh"
    echo "install stub" > "$stage/install.sh"
    tar -C "$stage" -czf "$srv/download/$tag/mistarr-armv7.tar.gz" mistarr mistarr.sh install.sh
    (cd "$srv/download/$tag" && sha256sum mistarr-armv7.tar.gz) \
        > "$srv/download/$tag/mistarr-armv7.tar.gz.sha256"
    rm -rf "$stage"
}

# A launcher stub whose daemon, $2 = ok, serves HTTP on port $3 after 3 s; exit, stops
# being reported running after 3 s; hang, never answers; migrate, advances
# mistarr.migrating for 6 s, then serves; stuck, keeps rewriting it with the same line.
write_slow_launcher_stub() {
    cat > "$1" <<STUB
#!/bin/sh
# stub-kind: slow-$2
state="\$MISTARR_ROOT/stub.state"
case "\${1:-}" in
    stop)
        kill "\$(cat "\$MISTARR_ROOT/stub.srv" 2>/dev/null)" 2>/dev/null
        rm -f "\$state" "\$MISTARR_ROOT/stub.srv"
        echo "mistarr stopped"
        ;;
    status)
        if [ -f "\$state" ]; then echo "mistarr running (pid 1)"; else echo "mistarr not running"; fi
        ;;
    start | "")
        echo running > "\$state"
        case "$2" in
            ok) (sleep 3; exec python3 -m http.server "$3" --bind 127.0.0.1) >/dev/null 2>&1 &
                echo \$! > "\$MISTARR_ROOT/stub.srv" ;;
            exit) (sleep 3; rm -f "\$state") >/dev/null 2>&1 & ;;
            migrate) (m="\$MISTARR_ROOT/mistarr/mistarr.migrating"; i=0
                while [ \$i -lt 6 ]; do
                    echo "migrating from 16 to 17: steps \$i wal 0" > "\$m"; i=\$((i + 1)); sleep 1
                done
                rm -f "\$m"; exec python3 -m http.server "$3" --bind 127.0.0.1) >/dev/null 2>&1 &
                echo \$! > "\$MISTARR_ROOT/stub.srv" ;;
            stuck) (while :; do
                    echo "migrating from 16 to 17: steps 7 wal 4152" > "\$MISTARR_ROOT/mistarr/mistarr.migrating"
                    sleep 1
                done) >/dev/null 2>&1 &
                echo \$! > "\$MISTARR_ROOT/stub.srv" ;;
        esac
        echo "mistarr started"
        ;;
esac
STUB
    chmod +x "$1"
}

# Publishes a release whose launcher is write_slow_launcher_stub $2 on port $3.
publish_slow_release() {
    tag="$1"
    mkdir -p "$srv/repos/mcfbytes/mistarr/releases/tags" "$srv/download/$tag"
    printf '{"tag_name": "%s"}' "$tag" > "$srv/repos/mcfbytes/mistarr/releases/tags/$tag"
    stage=$(mktemp -d)
    write_arm_binary "$stage/mistarr" "SLOW-$2"
    write_slow_launcher_stub "$stage/mistarr.sh" "$2" "$3"
    echo "install stub" > "$stage/install.sh"
    tar -C "$stage" -czf "$srv/download/$tag/mistarr-armv7.tar.gz" mistarr mistarr.sh install.sh
    (cd "$srv/download/$tag" && sha256sum mistarr-armv7.tar.gz) \
        > "$srv/download/$tag/mistarr-armv7.tar.gz.sha256"
    rm -rf "$stage"
}

set_latest() {
    printf '{"tag_name": "%s"}' "$1" > "$srv/repos/mcfbytes/mistarr/releases/latest"
}

run_install() {
    root="$1"
    tty="$2"
    shift 2
    env PATH="${extra_path:+$extra_path:}$board_path" MISTARR_TEST_EXEC="$runner" \
        TEST_LISTEN="${test_listen:-0.0.0.0:${health_port:-$port}}" \
        MISTARR_START_TIMEOUT="${start_timeout:-30}" MISTARR_ROOT="$root" \
        MISTARR_PROGRESS_TIMEOUT="${progress_timeout:-300}" \
        MISTARR_RELEASE_API="$api" MISTARR_RELEASE_BASE="$dl_base" \
        MISTARR_FROZEN="${frozen:-$work/no-frozen-client}" \
        MISTARR_TTY="$tty" sh "$install_script" "$@" 2>&1
}

no_tty="$work/no-such-tty"

# Fresh install, no version argument: resolves "latest".
publish_release v1.0.0 FRESH-BINARY-V1 yes
set_latest v1.0.0
root1="$work/root1"
mkdir -p "$root1"
out=$(run_install "$root1" "$no_tty")
code=$?
[ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: fresh install exits 0 (got $code): $out"; }
expect_contains "$out" "installed mistarr v1.0.0" "fresh install reports the installed tag"
expect_contains "$out" "mistarr is running at http://" "fresh install prints the running URL"
grep -q "FRESH-BINARY-V1" "$root1/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: fresh install wrote the new binary"; }
[ -x "$root1/Scripts/mistarr.sh" ] \
    || { fail=$((fail + 1)); echo "FAIL: fresh install placed an executable launcher"; }

# Upgrade preserves the database, config and watched directories.
root2="$work/root2"
mkdir -p "$root2/mistarr/dats" "$root2/Scripts"
echo "DBDATA" > "$root2/mistarr/mistarr.db"
echo "# CONFIG" > "$root2/mistarr/mistarr.toml"
echo "a-dat-file" > "$root2/mistarr/dats/sample.dat"
write_arm_binary "$root2/mistarr/mistarr" OLD-BINARY-V0
write_launcher_stub "$root2/Scripts/mistarr.sh"
publish_release v1.1.0 UPGRADED-BINARY-V1-1 yes
out=$(run_install "$root2" "$no_tty" v1.1.0)
code=$?
[ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: upgrade exits 0 (got $code): $out"; }
expect "$(cat "$root2/mistarr/mistarr.db")" "DBDATA" "upgrade preserves the database"
expect "$(cat "$root2/mistarr/mistarr.toml")" "# CONFIG" "upgrade preserves the config"
expect "$(cat "$root2/mistarr/dats/sample.dat")" "a-dat-file" "upgrade preserves watched directories"
grep -q "UPGRADED-BINARY-V1-1" "$root2/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: upgrade wrote the new binary"; }
grep -q "OLD-BINARY-V0" "$root2/mistarr/mistarr.prev" \
    || { fail=$((fail + 1)); echo "FAIL: a successful upgrade keeps the previous binary as .prev for manual rollback"; }

# Checksum mismatch is refused, leaving the previous binary intact.
root3="$work/root3"
mkdir -p "$root3/mistarr" "$root3/Scripts"
write_arm_binary "$root3/mistarr/mistarr" GOOD-PREVIOUS-BINARY
write_launcher_stub "$root3/Scripts/mistarr.sh"
publish_release v2.0.0 BAD-CHECKSUM-BINARY no
out=$(run_install "$root3" "$no_tty" v2.0.0)
code=$?
[ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: checksum mismatch must exit non-zero"; }
expect_contains "$out" "checksum verification failed" "checksum mismatch is reported"
grep -q "GOOD-PREVIOUS-BINARY" "$root3/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: checksum mismatch must not touch the previous binary"; }

# A non-ARM binary is refused even though its checksum matches.
root4="$work/root4"
mkdir -p "$root4/mistarr" "$root4/Scripts"
write_arm_binary "$root4/mistarr/mistarr" GOOD-PREVIOUS-BINARY-2
write_launcher_stub "$root4/Scripts/mistarr.sh"
publish_bad_binary_release v3.0.0
out=$(run_install "$root4" "$no_tty" v3.0.0)
code=$?
[ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: non-ARM binary must exit non-zero"; }
expect_contains "$out" "not an ARM ELF" "non-ARM binary is reported"
grep -q "GOOD-PREVIOUS-BINARY-2" "$root4/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: non-ARM binary must not touch the previous binary"; }

# An empty binary file is refused rather than accepted by an ARM-ELF check
# that fails open on no input.
root4b="$work/root4b"
mkdir -p "$root4b/mistarr" "$root4b/Scripts"
write_arm_binary "$root4b/mistarr/mistarr" GOOD-PREVIOUS-BINARY-3
write_launcher_stub "$root4b/Scripts/mistarr.sh"
publish_empty_binary_release v3.1.0
out=$(run_install "$root4b" "$no_tty" v3.1.0)
code=$?
[ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: empty binary must exit non-zero"; }
expect_contains "$out" "not an ARM ELF" "empty binary is reported"
grep -q "GOOD-PREVIOUS-BINARY-3" "$root4b/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: empty binary must not touch the previous binary"; }

# An explicit version argument hits /releases/tags/<version>, not "latest".
root5="$work/root5"
mkdir -p "$root5"
publish_release v4.0.0 EXPLICIT-VERSION-BINARY yes
set_latest v1.0.0
out=$(run_install "$root5" "$no_tty" v4.0.0)
code=$?
[ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: explicit version install exits 0: $out"; }
grep -q "EXPLICIT-VERSION-BINARY" "$root5/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: explicit version install used the requested tag, not latest"; }

# Scripts-menu / piped flow: stdin is a pipe unrelated to the prompt, and no
# real terminal is reachable, so the prompt is skipped with a printed note.
root6="$work/root6"
mkdir -p "$root6"
publish_release v5.0.0 PIPED-FLOW-BINARY yes
set_latest v5.0.0
out=$(printf 'unrelated piped bytes\n' | env PATH="$board_path" MISTARR_TEST_EXEC="$runner" TEST_LISTEN="0.0.0.0:$port" MISTARR_ROOT="$root6" MISTARR_RELEASE_API="$api" \
    MISTARR_RELEASE_BASE="$dl_base" MISTARR_TTY="$no_tty" sh "$install_script" 2>&1)
code=$?
[ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: piped no-argument run exits 0: $out"; }
expect_contains "$out" "no terminal available; skipping the start-at-boot prompt" \
    "piped run with no tty skips the prompt with a note"
expect_contains "$out" "mistarr is running at http://" "piped run still starts and reports the URL"

# When a real terminal is reachable it is used for the prompt instead of
# install.sh's own stdin, even when that stdin is a pipe.
root7="$work/root7"
mkdir -p "$root7"
publish_release v5.1.0 PIPED-FLOW-TTY-BINARY yes
set_latest v5.1.0
fake_tty="$work/fake-tty"
echo "y" > "$fake_tty"
out=$(printf 'unrelated piped bytes\n' | env PATH="$board_path" MISTARR_TEST_EXEC="$runner" TEST_LISTEN="0.0.0.0:$port" MISTARR_ROOT="$root7" MISTARR_RELEASE_API="$api" \
    MISTARR_RELEASE_BASE="$dl_base" MISTARR_TTY="$fake_tty" sh "$install_script" 2>&1)
code=$?
[ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: run with a reachable tty exits 0: $out"; }
expect_contains "$out" "answer=y" "the prompt reads from the tty override, not the piped stdin"

# A binary that fails to start rolls both the binary and the launcher back
# to the previous release and restarts it.
root8="$work/root8"
mkdir -p "$root8/mistarr" "$root8/Scripts"
write_arm_binary "$root8/mistarr/mistarr" GOOD-BEFORE-CRASH
write_launcher_stub "$root8/Scripts/mistarr.sh"
publish_crashing_release v6.0.0 CRASHING-NEW-BINARY
out=$(run_install "$root8" "$no_tty" v6.0.0)
code=$?
[ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: a failed start must exit non-zero"; }
expect_contains "$out" "restored the previous binary and launcher after a failed install" \
    "a failed start reports the rollback"
expect_contains "$out" "restarted the previous version of mistarr" \
    "a failed start reports the restart of the previous version"
grep -q "GOOD-BEFORE-CRASH" "$root8/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: a failed start must restore the previous binary"; }
grep -q "stub-kind: normal" "$root8/Scripts/mistarr.sh" \
    || { fail=$((fail + 1)); echo "FAIL: a failed start must restore the previous launcher"; }

expect_absent() {
    if [ -e "$1" ]; then
        fail=$((fail + 1))
        echo "FAIL: $2 ($1 exists)"
    fi
}

# No file of the rollback set is left under a staging name.
expect_no_staging() {
    left=$(find "$1" -name '*.new' -o -name '*.restore')
    [ -z "$left" ] || { fail=$((fail + 1)); echo "FAIL: $2 (left $left)"; }
}

# A fresh install has no database to save.
expect_contains "$(run_install "$work/root1b" "$no_tty" v1.0.0)" "no database yet; nothing to back up" \
    "a fresh install says there is nothing to back up"
expect_absent "$work/root1b/mistarr/mistarr.db.prev" "a fresh install makes no database backup"
expect_absent "$root1/mistarr/mistarr.db.prev" "the first fresh install makes no database backup"
expect "$(cat "$root2/mistarr/mistarr.db.prev")" "DBDATA" "an upgrade saves the database"
expect_absent "$root2/mistarr/mistarr.db.prev-wal" "an upgrade with no wal saves none"

# An upgrade stops mistarr, then saves the database with its wal and shm.
root9="$work/root9"
mkdir -p "$root9/mistarr" "$root9/Scripts"
echo "DB9" > "$root9/mistarr/mistarr.db"
echo "WAL9" > "$root9/mistarr/mistarr.db-wal"
echo "SHM9" > "$root9/mistarr/mistarr.db-shm"
write_arm_binary "$root9/mistarr/mistarr" OLD-BINARY-9
write_launcher_stub "$root9/Scripts/mistarr.sh"
out=$(run_install "$root9" "$no_tty" v1.1.0)
code=$?
[ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: upgrade with wal exits 0 (got $code): $out"; }
expect "$(head -n1 "$root9/launcher.log")" "stop prev=0 db=DB9" "mistarr is stopped before anything is saved"
expect "$(cat "$root9/mistarr/mistarr.db.prev")" "DB9" "the database is saved"
expect "$(cat "$root9/mistarr/mistarr.db.prev-wal")" "WAL9" "the wal is saved"
expect "$(cat "$root9/mistarr/mistarr.db.prev-shm")" "SHM9" "the shm is saved"
expect "$(cat "$root9/mistarr/mistarr.db-wal")" "WAL9" "the live wal is left in place"
expect_no_staging "$root9" "a saved set leaves no staging files"
expect_contains "$out" "saved the database as $root9/mistarr/mistarr.db.prev" "the backup path is printed"
expect_contains "$out" "mistarr answered at http://127.0.0.1:$port/" "the install waits for an answer"
expect_contains "$out" "to roll back by hand" "the manual rollback is printed"

# An upgrade resumes a download client a killed mistarr left stopped.
start_of() { sed 's/.*) //' "/proc/$1/stat" | cut -d' ' -f20; }
state_of() { sed 's/.*) //' "/proc/$1/stat" | cut -d' ' -f1; }
mkdir -m 700 "$work/run9"
cp "$(readlink -f /bin/sh)" "$work/rtorrent"
"$work/rtorrent" -c 'while :; do sleep 1; done' &
stopped=$!
sleep 1
kill -STOP "$stopped"
echo "$stopped $(start_of "$stopped")" > "$work/run9/client.frozen"
frozen="$work/run9/client.frozen" run_install "$root9" "$no_tty" v1.1.0 >/dev/null
expect "$(state_of "$stopped" | sed "s/[^T]/running/")" "running" "the install resumes the stopped client"
expect_absent "$work/run9/client.frozen" "the frozen record is removed"

# A BusyBox without stat formats checks the record through ls -ldn.
mkdir -p "$work/nostat"
printf '#!/bin/sh\necho "stat: unrecognized option" >&2\nexit 1\n' > "$work/nostat/stat"
chmod +x "$work/nostat/stat"
kill -STOP "$stopped"
echo "$stopped $(start_of "$stopped")" > "$work/run9/client.frozen"
extra_path="$work/nostat" frozen="$work/run9/client.frozen" run_install "$root9" "$no_tty" v1.1.0 >/dev/null
expect "$(state_of "$stopped" | sed "s/[^T]/running/")" "running" "without stat formats the install resumes the client"
kill -STOP "$stopped"
echo "$stopped $(start_of "$stopped")" > "$work/run9/client.frozen"
chmod 750 "$work/run9"
extra_path="$work/nostat" frozen="$work/run9/client.frozen" run_install "$root9" "$no_tty" v1.1.0 >/dev/null
expect "$(state_of "$stopped")" "T" "without stat formats an open record directory is refused"
chmod 700 "$work/run9"
rm -f "$work/run9/client.frozen"
rm -rf "$work/nostat"
kill -CONT "$stopped"

# A stop that fails while mistarr still runs leaves its client to it.
sh -c "sleep 100; : $root9/mistarr/mistarr" &
daemon=$!
echo "$daemon" > "$root9/mistarr/mistarr.pid"
kill -STOP "$stopped"
echo "$stopped $(start_of "$stopped")" > "$work/run9/client.frozen"
write_launcher_stub "$root9/Scripts/mistarr.sh"
sed 's/^        echo "mistarr stopped"$/        exit 1/' "$root9/Scripts/mistarr.sh" > "$work/failing-stop"
cat "$work/failing-stop" > "$root9/Scripts/mistarr.sh"
frozen="$work/run9/client.frozen" run_install "$root9" "$no_tty" v1.1.0 >/dev/null
expect "$(state_of "$stopped")" "T" "a running mistarr keeps its client stopped"
expect_present_file() {
    [ -e "$1" ] || { fail=$((fail + 1)); echo "FAIL: $2 ($1 missing)"; }
}
expect_present_file "$work/run9/client.frozen" "the running mistarr keeps its record"
kill "$daemon" 2>/dev/null
rm -f "$root9/mistarr/mistarr.pid" "$work/run9/client.frozen" "$work/failing-stop"
kill -CONT "$stopped"
kill "$stopped" 2>/dev/null
rm -f "$work/rtorrent"

# A later upgrade with no wal drops the stale saved wal and shm, never mixing sets.
rm -f "$root9/mistarr/mistarr.db-wal" "$root9/mistarr/mistarr.db-shm"
echo "DB9B" > "$root9/mistarr/mistarr.db"
run_install "$root9" "$no_tty" v1.1.0 >/dev/null
expect "$(cat "$root9/mistarr/mistarr.db.prev")" "DB9B" "the database is saved again"
expect_absent "$root9/mistarr/mistarr.db.prev-wal" "a stale saved wal is removed"
expect_absent "$root9/mistarr/mistarr.db.prev-shm" "a stale saved shm is removed"

# The port to wait on comes from [server] listen in mistarr.toml.
root9t="$work/root9t"
mkdir -p "$root9t/mistarr"
printf '[jobs]\nlisten = "x:1"\n[server]\nlisten = "0.0.0.0:%s"\n' "$port" > "$root9t/mistarr/mistarr.toml"
out=$(health_port=1 run_install "$root9t" "$no_tty" v1.1.0)
expect_contains "$out" "mistarr answered at http://127.0.0.1:$port/" "the port is read from mistarr.toml"
expect_present() {
    [ -e "$1" ] || { fail=$((fail + 1)); echo "FAIL: $2 ($1 missing)"; }
}
expect_present "$root9/mistarr/mistarr.prev.ok" "a complete rollback set is marked"
for form in single dotted; do
    r="$work/root9-$form"
    mkdir -p "$r/mistarr"
    if [ "$form" = single ]; then
        printf "[ server ]\nlisten = '0.0.0.0:%s'\n" "$port" > "$r/mistarr/mistarr.toml"
    else
        printf 'server.listen = "0.0.0.0:%s"\n' "$port" > "$r/mistarr/mistarr.toml"
    fi
    out=$(health_port=1 start_timeout=5 run_install "$r" "$no_tty" v1.1.0)
    code=$?
    [ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: a $form config installs without a rollback: $out"; }
    expect_contains "$out" "mistarr answered at http://127.0.0.1:$port/" "a $form config names the port"
done

# An address the binary does not report cleanly is never turned into a URL.
out=$(test_listen="listen = '0.0.0.0:9000'" start_timeout=1 run_install "$work/root9-garbled" "$no_tty" v1.1.0)
expect_contains "$out" "answer at http://127.0.0.1:8420/" "an unparsed address falls back to 8420"

# A new binary that migrates the database and fails to start is stopped, then
# gets both the binary and the database set rolled back.
root10="$work/root10"
mkdir -p "$root10/mistarr" "$root10/Scripts"
echo "OLD-SCHEMA" > "$root10/mistarr/mistarr.db"
write_arm_binary "$root10/mistarr/mistarr" GOOD-BEFORE-MIGRATION
write_launcher_stub "$root10/Scripts/mistarr.sh"
out=$(run_install "$root10" "$no_tty" v6.0.0)
code=$?
[ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: a failed migrating start must exit non-zero"; }
expect_contains "$out" "restored the database from" "a failed start reports the database restore"
expect "$(cat "$root10/mistarr/mistarr.db")" "OLD-SCHEMA" "a failed start restores the database"
expect_absent "$root10/mistarr/mistarr.db-wal" "a failed start drops the new version's wal"
expect_no_staging "$root10" "a restore leaves no staging files"
grep -q "GOOD-BEFORE-MIGRATION" "$root10/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: a failed migrating start must restore the previous binary"; }
expect_contains "$out" "restarted the previous version of mistarr" "the previous version is restarted"
expect "$(grep '^stop' "$root10/launcher.log" | tail -n1)" "stop prev=4 db=MIGRATED" \
    "the new version is stopped before the database is restored"
expect "$(tail -n1 "$root10/launcher.log")" "start" "the previous version starts last"

stubs="$work/stubs"
real_cp=$(command -v cp)
real_stat=$(command -v stat)
for kind in badcp badwal badrestore badprevdb enospc; do
    mkdir -p "$stubs/$kind"
    case "$kind" in
        badcp) pattern='*::*.db.prev.new' ;;
        badwal) pattern='*::*.db.prev-wal.new' ;;
        badrestore) pattern='*::*.restore' ;;
        badprevdb) pattern='*.db.prev::*' ;;
        enospc) pattern='*/extract/mistarr::*' ;;
    esac
    cat > "$stubs/$kind/cp" <<EOS
#!/bin/sh
case "\$1::\$2" in $pattern) printf partial > "\$2"; exit 1 ;; esac
exec "$real_cp" "\$@"
EOS
    chmod +x "$stubs/$kind/cp"
done
mkdir -p "$stubs/nospace" "$stubs/busy"
cat > "$stubs/nospace/df" <<'EOS'
#!/bin/sh
echo "Filesystem 1024-blocks Used Available Capacity Mounted on"
echo "/dev/fake 100000 99999 1 100% /"
EOS
cat > "$stubs/nospace/stat" <<EOS
#!/bin/sh
[ "\$1" = -f ] && { echo "1 1024"; exit 0; }
exec "$real_stat" "\$@"
EOS
printf '#!/bin/sh\necho " 4242"\nexit 0\n' > "$stubs/busy/fuser"
chmod +x "$stubs/nospace/df" "$stubs/nospace/stat" "$stubs/busy/fuser"

# With no room to stage the restore, the saved set is copied straight back.
root12="$work/root12"
mkdir -p "$root12/mistarr" "$root12/Scripts"
echo "OLD-SCHEMA" > "$root12/mistarr/mistarr.db"
write_arm_binary "$root12/mistarr/mistarr" GOOD-BEFORE-FAILED-RESTORE
write_launcher_stub "$root12/Scripts/mistarr.sh"
out=$(extra_path="$stubs/badrestore" run_install "$root12" "$no_tty" v6.0.0)
expect_contains "$out" "no room to stage the database restore" "an unstaged restore is reported"
expect "$(cat "$root12/mistarr/mistarr.db")" "OLD-SCHEMA" "an unstaged restore puts the database back"
expect_absent "$root12/mistarr/mistarr.db-wal" "an unstaged restore drops the new wal"
expect "$(cat "$root12/mistarr/mistarr.db.prev")" "OLD-SCHEMA" "an unstaged restore keeps the saved set"
expect_no_staging "$root12" "an unstaged restore leaves no staging files"
expect_contains "$out" "restarted the previous version of mistarr" "an unstaged restore restarts"

# A database restore that fails outright still puts the binary and launcher
# back, keeps the saved set, and starts nothing against the migrated database.
root12b="$work/root12b"
mkdir -p "$root12b/mistarr" "$root12b/Scripts"
echo "OLD-SCHEMA" > "$root12b/mistarr/mistarr.db"
write_arm_binary "$root12b/mistarr/mistarr" GOOD-BEFORE-FAILED-RESTORE
write_launcher_stub "$root12b/Scripts/mistarr.sh"
out=$(extra_path="$stubs/badprevdb" run_install "$root12b" "$no_tty" v6.0.0)
code=$?
[ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: a failed restore must exit non-zero"; }
expect_contains "$out" "restored the previous binary and launcher, but not the database" \
    "a failed restore says what it restored"
expect_contains "$out" "not restarting" "a failed restore does not restart"
grep -q "GOOD-BEFORE-FAILED-RESTORE" "$root12b/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: a failed database restore still restores the binary"; }
grep -q "stub-kind: normal" "$root12b/Scripts/mistarr.sh" \
    || { fail=$((fail + 1)); echo "FAIL: a failed database restore still restores the launcher"; }
expect "$(cat "$root12b/mistarr/mistarr.db.prev")" "OLD-SCHEMA" "a failed restore keeps the saved set"
expect_no_staging "$root12b" "a failed restore leaves no staging files"
case "$(tail -n1 "$root12b/launcher.log")" in
    stop*) ;;
    *) fail=$((fail + 1)); echo "FAIL: nothing starts after a failed restore" ;;
esac

# A new binary that cannot be copied in, as on a full card, is replaced by the
# saved one; the database was never opened, so it is left alone.
root14="$work/root14"
mkdir -p "$root14/mistarr" "$root14/Scripts"
echo "NEVER-OPENED" > "$root14/mistarr/mistarr.db"
echo "LIVE-WAL" > "$root14/mistarr/mistarr.db-wal"
write_arm_binary "$root14/mistarr/mistarr" GOOD-BEFORE-FULL-CARD
write_launcher_stub "$root14/Scripts/mistarr.sh"
out=$(extra_path="$stubs/enospc" run_install "$root14" "$no_tty" v1.1.0)
code=$?
[ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: a failed binary copy must exit non-zero"; }
expect_contains "$out" "failed to install the new binary" "a failed binary copy is reported"
grep -q "GOOD-BEFORE-FULL-CARD" "$root14/mistarr/mistarr" \
    || { fail=$((fail + 1)); echo "FAIL: a failed binary copy restores the previous binary"; }
expect "$(cat "$root14/mistarr/mistarr.db")" "NEVER-OPENED" "a failed binary copy leaves the database"
expect "$(cat "$root14/mistarr/mistarr.db-wal")" "LIVE-WAL" "a failed binary copy leaves the wal"
case "$out" in
    *"restored the database"* | *"by hand"*)
        fail=$((fail + 1)); echo "FAIL: a failed binary copy must not touch or blame the database" ;;
esac
expect_contains "$out" "restarted the previous version of mistarr" "a failed binary copy restarts"

# A rollback set whose completion marker is gone is never restored.
root15="$work/root15"
mkdir -p "$root15/mistarr" "$root15/Scripts"
echo "OLD-SCHEMA" > "$root15/mistarr/mistarr.db"
: > "$root15/drop-marker"
write_arm_binary "$root15/mistarr/mistarr" GOOD-BEFORE-UNMARKED
write_launcher_stub "$root15/Scripts/mistarr.sh"
out=$(run_install "$root15" "$no_tty" v6.0.0)
expect_contains "$out" "the saved rollback set is incomplete; not restoring it" "an unmarked set is refused"
expect "$(cat "$root15/mistarr/mistarr.db")" "MIGRATED" "an unmarked set is not restored"

# A new version that answers after a slow start is kept.
port2=$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()')
publish_slow_release v7.0.0 ok "$port2"
publish_slow_release v7.1.0 exit "$port2"
publish_slow_release v7.2.0 hang "$port2"
for tag in v7.0.0 v7.1.0 v7.2.0; do
    r="$work/root13-$tag"
    mkdir -p "$r/mistarr" "$r/Scripts"
    echo "SLOW-OLD-DB" > "$r/mistarr/mistarr.db"
    write_arm_binary "$r/mistarr/mistarr" "GOOD-BEFORE-$tag"
    write_launcher_stub "$r/Scripts/mistarr.sh"
    out=$(health_port="$port2" start_timeout=8 run_install "$r" "$no_tty" "$tag")
    code=$?
    last=$(tail -n1 "$r/launcher.log" 2>/dev/null)
    MISTARR_ROOT="$r" sh "$r/Scripts/mistarr.sh" stop >/dev/null 2>&1
    if [ "$tag" = v7.0.0 ]; then
        [ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: a slow start that answers succeeds: $out"; }
        grep -q "SLOW-ok" "$r/mistarr/mistarr" \
            || { fail=$((fail + 1)); echo "FAIL: a slow start that answers keeps the new binary"; }
        continue
    fi
    [ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: $tag: a slow failed start exits non-zero"; }
    grep -q "GOOD-BEFORE-$tag" "$r/mistarr/mistarr" \
        || { fail=$((fail + 1)); echo "FAIL: $tag: a slow failed start restores the binary"; }
    expect_contains "$out" "restored the database from" "$tag: a slow failed start restores the database"
    if [ "$tag" = v7.1.0 ]; then
        expect_contains "$out" "mistarr exited before answering" "a daemon that exits while starting is caught"
    else
        expect_contains "$out" "did not answer at http://127.0.0.1:$port2/ within 8 s" "a start that never answers times out"
    fi
    expect "$last" "start" "$tag: the previous version is restarted"
done

# A start that migrates for longer than the start wait is kept while its progress
# file changes; one whose progress file stops changing is rolled back.
publish_slow_release v7.3.0 migrate "$port2"
publish_slow_release v7.4.0 stuck "$port2"
for tag in v7.3.0 v7.4.0; do
    r="$work/root16-$tag"
    mkdir -p "$r/mistarr" "$r/Scripts"
    echo "PRE-MIGRATION-DB" > "$r/mistarr/mistarr.db"
    write_arm_binary "$r/mistarr/mistarr" "GOOD-BEFORE-$tag"
    write_launcher_stub "$r/Scripts/mistarr.sh"
    out=$(health_port="$port2" start_timeout=2 progress_timeout=4 run_install "$r" "$no_tty" "$tag")
    code=$?
    MISTARR_ROOT="$r" sh "$r/Scripts/mistarr.sh" stop >/dev/null 2>&1
    expect_contains "$out" "mistarr is migrating its database, migrating from 16 to 17" \
        "$tag: the migration's progress is printed"
    if [ "$tag" = v7.3.0 ]; then
        [ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: a migration past the start wait succeeds: $out"; }
        grep -q "SLOW-migrate" "$r/mistarr/mistarr" \
            || { fail=$((fail + 1)); echo "FAIL: a migration past the start wait keeps the new binary"; }
        continue
    fi
    [ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: a stalled migration exits non-zero"; }
    expect_contains "$out" "made no progress migrating its database for 4 s" "a stalled migration is named"
    grep -q "GOOD-BEFORE-$tag" "$r/mistarr/mistarr" \
        || { fail=$((fail + 1)); echo "FAIL: a stalled migration restores the binary"; }
    expect "$(cat "$r/mistarr/mistarr.db")" "PRE-MIGRATION-DB" "a stalled migration restores the database"
done

# A backup that cannot complete aborts before the binary or launcher is
# touched and leaves an older rollback set exactly as it was.
for kind in nospace badcp badwal busy; do
    r="$work/root11-$kind"
    mkdir -p "$r/mistarr" "$r/Scripts"
    echo "KEEP-DB" > "$r/mistarr/mistarr.db"
    echo "KEEP-WAL" > "$r/mistarr/mistarr.db-wal"
    echo "OLDER-DB" > "$r/mistarr/mistarr.db.prev"
    echo "OLDER-WAL" > "$r/mistarr/mistarr.db.prev-wal"
    echo "OLDER-BIN" > "$r/mistarr/mistarr.prev"
    write_arm_binary "$r/mistarr/mistarr" "INSTALLED-$kind"
    write_launcher_stub "$r/Scripts/mistarr.sh"
    out=$(extra_path="$stubs/$kind" run_install "$r" "$no_tty" v1.1.0)
    code=$?
    [ "$code" -ne 0 ] || { fail=$((fail + 1)); echo "FAIL: $kind: a failed backup must exit non-zero"; }
    expect_contains "$out" "aborting the install; nothing was changed" "$kind: the abort is reported"
    grep -q "INSTALLED-$kind" "$r/mistarr/mistarr" \
        || { fail=$((fail + 1)); echo "FAIL: $kind: a failed backup must not touch the binary"; }
    grep -q "stub-kind: normal" "$r/Scripts/mistarr.sh" \
        || { fail=$((fail + 1)); echo "FAIL: $kind: a failed backup must not touch the launcher"; }
    expect "$(cat "$r/mistarr/mistarr.db.prev")" "OLDER-DB" "$kind: the older saved database survives"
    expect "$(cat "$r/mistarr/mistarr.db.prev-wal")" "OLDER-WAL" "$kind: the older saved wal survives"
    expect "$(cat "$r/mistarr/mistarr.prev")" "OLDER-BIN" "$kind: the older saved binary survives"
    expect_absent "$r/Scripts/mistarr.sh.prev" "$kind: no launcher is saved"
    expect_no_staging "$r" "$kind: no partial copy is left"
    expect "$(cat "$r/mistarr/mistarr.db")" "KEEP-DB" "$kind: the database is untouched"
    expect "$(cat "$r/mistarr/mistarr.db-wal")" "KEEP-WAL" "$kind: the wal is untouched"
    case "$kind" in
        nospace) expect_contains "$out" "not enough free space" "a full disk is named" ;;
        badwal) expect_contains "$out" "failed to copy $r/mistarr/mistarr.db-wal" "a failed wal copy is named" ;;
        busy) expect_contains "$out" "is still open by process 4242" "a held database is named" ;;
    esac
    expect_contains "$out" "restarted the installed version" \
        "$kind: the installed version is restarted"
done

# A copy a stopped DAT import left beside the database is removed, never saved,
# and a process still writing one holds the install back like the database itself.
root16="$work/root16"
mkdir -p "$root16/mistarr" "$root16/Scripts"
echo "DB16" > "$root16/mistarr/mistarr.db"
echo "PARTIAL" > "$root16/mistarr/mistarr.db.new"
write_arm_binary "$root16/mistarr/mistarr" OLD-BINARY-16
write_launcher_stub "$root16/Scripts/mistarr.sh"
mkdir -p "$stubs/busynew"
cat > "$stubs/busynew/fuser" <<'EOS'
#!/bin/sh
for f in "$@"; do
    case "$f" in *.db.new) echo " 4343"; exit 0 ;; esac
done
exit 1
EOS
chmod +x "$stubs/busynew/fuser"
out=$(extra_path="$stubs/busynew" run_install "$root16" "$no_tty" v1.1.0)
expect_contains "$out" "is still open by process 4343" "a process writing the copy holds the install"
expect "$(cat "$root16/mistarr/mistarr.db.new")" "PARTIAL" "a held copy is left alone"
out=$(run_install "$root16" "$no_tty" v1.1.0)
expect_contains "$out" "removed the unfinished database copy" "a stale copy is reported"
expect_absent "$root16/mistarr/mistarr.db.new" "a stale copy is removed"
expect_absent "$root16/mistarr/mistarr.db.prev.new" "a stale copy is never saved"
expect "$(cat "$root16/mistarr/mistarr.db.prev")" "DB16" "the database itself is saved"

# Swaps cut short at each step; a copy with no database beside it is never removed.
swap_root() {
    r="$work/root17-$1"
    mkdir -p "$r/mistarr" "$r/Scripts"
    write_arm_binary "$r/mistarr/mistarr" "OLD-BINARY-17-$1"
    write_launcher_stub "$r/Scripts/mistarr.sh"
    echo "$r"
}
# A copy with no database beside it aborts the install and keeps the saved set.
for kind in old-new new; do
    r=$(swap_root "$kind")
    if [ "$kind" = old-new ]; then
        echo "OLD17" > "$r/mistarr/mistarr.db.old"
    fi
    echo "NEW17" > "$r/mistarr/mistarr.db.new"
    echo "PREV17" > "$r/mistarr/mistarr.db.prev"
    echo "PREV17-WAL" > "$r/mistarr/mistarr.db.prev-wal"
    : > "$r/mistarr/mistarr.prev.ok"
    out=$(run_install "$r" "$no_tty" v1.1.0 2>&1)
    expect_contains "$out" "interrupted database swap" "$kind: the swap is named"
    expect_contains "$out" "start the installed mistarr once" "$kind: the way out is given"
    expect "$(cat "$r/mistarr/mistarr.db.new")" "NEW17" "$kind: the copy is kept"
    expect_absent "$r/mistarr/mistarr.db" "$kind: no database is made up"
    expect "$(cat "$r/mistarr/mistarr.db.prev")" "PREV17" "$kind: the saved database is kept"
    expect "$(cat "$r/mistarr/mistarr.db.prev-wal")" "PREV17-WAL" "$kind: its WAL is kept"
    expect "$(test -f "$r/mistarr/mistarr.prev.ok" && echo kept)" kept "$kind: the saved set keeps its marker"
    expect "$(grep -c "OLD-BINARY-17-$kind" "$r/mistarr/mistarr")" 1 "$kind: the binary is not replaced"
done

r=$(swap_root old)
echo "OLD17" > "$r/mistarr/mistarr.db.old"
out=$(run_install "$r" "$no_tty" v1.1.0)
expect_contains "$out" "put the database back" "a lone .old is reported"
expect "$(cat "$r/mistarr/mistarr.db")" "OLD17" "a lone .old becomes the database"
expect "$(cat "$r/mistarr/mistarr.db.prev")" "OLD17" "and is saved"

r=$(swap_root db-old)
echo "NEW17" > "$r/mistarr/mistarr.db"
echo "OLD17" > "$r/mistarr/mistarr.db.old"
out=$(run_install "$r" "$no_tty" v1.1.0)
expect_contains "$out" "a finished swap replaced" "a replaced file is reported"
expect_absent "$r/mistarr/mistarr.db.old" "the replaced file is removed"
expect "$(cat "$r/mistarr/mistarr.db.prev")" "NEW17" "the database itself is saved"

if [ "$fail" -eq 0 ]; then
    echo "all tests passed"
else
    echo "$fail test(s) failed"
    exit 1
fi
