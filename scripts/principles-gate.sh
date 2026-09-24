#!/bin/sh
# Fails when tracked files carry pointers to content (docs/PRINCIPLES.md §1).
# Code may parse magnets and torrents, so only live-looking values are flagged.

denylist=".github/principles-denylist.txt"
out=$(mktemp)
trap 'rm -f "$out"' EXIT

hex40='[0-9a-f]\{40\}'
# Placeholders used by tests and docs; everything else is a real pointer.
allow='tracker\.invalid\|example\.\(com\|invalid\)\|0000000000000000000000000000000000000000\|a94a8fe5ccb19ba61c4c0873d391e987982fbbd3\|da39a3ee5e6b4b0d3255bfef95601890afd80709\|34aa973cd4c4daa4f61eeb2bdbad27316534016f'

git ls-files | grep -v "^$denylist$\|^Cargo.lock$\|package-lock.json$" | while read -r f; do
    # A magnet with a hash, or an announce URL, that is not a placeholder.
    grep -n "magnet:?[^ ]*btih:$hex40\|https\?://[^ \"')]*/announce" "$f" 2>/dev/null \
        | grep -v "$allow" | sed "s|^|$f:|" >> "$out"
    # Bare 40-hex outside Rust and TypeScript source, where test vectors live.
    case "$f" in *.rs|*.ts|*.svelte) ;; *)
        grep -n "$hex40" "$f" 2>/dev/null | grep -v "$allow" | sed "s|^|$f:|" >> "$out" ;;
    esac
    grep -v '^#\|^$' "$denylist" | while read -r d; do
        grep -ni "$d" "$f" 2>/dev/null | sed "s|^|$f:|" >> "$out"
    done
done

if [ -s "$out" ]; then
    echo "principles gate: pointers to content found"
    cat "$out"
    exit 1
fi
echo "principles gate: clean"
