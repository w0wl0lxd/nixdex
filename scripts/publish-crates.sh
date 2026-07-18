#!/usr/bin/env bash
set -euo pipefail

# Publishes nixdex workspace crates to crates.io.
# The default order publishes dependency crates first, then the umbrella
# `nixdex` crate. Additional crate names can be passed as arguments for
# future nixdex-XYZ subcrate names.

DEFAULT_CRATES=(nixdex-core nixdex-cli nixdex-daemon nixdex)
MAX_ATTEMPTS=5
RETRY_DELAY=30

# Return 0 if the given crate/version already exists on crates.io.
is_published() {
  local crate=$1
  local version=$2
  local status
  status=$(curl -s -o /dev/null -w "%{http_code}" "https://crates.io/api/v1/crates/$crate/$version" 2>/dev/null || echo 000)
  [ "$status" = "200" ]
}

# Extract the version from `cargo pkgid` output, which may take forms such as
# `path+file:///.../crates/nixdex-core#0.1.0` or `nixdex-core@0.1.0`.
crate_version() {
  local pkgid=$1
  local version="${pkgid##*#}"
  version="${version##*@}"
  version="${version##*:}"
  printf '%s' "$version"
}

publish_crate() {
  local crate=$1
  local attempt=1

  local pkgid
  pkgid=$(cargo pkgid -p "$crate")
  local version
  version=$(crate_version "$pkgid")

  while true; do
    if is_published "$crate" "$version"; then
      echo "$crate $version is already published. Skipping."
      return 0
    fi

    echo "Publishing $crate $version (attempt $attempt/$MAX_ATTEMPTS)..."
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
