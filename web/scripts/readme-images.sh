#!/bin/sh
# Regenerates docs/images from mock data; see docs/TESTING.md "README images".
set -eu

web=$(cd "$(dirname "$0")/.." && pwd)
images="$web/../docs/images"
raw=$(mktemp -d)
trap 'rm -rf "$raw"' EXIT

command -v magick >/dev/null || { echo "readme-images: needs ImageMagick 7 with WebP"; exit 1; }
cd "$web"
README_SHOTS_DIR="$raw" npx playwright test -c e2e/playwright.config.ts readme-shots
mkdir -p "$images"

# Art-heavy images are lossy WebP; interface text stays lossless.
for name in hero-dark hero-light platforms-dark platforms-light browse-dark browse-light phone-dark phone-light; do
    magick "$raw/$name.png" -quality 90 -define webp:method=6 "$images/$name.webp"
done
for name in title-dark title-light activity-dark activity-light system-dark system-light wizard-dark wizard-light; do
    magick "$raw/$name.png" -define webp:lossless=true -define webp:method=6 "$images/$name.webp"
done

cp "$raw/social-preview.png" "$images/social-preview.png"
if command -v oxipng >/dev/null; then
    oxipng -o 4 --strip safe -q "$images/social-preview.png"
elif command -v optipng >/dev/null; then
    optipng -quiet -o5 -strip all "$images/social-preview.png"
fi
ls -l "$images"
