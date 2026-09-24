#!/bin/sh
# Grep gate for principles violations in tracked files.

denylist_file=".github/principles-denylist.txt"
tmpfile=$(mktemp)
trap "rm -f '$tmpfile'" EXIT

git ls-files | while read -r file; do
    # Skip the denylist file itself
    if [ "$file" = "$denylist_file" ]; then
        continue
    fi
    
    # Skip files with "tests" in the path for all checks
    if echo "$file" | grep -q "tests"; then
        continue
    fi
    
    # Check for magnet URIs (skip lines with "test")
    grep -n "magnet:" "$file" 2>/dev/null | grep -vi "test" | sed "s|^|$file:|" >> "$tmpfile" || true
    
    # Check for /announce URLs (skip lines with "test")
    grep -n "/announce" "$file" 2>/dev/null | grep -vi "test" | sed "s|^|$file:|" >> "$tmpfile" || true
    
    # Check for 40-char hex strings (skip in Cargo.lock and skip lines with "test")
    if [ "$file" != "Cargo.lock" ]; then
        grep -n "[0-9a-f]\{40\}" "$file" 2>/dev/null | grep -vi "test" | sed "s|^|$file:|" >> "$tmpfile" || true
    fi
    
    # Check for domains from denylist
    while IFS= read -r domain; do
        # Skip empty lines and comments
        [ -z "$domain" ] && continue
        echo "$domain" | grep -q "^#" && continue
        
        # Skip allowed domains
        case "$domain" in
            tracker.invalid|example.com|example.invalid)
                continue
                ;;
        esac
        
        # Check if domain appears in file (skip lines with "test")
        grep -n "$domain" "$file" 2>/dev/null | grep -vi "test" | sed "s|^|$file:|" >> "$tmpfile" || true
    done < "$denylist_file"
done

# Print violations and exit
if [ -s "$tmpfile" ]; then
    cat "$tmpfile"
    exit 1
fi

exit 0
