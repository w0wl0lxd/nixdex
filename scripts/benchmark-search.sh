#!/usr/bin/env bash
set -euo pipefail

# Benchmark the `nixdex search` package-metadata command.
#
# Usage:
#   scripts/benchmark-search.sh [database-dir]
#
# The database dir must contain a `packages.json` sidecar. If no dir is supplied,
# $NIXDEX_BENCH_DB or ~/.cache/nixdex is used.

DB_DIR="${1:-${NIXDEX_BENCH_DB:-$HOME/.cache/nixdex}}"
WARMUP="${WARMUP:-3}"
MIN_RUNS="${MIN_RUNS:-5}"
OUT_MD="${OUT_MD:-/tmp/nixdex-bench-search.md}"

cargo build --release --bin nixdex
NIXDEX="$(realpath "${CARGO_TARGET_DIR:-target}/release/nixdex")"

if [[ ! -f "$DB_DIR/packages.json" ]]; then
  echo "error: no packages.json sidecar found in '$DB_DIR'" >&2
  echo "Run 'nixdex index --download-prebuilt -d $DB_DIR' first, or pass a nixdex database directory." >&2
  exit 1
fi

QUERIES=(
  "firefox"
  "hello"
  "git"
  "^nix.*$:r"
  "^hello$:rx"
)

rm -f "$OUT_MD"
{
  echo "# nixdex search benchmark"
  echo ""
  echo "Database: $DB_DIR"
  echo ""
} >"$OUT_MD"

for entry in "${QUERIES[@]}"; do
  pattern="${entry%%:*}"
  flags="${entry##*:}"

  cmd="\"$NIXDEX\" search -d \"$DB_DIR\""
  if [[ "$flags" == *"r"* ]]; then
    cmd="$cmd -r"
  fi
  if [[ "$flags" == *"x"* ]]; then
    cmd="$cmd --exact"
  fi
  cmd="$cmd '$pattern' >/dev/null 2>&1"

  echo "## Pattern: '$pattern' (flags=$flags)" >>"$OUT_MD"
  echo "" >>"$OUT_MD"

  hyperfine \
    --warmup "$WARMUP" \
    --min-runs "$MIN_RUNS" \
    --export-markdown /tmp/bench-search-query.md \
    "$cmd"

  cat /tmp/bench-search-query.md >>"$OUT_MD"
  echo "" >>"$OUT_MD"
done

echo "Search benchmark report: $OUT_MD"
