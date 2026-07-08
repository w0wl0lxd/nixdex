set shell := ["bash", "-uc"]

default:
    @just --list

# One-time per clone: point git at tracked hooks under .githooks/
setup-hooks:
    git config core.hooksPath .githooks
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

validate: fmt check clippy test
