#!/bin/sh
# Checks that every repository path and code symbol named in docs/ exists.
set -u

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/../.." || exit 1

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

grep -rhIoE '[A-Za-z_][A-Za-z0-9_]*' crates web/src scripts \
    --exclude-dir=node_modules --exclude-dir=target | sort -u > "$work/words"

# Names of other projects, the kernel and sshd that the docs mention in code font.
external='dlopen SvelteMap prctl _exit umount2 capable statat CapEff NoNewPrivs PasswordAuthentication'
# Build output and files the docs say are not shipped.
absent='web/dist docs/SANDBOX.md'

for doc in docs/*.md; do
    [ "$doc" = docs/CONSOLIDATION.md ] && continue
    grep -noE '`[^`]+`' "$doc" | while IFS=: read -r line tok; do
        tok=${tok#\`}
        tok=${tok%\`}
        case "$tok" in
            crates/* | docs/* | scripts/* | web/* | migrations/* | .github/*)
                case "$tok" in *[!A-Za-z0-9_./-]*) continue ;; esac
                case " $absent " in *" $tok "*) continue ;; esac
                [ -e "${tok%/}" ] || echo "$doc:$line: missing path $tok"
                continue
                ;;
        esac
        name=${tok%%(*}
        case "$name" in
            *[!A-Za-z0-9_:]*) continue ;;
        esac
        case "$tok" in
            *::*) ;;
            *'('*) ;;
            [A-Z][a-z0-9]*[A-Z]*) ;;
            *) continue ;;
        esac
        last=${name##*:}
        [ -n "$last" ] || continue
        case " $external " in *" $last "*) continue ;; esac
        grep -qxF "$last" "$work/words" || echo "$doc:$line: missing symbol $tok"
    done
done > "$work/out"

if [ -s "$work/out" ]; then
    cat "$work/out"
    echo "$(wc -l < "$work/out") doc reference(s) not found"
    exit 1
fi
echo "docs references ok"
