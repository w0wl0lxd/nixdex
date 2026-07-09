set shell := ["bash", "-uc"]

default:
    @just --list

setup-hooks:
    git config core.hooksPath .githooks
    @chmod +x .githooks/pre-commit .githooks/commit-msg .githooks/prepare-commit-msg .githooks/pre-push
    @echo "hooks active: core.hooksPath=.githooks"
    @ls -1 .githooks/

fmt:
    cargo fmt --all

check:
    cargo check --all-features

clippy:
    cargo clippy --all-features -- -D warnings

test:
    cargo nextest run --workspace --no-fail-fast

build:
    cargo build --release

secrets:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v gitleaks >/dev/null 2>&1; then
      gitleaks detect --source . --config .gitleaks.toml --redact --verbose --exit-code 1
    else
      echo "error: gitleaks not installed" >&2
      exit 1
    fi
    if command -v ripsecrets >/dev/null 2>&1; then
      ripsecrets --strict-ignore .
    else
      echo "error: ripsecrets not installed" >&2
      exit 1
    fi

validate: secrets fmt check clippy test
