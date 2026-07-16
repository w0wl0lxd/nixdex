#!/usr/bin/env bash
set -euo pipefail

# Benchmark `nixdex index` wall-clock time and print the structured INFO log
# emitted by nixdex-core. Use `--only-eval` mode for a quick eval throughput
# measurement, or `full` for an end-to-end eval + fetch + write measurement.
#
# Usage:
#   scripts/benchmark-index.sh <nixpkgs> [mode]
#
# Examples:
#   scripts/benchmark-index.sh '<nixpkgs>' eval
#   scripts/benchmark-index.sh '<nixpkgs>' full
#   scripts/benchmark-index.sh /path/to/custom-nixpkgs full

NIXPKGS="${1:-<nixpkgs>}"
MODE="${2:-eval}"
RUNS="${3:-1}"

DB_DIR=$(mktemp -d -t nixdex-bench-XXXXXX)
trap 'rm -rf "$DB_DIR"' EXIT

cargo build --release --bin nixdex
NIXDEX=$(realpath "${CARGO_TARGET_DIR:-target}/release/nixdex")

EXTRA_ARGS="--requests 100"
if [[ "$MODE" == "eval" ]]; then
  EXTRA_ARGS="$EXTRA_ARGS --only-eval"
fi

OUT_MD="/tmp/nixdex-bench-${MODE}.md"

echo "=== nixdex index benchmark ==="
echo "nixpkgs: $NIXPKGS"
echo "mode:    $MODE"
echo "runs:    $RUNS"
echo "db dir:  $DB_DIR"

hyperfine \
  --runs "$RUNS" \
  --prepare "rm -rf '$DB_DIR'; mkdir -p '$DB_DIR'" \
  --export-markdown "$OUT_MD" \
  --show-output \
  "RUST_LOG=info \"$NIXDEX\" index -d '$DB_DIR' -f '$NIXPKGS' $EXTRA_ARGS"

echo ""
echo "Hyperfine results written to $OUT_MD"
