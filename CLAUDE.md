# CLAUDE.md — nixdex

> See `AGENTS.md` for universal repository rules and build commands.

## Core Commands

```bash
cargo check --all-features
cargo clippy --all-features -- -D warnings
cargo nextest run --no-fail-fast
```

## Critical Patterns

- No `unsafe`, `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, `dbg!` in production code.
- No `std::collections::HashMap`/`HashSet` or `f32`.
- Use `thiserror` in `nixdex-core`, `anyhow`/`color-eyre` in CLI/daemon crates.
- Propagate errors with `?`/`ok_or_else`/`map_err`.
- Prefer `sonic-rs` over `serde_json` for hot JSON; keep `serde`/`bytes` for serializable core types.
- Use `nixdex-core` types (`StorePath`, `FileTree`, `FileNode`, `FileType`) throughout;
  avoid duplicate definitions.

## Claude Tool Preferences

- `cargo nextest` for tests.
- `sccache` for builds (configured in `.cargo/config.toml`).
- `mold` + `clang` linker (`target.x86_64-unknown-linux-gnu`).

## No AI-Attribution

Never add "Generated with Devin", "Co-Authored-By: Devin", or other AI-agent
attribution to commits, PRs, or authored content.
