#!/usr/bin/env bash
set -euo pipefail

# Compare `nixdex index` and upstream `nix-index` build times on a tiny
# nixpkgs set. This is useful for CI/quick regression checks; a full
# `<nixpkgs>` comparison is still best measured with `scripts/benchmark-index.sh`.
#
# Usage:
#   scripts/benchmark-index-comparison.sh [runs]

RUNS="${1:-5}"
WARMUP="${WARMUP:-2}"
OUT_MD="${OUT_MD:-/tmp/nixdex-bench-index-compare.md}"

if ! command -v nix-index >/dev/null 2>&1; then
  echo "error: nix-index not found; cannot run comparison" >&2
  exit 1
fi

cargo build --release --bin nixdex
NIXDEX="$(realpath "${CARGO_TARGET_DIR:-target}/release/nixdex")"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

NIXPKGS_MINIMAL="$TMP_DIR/minimal.nix"
cat >"$NIXPKGS_MINIMAL" <<'EOF'
let
  pkgs = import <nixpkgs> { config = {}; };
in
{
  inherit (pkgs) hello coreutils;
}
EOF

NIXDEX_DB="$TMP_DIR/nixdex-db"
NIXINDEX_DB="$TMP_DIR/nix-index-db"

SELECT="p: { inherit (p) hello coreutils; }"

echo "=== nixdex vs nix-index index build comparison ==="
echo "minimal set: { inherit (pkgs) hello coreutils; }"
echo "runs: $RUNS"

hyperfine \
  --warmup "$WARMUP" \
  --min-runs "$RUNS" \
  --prepare "rm -rf '$NIXDEX_DB' '$NIXINDEX_DB'; mkdir -p '$NIXDEX_DB' '$NIXINDEX_DB'" \
  --export-markdown "$OUT_MD" \
  -n 'nixdex index --small' \
  "\"$NIXDEX\" index --small --select '$SELECT' -d '$NIXDEX_DB' -f '<nixpkgs>' --requests 4 >/dev/null 2>&1" \
  -n 'nix-index --filter-prefix /bin/' \
  "nix-index -d '$NIXINDEX_DB' -f '$NIXPKGS_MINIMAL' --filter-prefix /bin/ --requests 4 --extra-scopes '' >/dev/null 2>&1"

echo "Index comparison report: $OUT_MD"
