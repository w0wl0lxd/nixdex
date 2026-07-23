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

rail-plan:
    cargo rail plan --merge-base --explain

rail-run PROFILE='local':
    cargo rail run --merge-base --profile '{{PROFILE}}'

rail-validate:
    cargo rail config validate

validate: secrets fmt check clippy test changelog-check

benchmark:
    cargo bench --benches
    @echo ""
    @echo "Shell-level comparisons:"
    @echo "  just benchmark-index-compare      # nixdex vs nix-index on a tiny set"
    @echo "  just benchmark-locate [DB_DIR]    # nixdex vs nix-locate"
    @echo "  just benchmark-search [DB_DIR]    # nixdex search"
    @echo "  just benchmark-index <nixpkgs> [eval|full] [runs]  # nixdex index build"

benchmark-locate DB='':
    ./scripts/benchmark-locate.sh "{{DB}}"

benchmark-search DB='':
    ./scripts/benchmark-search.sh "{{DB}}"

benchmark-index-compare RUNS='5':
    ./scripts/benchmark-index-comparison.sh '{{RUNS}}'

# p99 latency gate for the resident-daemon search path. With no real DB it
# exercises the synthetic fixture (regression guard); pass a real `files` index
# DB path via DB=... to gate against production-scale data.
benchmark-p99 DB='':
    ./scripts/p99-guard.sh '{{DB}}'

benchmark-index NIXPKGS='<nixpkgs>' MODE='eval' RUNS='1':
    ./scripts/benchmark-index.sh '{{NIXPKGS}}' '{{MODE}}' '{{RUNS}}'
