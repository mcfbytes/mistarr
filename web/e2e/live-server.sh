#!/usr/bin/env bash
# Builds the server and the SPA, then runs mistarr against a fresh temp data dir.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$repo/target}"
port="${MISTARR_E2E_PORT:-18420}"

data_dir="$(mktemp -d)"
mkdir -p "$data_dir/games"
cat > "$data_dir/mistarr.toml" <<TOML
[server]
listen = "127.0.0.1:$port"

[paths]
root = "$data_dir"
games = "$data_dir/games"

[client]
kind = "transmission"
url = "http://127.0.0.1:1/transmission/rpc"
TOML

export CARGO_TARGET_DIR="$target_dir"
cargo build -p mistarr-server
(cd "$repo/web" && npm run build)

exec "$target_dir/debug/mistarr" --data "$data_dir" serve
