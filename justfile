set shell := ["bash", "-uc"]

default:
    @just --list

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
