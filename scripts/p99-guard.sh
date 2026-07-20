#!/usr/bin/env bash
set -euo pipefail

# p99 latency gate for the resident-daemon search path.
#
# Two modes:
#   1. No DB argument (default / CI): run the `locate` criterion bench on the
#      synthetic fixture. Every bench's mean latency must stay under the gate.
#      This is a regression guard for the entry-index + ngram-index hot paths.
#   2. DB=<path> provided: run `nixdex locate` against a real `files` index db
#      and assert hyperfine p99 < 100ms for a representative query set. Requires
#      a resident daemon (or warm local index) for meaningful warm numbers.
#
# Usage:
#   scripts/p99-guard.sh            # synthetic regression gate
#   scripts/p99-guard.sh /path/db  # real-DB p99 gate (DB is the index dir)

GATE_NS=100000000 # 100ms in nanoseconds

DB="${1:-}"

if [[ -n "$DB" ]]; then
  echo "=== p99 guard: real DB mode ($DB) ==="
  if ! command -v hyperfine >/dev/null 2>&1; then
    echo "error: hyperfine is required for the real-DB gate" >&2
    exit 1
  fi
  cargo build --release --bin nixdex
  NIXDEX=$(realpath "${CARGO_TARGET_DIR:-target}/release/nixdex")

  QUERIES=("bin/ls" "python3" "libc.so" "vim")
  for q in "${QUERIES[@]}"; do
    echo "--- query: $q ---"
    STATS=$(hyperfine --warmup 3 --min-runs 20 --export-json - \
      "$NIXDEX locate --database '$DB' '$q'" |
      python3 -c 'import sys,json; d=json.load(sys.stdin); print(d["results"][0]["p99"])')
    # hyperfine reports seconds; convert to ns.
    P99_NS=$(python3 -c "print(int(float('$STATS')*1_000_000_000))")
    echo "p99: ${P99_NS}ns (gate ${GATE_NS}ns)"
    if ((P99_NS > GATE_NS)); then
      echo "error: p99 ${P99_NS}ns exceeds gate ${GATE_NS}ns for query '$q'" >&2
      exit 1
    fi
  done
  echo "real-DB p99 gate passed"
  exit 0
fi

echo "=== p99 guard: synthetic fixture regression gate ==="
OUT=$(cargo bench --bench locate 2>&1)
echo "$OUT" | sed -n '1,200p'

# Parse every `time: [lo mean hi]` line and keep the worst mean.
if ! echo "$OUT" | grep -q 'time:'; then
  echo "error: no bench timing captured (did 'cargo bench --bench locate' fail?)" >&2
  exit 1
fi
WORST=0
while IFS= read -r mean; do
  ns=${mean%.*}
  # criterion prints ns/µs/ms; normalize to ns.
  case "$mean" in
  *ms) ns=$(python3 -c "print(int(float('${mean%ms}')*1_000_000))") ;;
  *us) ns=$(python3 -c "print(int(float('${mean%us}')*1000))") ;;
  *ns) ns=$ns ;;
  *s) ns=$(python3 -c "print(int(float('${mean%s}')*1_000_000_000))") ;;
  esac
  if ((ns > WORST)); then WORST=$ns; fi
done < <(echo "$OUT" | grep -oE 'time:\s+\[[0-9.]+ (ns|us|ms|s) [0-9.]+ (ns|us|ms|s) [0-9.]+ (ns|us|ms|s)\]' | grep -oE '[0-9.]+ (ns|us|ms|s)' | sed -n '2p')

echo "worst mean latency: ${WORST}ns (gate ${GATE_NS}ns)"
if ((WORST > GATE_NS)); then
  echo "error: worst mean ${WORST}ns exceeds gate ${GATE_NS}ns" >&2
  exit 1
fi
echo "synthetic p99 gate passed"
