#!/usr/bin/env bash
# Local loop: fetch the dev sources, publish feed.xml, serve it for Reeder.
# Re-run after a code change; the feed URL stays the same.
set -euo pipefail
cd "$(dirname "$0")/.."

out="${PAPERS_OUT:-$PWD/dev/out}"
mkdir -p "$out"
export PAPERS_OUT="$out"
export PAPERS_DB="$out/papers.db"
export PAPERS_CONFIG="${PAPERS_CONFIG:-$PWD/dev/sources.toml}"
export PAPERS_PORT="${PAPERS_PORT:-8199}"
lan=$(/usr/sbin/ipconfig getifaddr en0 2>/dev/null || echo localhost)
export PAPERS_BASE_URL="${PAPERS_BASE_URL:-http://${lan}:${PAPERS_PORT}}"

cargo run -- run --no-llm
echo "feed: $PAPERS_BASE_URL/feed.xml"
exec cargo run -- serve
