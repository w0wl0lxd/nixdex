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
  echo "no database at '$DB_DIR/files'; downloading the prebuilt index" >&2
  mkdir -p "$DB_DIR"
  "$NIXDEX" index --download-prebuilt -d "$DB_DIR" || true
fi

if [[ ! -f "$DB_DIR/files" ]]; then
  echo "error: no database found at '$DB_DIR/files'" >&2
  echo "Run 'nixdex index --download-prebuilt -d $DB_DIR' first, or pass a database directory." >&2
  exit 1
fi

echo "database: $(ls -l "$DB_DIR/files")" >&2
df -h "$DB_DIR" >&2 || true
free -m >&2 || true
echo "single nixdex query" >&2
timeout 120 "$NIXDEX" locate -d "$DB_DIR" 'bin/ls' | head -3 >&2 || echo "nixdex single query exit=$?" >&2
free -m >&2 || true
if command -v nix-locate >/dev/null 2>&1; then
  echo "single nix-locate query" >&2
  timeout 120 nix-locate -d "$DB_DIR" 'bin/ls' | head -3 >&2 || echo "nix-locate single query exit=$?" >&2
  free -m >&2 || true
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

  # No shell redirection: hyperfine 2 runs a command directly, so `>/dev/null`
  # would reach the program as an argument. `--output null` discards output.
  nixdex_cmd="$nixdex_cmd '$pattern'"
  nixlocate_cmd="$nixlocate_cmd '$pattern'"

  echo "## Pattern: '$pattern' (flags=$flags)" >>"$OUT_MD"
  echo "" >>"$OUT_MD"

  QUERY_MD="$(mktemp -p "$TMP_DIR" bench-locate-query-XXXXXX.md)"

  if command -v nix-locate >/dev/null 2>&1; then
    hyperfine \
      --output null \
      --warmup "$WARMUP" \
      --min-runs "$MIN_RUNS" \
      --export-markdown "$QUERY_MD" \
      "$nixdex_cmd" \
      "$nixlocate_cmd"
  else
    hyperfine \
      --output null \
      --warmup "$WARMUP" \
      --min-runs "$MIN_RUNS" \
      --export-markdown "$QUERY_MD" \
      "$nixdex_cmd"
  fi

  cat "$QUERY_MD" >>"$OUT_MD"
  echo "" >>"$OUT_MD"
done

echo "Locate benchmark report: $OUT_MD"
