#!/usr/bin/env bash
set -euo pipefail

# Publishes nixdex workspace crates to crates.io.
# The default order claims the umbrella `nixdex` name first, then the
# dependency crates. Additional crate names can be passed as arguments
# for future nixdex-XYZ subcrate names.

DEFAULT_CRATES=(nixdex nixdex-core nixdex-cli nixdex-daemon)
MAX_ATTEMPTS=5
RETRY_DELAY=30

publish_crate() {
  local crate=$1
  local attempt=1
  while true; do
    echo "Publishing $crate (attempt $attempt/$MAX_ATTEMPTS)..."
    if cargo publish -p "$crate"; then
      echo "Published $crate."
      return 0
    fi
    if [ "$attempt" -ge "$MAX_ATTEMPTS" ]; then
      echo "Failed to publish $crate after $MAX_ATTEMPTS attempts." >&2
      return 1
    fi
    echo "Retrying $crate in $RETRY_DELAY seconds..."
    sleep "$RETRY_DELAY"
    attempt=$((attempt + 1))
  done
}

CRATES=("${@:-${DEFAULT_CRATES[@]}}")

cd "$(dirname "$0")/.."

for crate in "${CRATES[@]}"; do
  publish_crate "$crate"
done

echo "All crates published."
