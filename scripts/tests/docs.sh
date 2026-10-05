#!/bin/sh
# Checks that every repository path and code symbol named in docs/ exists.
set -u

here=$(cd "$(dirname "$0")" && pwd)
cd "$here/../.." || exit 1

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

grep -rhIoE '[A-Za-z_][A-Za-z0-9_]*' crates web/src web/e2e web/scripts scripts \
    --exclude-dir=node_modules --exclude-dir=target --exclude=docs.sh | sort -u > "$work/words"

# Names of other projects, the kernel, sshd, wire values and plan-only names the docs
# mention in code font.
external='no_new_privs pivot_root oom_score_adj uevent_helper core_pattern exfat_symlink test_perm open_local S99user Presets Satellaview bad_infohash launch_game launch_mra stop_client quiesce_client board_checks decisions_for_user dlopen SvelteMap prctl _exit umount2 capable statat CapEff NoNewPrivs PasswordAuthentication'
# Build output and files the docs say are not shipped.
absent='web/dist docs/SANDBOX.md'

for doc in docs/*.md; do
    [ "$doc" = docs/CONSOLIDATION.md ] && continue
    # shellcheck disable=SC2016 # the backticks are literal
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
        case "$tok" in
            MISTARR_[A-Z0-9_]*)
                case "$tok" in *[!A-Z0-9_]*) continue ;; esac
                grep -qxF "$tok" "$work/words" || echo "$doc:$line: missing symbol $tok"
                continue
                ;;
        esac
        name=${tok%%(*}
        case "$name" in
            *[!A-Za-z0-9_:]*) continue ;;
        esac
        case "$name" in
            *[a-z]*) ;;
            *) continue ;;
        esac
        case "$tok" in
            *::*) ;;
            *'('*) ;;
            [A-Z]*[a-z]*) ;;
            *_*) ;;
            *) continue ;;
        esac
        last=${name##*:}
        [ -n "$last" ] || continue
        case " $external " in *" $last "*) continue ;; esac
        for seg in $(echo "$name" | tr ':' ' '); do
            case " $external " in *" $seg "*) continue ;; esac
            grep -qxF "$seg" "$work/words" || echo "$doc:$line: missing symbol $tok"
        done
    done
    # Fenced rust and ts blocks: CamelCase names and names after fn.
    awk -v doc="$doc" '
        /^```/ { if (infence) infence = 0; else infence = ($0 ~ /^```(rust|ts)/); next }
        infence {
            line = $0
            while (match(line, /(fn +[a-z_][A-Za-z0-9_]*|[A-Z][a-z0-9]+[A-Z][A-Za-z0-9]*)/)) {
                tok = substr(line, RSTART, RLENGTH)
                sub(/^fn +/, "", tok)
                print doc ":" FNR ": " tok
                line = substr(line, RSTART + RLENGTH)
            }
        }' "$doc" | while IFS=' ' read -r where tok; do
        case " $external " in *" $tok "*) continue ;; esac
        grep -qxF "$tok" "$work/words" || echo "$where missing symbol $tok"
    done
done > "$work/out"

if [ -s "$work/out" ]; then
    cat "$work/out"
    echo "$(wc -l < "$work/out") doc reference(s) not found"
    exit 1
fi
echo "docs references ok"
