#!/bin/sh
# Exercises scripts/install.sh against a fake GitHub API and release server.
set -u

here=$(cd "$(dirname "$0")" && pwd)
install_script="$here/../install.sh"
srv=$(mktemp -d)
work=$(mktemp -d)

fail=0
srv_pid=""

# Stand in for the board's BusyBox: tar without gzip support, od without -t/-A.
shims="$work/bin"
mkdir -p "$shims"
if command -v busybox >/dev/null 2>&1; then
    cat > "$shims/tar" <<'EOS'
#!/bin/sh
case " $* " in *" -xzf "*|*" -z"*|*"z"*" -f "*) echo "tar: invalid option -- 'z'" >&2; exit 1;; esac
exec busybox tar "$@"
EOS
    printf '#!/bin/sh\nexit 1\n' > "$shims/od"
    printf '#!/bin/sh\nexec busybox hexdump "$@"\n' > "$shims/hexdump"
    chmod +x "$shims/tar" "$shims/od" "$shims/hexdump"
    PATH="$shims:$PATH"
    export PATH
fi

cleanup() {
    [ -n "$srv_pid" ] && kill "$srv_pid" 2>/dev/null
    rm -rf "$srv" "$work"
}
trap cleanup EXIT

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
case "${1:-}" in
    stop) echo "mistarr stopped" ;;
    start) echo "mistarr started" ;;
    status) echo "mistarr not running" ;;
    "")
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
case "${1:-}" in
    stop) echo "mistarr stopped" ;;
    start) echo "mistarr started" ;;
    status) echo "mistarr not running" ;;
    "")
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

set_latest() {
    printf '{"tag_name": "%s"}' "$1" > "$srv/repos/mcfbytes/mistarr/releases/latest"
}

run_install() {
    root="$1"
    tty="$2"
    shift 2
    env MISTARR_ROOT="$root" MISTARR_RELEASE_API="$api" MISTARR_RELEASE_BASE="$dl_base" \
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
echo "CONFIG" > "$root2/mistarr/mistarr.toml"
echo "a-dat-file" > "$root2/mistarr/dats/sample.dat"
write_arm_binary "$root2/mistarr/mistarr" OLD-BINARY-V0
write_launcher_stub "$root2/Scripts/mistarr.sh"
publish_release v1.1.0 UPGRADED-BINARY-V1-1 yes
out=$(run_install "$root2" "$no_tty" v1.1.0)
code=$?
[ "$code" -eq 0 ] || { fail=$((fail + 1)); echo "FAIL: upgrade exits 0 (got $code): $out"; }
expect "$(cat "$root2/mistarr/mistarr.db")" "DBDATA" "upgrade preserves the database"
expect "$(cat "$root2/mistarr/mistarr.toml")" "CONFIG" "upgrade preserves the config"
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
out=$(printf 'unrelated piped bytes\n' | env MISTARR_ROOT="$root6" MISTARR_RELEASE_API="$api" \
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
out=$(printf 'unrelated piped bytes\n' | env MISTARR_ROOT="$root7" MISTARR_RELEASE_API="$api" \
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

if [ "$fail" -eq 0 ]; then
    echo "all tests passed"
else
    echo "$fail test(s) failed"
    exit 1
fi
