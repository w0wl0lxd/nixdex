# AGENTS.md — nixdex

nixdex is a Rust workspace (`crates/nixdex-core`, `crates/nixdex-cli`, `crates/nixdex-daemon`)
that rewrites `nix-index` with modern Rust, zero-copy parsing, and a `nix-locate` /
`nix-index` compatible CLI.

## Build Commands

```bash
cargo check --all-features
cargo clippy --all-features -- -D warnings
cargo nextest run --no-fail-fast
cargo fmt --all
```

## Code Rules

- Edition 2024, Rust stable via `rust-toolchain.toml`.
- `unsafe`, `unwrap`, `panic`, `todo`, `unimplemented`, `dbg!`,
  `std::collections::HashMap`/`HashSet`, and `f32` are forbidden in production code.
- Propagate errors with `?` and typed `thiserror`/`anyhow` errors.
- Prefer `sonic-rs`, `bytes`, `smallvec`, `compact_str`,
  `scc`/`ahash`/`rustc-hash`/`indexmap` for collections.
- Keep functions under 100 lines and below `clippy.toml` complexity thresholds.

## Shared Reasoning Memory

Use the `thoughtbox` MCP knowledge graph for durable facts across agents: nix-index
database format decisions, `nix-eval-jobs` integration findings, and parser edge cases.
Ephemeral reasoning belongs in a thoughtbox session.
