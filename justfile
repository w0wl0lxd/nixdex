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

changelog:
    python3 scripts/changelog.py collect

changelog-check:
    python3 scripts/changelog.py check --base origin/main

validate: secrets fmt check clippy test changelog-check

benchmark:
    cargo bench --bench search
    @echo "Run 'just benchmark-index <nixpkgs> [eval|full] [runs]' for a full index build benchmark."

benchmark-index NIXPKGS='<nixpkgs>' MODE='eval' RUNS='1':
    ./scripts/benchmark-index.sh '{{NIXPKGS}}' '{{MODE}}' '{{RUNS}}'
