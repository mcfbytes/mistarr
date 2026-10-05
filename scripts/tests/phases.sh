#!/bin/sh
# Fails when a job phase the server publishes has no text in web/src/lib/status.ts.
cd "$(dirname "$0")/../.." || exit 1
status=0
phases=$(grep -rhoE 'Progress::phase\("[^"]+"\)|\.report\("[^"]+"|"phase": "[^"]+"|\.progress\("[^"]+"\)|[^_a-z]phase\(req, "[^"]+"\)|\("indexing", meter\)' \
    crates/mistarr-server/src --include='*.rs' | sed 's/.*"\([^"]*\)".*/\1/')
labels=$(grep -oE '=> "[a-z ]+",' crates/mistarr-server/src/db/ram.rs | sed 's/.*"\([^"]*\)".*/\1/')
[ -n "$phases" ] || { echo "no phases found"; status=1; }
printf '%s\n%s\n' "$phases" "$labels" | sort -u | while IFS= read -r p; do
    grep -qE "^  ('$p'|$p):" web/src/lib/status.ts || echo "phase without text: $p"
done > "${TMPDIR:-/tmp}/phases.$$"
if [ -s "${TMPDIR:-/tmp}/phases.$$" ]; then
    cat "${TMPDIR:-/tmp}/phases.$$"
    status=1
fi
rm -f "${TMPDIR:-/tmp}/phases.$$"
exit $status
