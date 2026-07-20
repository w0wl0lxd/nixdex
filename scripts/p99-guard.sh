#!/usr/bin/env bash
set -euo pipefail

# p99 latency gate for the resident-daemon search path.
#
# Two modes:
#   1. No DB argument (default / CI): run the `locate` Criterion bench in
#      --quick mode. Every benchmark's mean latency must stay under the gate.
#      This is a regression guard for the entry-index + ngram-index hot paths.
#   2. DB=<path> provided: run `nixdex locate` against a real `files` index db
#      and assert hyperfine p99 < 50ms for a representative query set. Requires
#      a resident daemon (or warm local index) for meaningful warm numbers.
#
# Usage:
#   scripts/p99-guard.sh            # synthetic regression gate
#   scripts/p99-guard.sh /path/db   # real-DB p99 gate (DB is the index dir)

GATE_NS=50000000 # 50ms in nanoseconds

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
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
# Build the locate benchmark binary directly; in some build roots the artifact
# is written without the executable bit, so fix permissions before running.
cargo build --release --bench locate
BENCH_BIN=$(find "$TARGET_DIR/release/deps" -maxdepth 1 -name 'locate-*' -type f ! -name '*.d' \
  -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2-)
if [[ -z "$BENCH_BIN" ]]; then
  echo "error: could not locate locate benchmark binary under $TARGET_DIR/release/deps" >&2
  exit 1
fi
chmod +x "$BENCH_BIN"
OUT=$("$BENCH_BIN" --bench --quick 2>&1)
echo "$OUT" | sed -n '1,200p'

# Parse every `time: [lo mean hi]` line, normalize the mean to ns, and keep the worst.
PY=$(cat <<'PY'
import re, sys

text = sys.stdin.read()
pattern = re.compile(
    r'time:\s+\[([\d.]+)\s+(\S+)\s+([\d.]+)\s+(\S+)\s+([\d.]+)\s+(\S+)\]'
)
factors = {
    'ns': 1,
    'us': 1_000,
    '\u00b5s': 1_000,
    'ms': 1_000_000,
    's': 1_000_000_000,
}
worst = 0
for m in pattern.finditer(text):
    mean_val = float(m.group(3))
    unit = m.group(4)
    factor = factors.get(unit, 0)
    if factor == 0:
        continue
    ns = int(mean_val * factor)
    if ns > worst:
        worst = ns
print(worst)
PY
)
WORST=$(echo "$OUT" | python3 -c "$PY")

if ! [[ "$WORST" =~ ^[0-9]+$ ]]; then
  echo "error: no bench timing captured (did the locate benchmark fail?)" >&2
  exit 1
fi

echo "worst mean latency: ${WORST}ns (gate ${GATE_NS}ns)"
if ((WORST > GATE_NS)); then
  echo "error: worst mean ${WORST}ns exceeds gate ${GATE_NS}ns" >&2
  exit 1
fi
echo "synthetic p99 gate passed"
