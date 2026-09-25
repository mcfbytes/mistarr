#!/bin/sh
# Exercises scripts/mistarr.sh against a fake board root and stub binaries.
set -u

here=$(cd "$(dirname "$0")" && pwd)
script="$here/../mistarr.sh"
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

mkdir -p "$root/mistarr" "$root/linux" "$root/Scripts"

cat > "$root/mistarr/mistarr" <<'STUB'
#!/bin/sh
trap 'exit 0' TERM
while :; do sleep 1; done
STUB
chmod +x "$root/mistarr/mistarr"

MISTARR_ROOT="$root"
MISTARR_RUNDIR="$root/run"
export MISTARR_ROOT MISTARR_RUNDIR

fail=0

expect() {
    got="$1"
    want="$2"
    name="$3"
    if [ "$got" != "$want" ]; then
        fail=$((fail + 1))
        echo "FAIL: $name (want [$want] got [$got])"
    fi
}

expect_prefix() {
    got="$1"
    prefix="$2"
    name="$3"
    case "$got" in
        "$prefix"*) ;;
        *)
            fail=$((fail + 1))
            echo "FAIL: $name (want prefix [$prefix] got [$got])"
            ;;
    esac
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

expect_file_absent() {
    path="$1"
    name="$2"
    if [ -f "$path" ]; then
        fail=$((fail + 1))
        echo "FAIL: $name ($path exists)"
    fi
}

expect_file_present() {
    path="$1"
    name="$2"
    if [ ! -f "$path" ]; then
        fail=$((fail + 1))
        echo "FAIL: $name ($path missing)"
    fi
}

# Basic lifecycle.
out=$("$script" status)
expect "$out" "mistarr not running" "status before start"

out=$("$script" start)
sleep 1
expect_prefix "$out" "mistarr started" "start reports pid"
expect_file_present "$root/mistarr/mistarr.pid" "pid file created"

out=$("$script" status)
expect_prefix "$out" "mistarr running" "status after start"

out=$("$script" start)
expect_prefix "$out" "mistarr running" "start is idempotent"

"$script" stop >/dev/null
out=$("$script" status)
expect "$out" "mistarr not running" "status after stop"
expect_file_absent "$root/mistarr/mistarr.pid" "pid file removed after stop"

"$script" restart >/dev/null
sleep 1
out=$("$script" status)
expect_prefix "$out" "mistarr running" "restart starts process"
"$script" stop >/dev/null

# Finding 3: a stale pidfile naming a process that is not mistarr is not trusted.
sleep 100 &
foreign_pid=$!
echo "$foreign_pid" > "$root/mistarr/mistarr.pid"
out=$("$script" status)
expect "$out" "mistarr not running" "foreign pid in pidfile is not trusted"
expect_file_absent "$root/mistarr/mistarr.pid" "stale foreign pidfile is removed"
kill "$foreign_pid" 2>/dev/null

# Finding 2: do_start confirms the child survived and reports the log on failure.
cp "$root/mistarr/mistarr" "$root/mistarr/mistarr.good"
cat > "$root/mistarr/mistarr" <<'CRASH'
#!/bin/sh
echo "boom" >&2
exit 1
CRASH
chmod +x "$root/mistarr/mistarr"
out=$("$script" start 2>&1)
code=$?
[ "$code" -ne 0 ] || {
    fail=$((fail + 1))
    echo "FAIL: start exits non-zero when the daemon dies immediately"
}
expect_contains "$out" "mistarr failed to start" "start reports failure on crash"
expect_contains "$out" "boom" "start prints the log tail on crash"
expect_file_absent "$root/mistarr/mistarr.pid" "no pidfile left after a crashed start"
cp "$root/mistarr/mistarr.good" "$root/mistarr/mistarr"
chmod +x "$root/mistarr/mistarr"
rm -f "$root/mistarr/mistarr.log"

# Finding 1: a missing binary fails do_start, and menu_run propagates that
# instead of reporting the daemon as running.
mv "$root/mistarr/mistarr" "$root/mistarr/mistarr.hidden"
out=$(echo n | "$script" 2>&1)
code=$?
[ "$code" -ne 0 ] || {
    fail=$((fail + 1))
    echo "FAIL: menu run exits non-zero when the binary is missing"
}
expect_contains "$out" "mistarr binary not found" "menu run reports the missing binary"
case "$out" in
    *"is running at"* | *"could not detect an IPv4"*)
        fail=$((fail + 1))
        echo "FAIL: menu run must not claim mistarr is running when start failed"
        ;;
    *) ;;
esac
mv "$root/mistarr/mistarr.hidden" "$root/mistarr/mistarr"

# Finding 4: the start-at-boot line points at this script's own resolved
# path, not a hardcoded Scripts-menu location.
mkdir -p "$root/elsewhere"
cp "$script" "$root/elsewhere/mistarr.sh"
chmod +x "$root/elsewhere/mistarr.sh"
echo "y" | MISTARR_ROOT="$root" "$root/elsewhere/mistarr.sh" >/dev/null 2>&1
expect_contains "$(cat "$root/linux/user-startup.sh")" "$root/elsewhere/mistarr.sh" \
    "start-at-boot line uses the invoked script's own path"
MISTARR_ROOT="$root" "$root/elsewhere/mistarr.sh" stop >/dev/null
rm -f "$root/linux/user-startup.sh"

# Finding 5: an `ip` binary that runs but prints nothing falls through to
# ifconfig, then hostname -I, rather than reporting no address.
fakebin="$root/fakebin"
mkdir -p "$fakebin"
cat > "$fakebin/ip" <<'FAKEIP'
#!/bin/sh
exit 0
FAKEIP
chmod +x "$fakebin/ip"
out=$(echo n | PATH="$fakebin:$PATH" MISTARR_ROOT="$root" "$script" 2>&1)
expect_contains "$out" "is running at http://" "empty ip output falls through to another source"
"$script" stop >/dev/null

# No-argument menu run against a working binary: starts it, reports the
# URL, and declining start-at-boot leaves user-startup.sh untouched.
out=$(echo n | "$script")
expect_contains "$out" "mistarr started" "menu run starts mistarr"
expect_contains "$out" "start-at-boot not enabled" "menu run declines autostart on n"
expect_file_absent "$root/linux/user-startup.sh" "user-startup.sh untouched on decline"
"$script" stop >/dev/null

resolved_self=$(cd "$(dirname "$script")" && pwd)/$(basename "$script")

echo "y" | "$script" >/dev/null 2>&1
count1=$(grep -c "$resolved_self" "$root/linux/user-startup.sh" 2>/dev/null || true)
[ "$count1" -eq 1 ] || {
    fail=$((fail + 1))
    echo "FAIL: user-startup.sh gets the start-at-boot line (count $count1)"
}
"$script" stop >/dev/null

echo "y" | "$script" >/dev/null 2>&1
count2=$(grep -c "$resolved_self" "$root/linux/user-startup.sh")
[ "$count2" -eq 1 ] || {
    fail=$((fail + 1))
    echo "FAIL: start-at-boot line is idempotent (count $count2)"
}
"$script" stop >/dev/null

# Racing starts, as user-startup.sh and a manual start at boot: one daemon.
cp "$root/mistarr/mistarr" "$root/mistarr/mistarr.good"
cat > "$root/mistarr/mistarr" <<STUB
#!/bin/sh
echo \$\$ >> "$root/mistarr/launches"
trap 'exit 0' TERM
while :; do sleep 1; done
STUB
chmod +x "$root/mistarr/mistarr"
"$script" start >/dev/null &
"$script" start >/dev/null &
"$script" start >/dev/null
wait
sleep 1
launches=$(wc -l < "$root/mistarr/launches")
expect "$launches" "1" "racing starts launch one daemon"
out=$("$script" status)
expect_prefix "$out" "mistarr running" "the racing start left a live pidfile"
"$script" stop >/dev/null

# A start lock left by a start that died does not block the next one.
lock="$root/run/mistarr.start.lock"
sleep 100 &
dead=$!
kill "$dead" 2>/dev/null
wait "$dead" 2>/dev/null
echo "$dead" > "$lock"
out=$("$script" start)
expect_prefix "$out" "mistarr started" "a stale start lock is taken over"
"$script" stop >/dev/null
[ -e "$lock" ] && {
    fail=$((fail + 1))
    echo "FAIL: the start lock is released"
}

# A lock naming a live process that is not a start of this script is stale too.
sleep 100 &
other=$!
echo "$other" > "$lock"
out=$("$script" start)
expect_prefix "$out" "mistarr started" "a lock held by another program is taken over"
"$script" stop >/dev/null
kill "$other" 2>/dev/null
cp "$root/mistarr/mistarr.good" "$root/mistarr/mistarr"
chmod +x "$root/mistarr/mistarr"

# The supervisor restarts a daemon that crashes after starting.
rm -f "$root/mistarr/launches" "$root/mistarr/mistarr.log"
cat > "$root/mistarr/mistarr" <<STUB
#!/bin/sh
echo \$\$ >> "$root/mistarr/launches"
trap 'exit 0' TERM
if [ "\$(wc -l < "$root/mistarr/launches")" -eq 1 ]; then
    sleep 3
    exit 7
fi
while :; do sleep 1; done
STUB
chmod +x "$root/mistarr/mistarr"
out=$(MISTARR_BACKOFF=1 "$script" start)
expect_prefix "$out" "mistarr started" "a supervised start reports the daemon"
sleep 5
expect "$(wc -l < "$root/mistarr/launches")" "2" "a crashed daemon is started again"
expect_contains "$(cat "$root/mistarr/mistarr.log")" "status 7; restarting in 1 s" "the restart is logged"
out=$("$script" status)
expect_prefix "$out" "mistarr running" "the restarted daemon is running"
"$script" stop >/dev/null
expect_file_absent "$root/mistarr/supervisor.pid" "stop ends the supervisor"
sleep 2
expect "$(wc -l < "$root/mistarr/launches")" "2" "nothing restarts after stop"

# Repeated crashes make the supervisor give up.
rm -f "$root/mistarr/launches" "$root/mistarr/mistarr.log"
cat > "$root/mistarr/mistarr" <<STUB
#!/bin/sh
echo \$\$ >> "$root/mistarr/launches"
sleep 3
exit 9
STUB
chmod +x "$root/mistarr/mistarr"
out=$(MISTARR_BACKOFF=1 MISTARR_CRASH_LIMIT=2 "$script" start)
expect_prefix "$out" "mistarr started" "a daemon that crashes later still starts"
sleep 9
expect_contains "$(cat "$root/mistarr/mistarr.log")" "giving up" "the supervisor gives up"
expect_file_absent "$root/mistarr/supervisor.pid" "a supervisor that gave up is gone"
out=$("$script" status)
expect "$out" "mistarr not running" "nothing runs after giving up"
cp "$root/mistarr/mistarr.good" "$root/mistarr/mistarr"
chmod +x "$root/mistarr/mistarr"

# The daemon starts under nice but at the default I/O class.
cat > "$fakebin/nice" <<FAKENICE
#!/bin/sh
echo "nice \$*" >> "$root/prio"
shift 2
exec "\$@"
FAKENICE
cat > "$fakebin/ionice" <<FAKEIONICE
#!/bin/sh
echo "ionice \$*" >> "$root/prio"
FAKEIONICE
chmod +x "$fakebin/nice" "$fakebin/ionice"
rm -f "$root/prio"
PATH="$fakebin:$PATH" "$script" start >/dev/null
sleep 1
expect "$(cat "$root/prio" 2>/dev/null)" "nice -n 10 $root/mistarr/mistarr" \
    "the daemon runs under nice -n 10 and no ionice"
"$script" stop >/dev/null
rm -f "$fakebin/nice" "$fakebin/ionice"

# stop resumes a client a daemon left stopped, only while its pid is the same process.
start_of() { sed 's/.*) //' "/proc/$1/stat" | cut -d' ' -f20; }
state_of() { sed 's/.*) //' "/proc/$1/stat" | cut -d' ' -f1; }
MISTARR_FROZEN="$root/client.frozen"
export MISTARR_FROZEN
sleep 100 &
client=$!
kill -STOP "$client"
echo "$client $(start_of "$client")" > "$MISTARR_FROZEN"
out=$("$script" stop)
expect_contains "$out" "download client resumed" "stop reports the resumed client"
expect "$(state_of "$client" | sed "s/[^T]/running/")" "running" "stop resumes the stopped client"
expect_file_absent "$MISTARR_FROZEN" "stop removes the frozen record"
kill -STOP "$client"
echo "$client 1" > "$MISTARR_FROZEN"
"$script" stop >/dev/null
expect "$(state_of "$client")" "T" "a record for an older process with that pid resumes nothing"
expect_file_absent "$MISTARR_FROZEN" "a stale frozen record is removed"
kill -CONT "$client"
kill "$client" 2>/dev/null
unset MISTARR_FROZEN

if ! sh "$here/install.sh"; then
    fail=$((fail + 1))
    echo "FAIL: scripts/tests/install.sh"
fi

if [ "$fail" -eq 0 ]; then
    echo "all tests passed"
else
    echo "$fail test(s) failed"
    exit 1
fi
