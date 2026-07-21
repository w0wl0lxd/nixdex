#!/usr/bin/env bash
set -euo pipefail

# Benchmark `nixdex locate` against upstream `nix-locate`.
#
# Usage:
#   scripts/benchmark-locate.sh [database-dir]
#
# The database dir must contain a valid `files` database (upstream or nixdex).
# If no dir is supplied, $NIXDEX_BENCH_DB or ~/.cache/nixdex is used.

DB_DIR="${1:-${NIXDEX_BENCH_DB:-$HOME/.cache/nixdex}}"
WARMUP="${WARMUP:-3}"
MIN_RUNS="${MIN_RUNS:-5}"
OUT_MD="${OUT_MD:-/tmp/nixdex-bench-locate.md}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

cargo build --release --bin nixdex
NIXDEX="$(realpath "${CARGO_TARGET_DIR:-target}/release/nixdex")"

if [[ ! -f "$DB_DIR/files" ]]; then
  echo "no database found at '$DB_DIR/files'; downloading prebuilt index..."
  for attempt in 1 2 3; do
    if "$NIXDEX" update --small -d "$DB_DIR"; then
      break
    fi
    echo "download attempt $attempt failed; retrying in 10s..." >&2
    sleep 10
  done
  if [[ ! -f "$DB_DIR/files" ]]; then
    echo "error: failed to download prebuilt index to '$DB_DIR'" >&2
    echo "Run 'nixdex update -d $DB_DIR' first, or pass an existing database directory." >&2
    exit 1
  fi
fi

if ! command -v nix-locate >/dev/null 2>&1; then
  echo "warning: nix-locate not found; only nixdex will be benchmarked" >&2
fi

QUERIES=(
  "bin/firefox:"
  "bin/ls:"
  "lib/libc.so:"
  "share/man/man1/ls.1:"
  "bin/.*test$:r"
  "firefox:w"
)

rm -f "$OUT_MD"
{
  echo "# nixdex locate vs nix-locate"
  echo ""
  echo "Database: $DB_DIR"
  echo ""
} >"$OUT_MD"

for entry in "${QUERIES[@]}"; do
  if [[ "$entry" == *":"* ]]; then
    pattern="${entry%%:*}"
    flags="${entry##*:}"
  else
    pattern="$entry"
    flags=""
  fi

  nixdex_cmd="\"$NIXDEX\" locate -d \"$DB_DIR\""
  nixlocate_cmd="nix-locate -d \"$DB_DIR\""

  if [[ "$flags" == "r" ]]; then
    nixdex_cmd="$nixdex_cmd -r"
    nixlocate_cmd="$nixlocate_cmd -r"
  elif [[ "$flags" == "w" ]]; then
    nixdex_cmd="$nixdex_cmd -w"
    nixlocate_cmd="$nixlocate_cmd -w"
  fi

  nixdex_cmd="$nixdex_cmd '$pattern' >/dev/null 2>&1"
  nixlocate_cmd="$nixlocate_cmd '$pattern' >/dev/null 2>&1"

  echo "## Pattern: '$pattern' (flags=$flags)" >>"$OUT_MD"
  echo "" >>"$OUT_MD"

  QUERY_MD="$(mktemp -p "$TMP_DIR" bench-locate-query-XXXXXX.md)"

  if command -v nix-locate >/dev/null 2>&1; then
    hyperfine \
      --warmup "$WARMUP" \
      --min-runs "$MIN_RUNS" \
      --export-markdown "$QUERY_MD" \
      "$nixdex_cmd" \
      "$nixlocate_cmd"
  else
    hyperfine \
      --warmup "$WARMUP" \
      --min-runs "$MIN_RUNS" \
      --export-markdown "$QUERY_MD" \
      "$nixdex_cmd"
  fi

  cat "$QUERY_MD" >>"$OUT_MD"
  echo "" >>"$OUT_MD"
done

echo "Locate benchmark report: $OUT_MD"
