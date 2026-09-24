#!/bin/sh
# Exercises scripts/mistarr.sh against a fake board root and a stub binary.
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
export MISTARR_ROOT

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

out=$("$script" status)
expect "$out" "mistarr not running" "status before start"

out=$("$script" start)
sleep 1
expect_prefix "$out" "mistarr started" "start reports pid"
[ -f "$root/mistarr/mistarr.pid" ] || {
    fail=$((fail + 1))
    echo "FAIL: pid file created"
}

out=$("$script" status)
expect_prefix "$out" "mistarr running" "status after start"

out=$("$script" start)
expect_prefix "$out" "mistarr running" "start is idempotent"

"$script" stop >/dev/null
out=$("$script" status)
expect "$out" "mistarr not running" "status after stop"
[ -f "$root/mistarr/mistarr.pid" ] && {
    fail=$((fail + 1))
    echo "FAIL: pid file removed after stop"
}

"$script" restart >/dev/null
sleep 1
out=$("$script" status)
expect_prefix "$out" "mistarr running" "restart starts process"
"$script" stop >/dev/null

echo "n" | "$script" >/tmp/mistarr-test-menu.$$ 2>&1
menu_out=$(cat /tmp/mistarr-test-menu.$$)
rm -f /tmp/mistarr-test-menu.$$
case "$menu_out" in
    *"mistarr started"*) ;;
    *)
        fail=$((fail + 1))
        echo "FAIL: menu run starts mistarr"
        ;;
esac
case "$menu_out" in
    *"start-at-boot not enabled"*) ;;
    *)
        fail=$((fail + 1))
        echo "FAIL: menu run declines autostart on n"
        ;;
esac
[ -f "$root/linux/user-startup.sh" ] && {
    fail=$((fail + 1))
    echo "FAIL: user-startup.sh created on decline"
}
"$script" stop >/dev/null

echo "y" | "$script" >/dev/null 2>&1
count1=$(grep -c "mistarr.sh start" "$root/linux/user-startup.sh")
[ "$count1" -eq 1 ] || {
    fail=$((fail + 1))
    echo "FAIL: user-startup.sh gets the start-at-boot line"
}
"$script" stop >/dev/null

echo "y" | "$script" >/dev/null 2>&1
count2=$(grep -c "mistarr.sh start" "$root/linux/user-startup.sh")
[ "$count2" -eq 1 ] || {
    fail=$((fail + 1))
    echo "FAIL: start-at-boot line is idempotent (count $count2)"
}
"$script" stop >/dev/null

if [ "$fail" -eq 0 ]; then
    echo "all tests passed"
else
    echo "$fail test(s) failed"
    exit 1
fi
