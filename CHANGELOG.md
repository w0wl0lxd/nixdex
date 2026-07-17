# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Add Criterion benchmark harness (`benches/index.rs`, `benches/search.rs`, `benches/locate.rs`) and `criterion.toml`.
- Add verification workflows for coverage, exhaustive checks, hax, Kani, and Miri.
- Restructure `ci.yml` into fmt/check/clippy/test/deny/benchmark jobs with stable/beta and Ubuntu/macOS matrices.
- Add Kani proof for `basename_index::read_u32_le` and set `deny.toml` `all-features = true`.
- `nixdex info` text output now includes `main_program` for each output.
- `nixdex search` supports `--sort` by `attr`, `name`, and `main-program` in ascending or descending order.
- `nixdex index` accepts `--no-overlays` to disable nixpkgs overlays during evaluation.
- `nixdex index` accepts `--no-closure` to skip runtime reference traversal when fetching `.ls` listings.
- `nixdex index` accepts `--timeout` and `--retries` to configure binary-cache HTTP requests.
- `nixdex command-not-found` suggests running a missing command with `comma` when `,` is on `$PATH`.
- `nixdex command-not-found` supports `--interactive` and `NIX_AUTO_RUN_INTERACTIVE` for provider selection before auto-run.
- `nixdex-daemon` exposes `/version` and `/ready` HTTP endpoints.
- Add redb-backed file index (`files.redb`) with exact-path cache sidecar (`files.pathcache`).
- Add `redb` module with `Writer` for building the index and `Reader` for querying by origin key (`attr.output`), basename, or exact path.
- Key cached package entries by origin (`attr.output`) instead of hash to preserve all attribute aliases for the same store output, ensuring `--at-root --whole-name` and `--minimal` queries return all matching origins.

### Changed
- Track `.config/rail.toml` and wire `cargo-rail` into `mise.toml` and `justfile` for dependency linting.
- Apply `cargo fmt` to the redb index and CLI command-not-found code.

### Fixed
- Address CodeRabbit review comments: forward full `$argv` in Fish handler, default prebuilt architecture to host, normalize rooted locate patterns, cap result limits in daemon endpoints, make minimal `nix-locate` results omit unused fields, coalesce `/reload` requests over a bounded one-slot channel, and apply `spawn_blocking`/`spawn_blocking` for CPU-bound searches.
- Fix redb index compilation: remove unused `FileTree` import and iterate over entry slice directly.
- Fix redb index to use the same filtered file entries as the NIXI database so excluded prefixes are not cached.
- Apply `--hash` and `--package` filters to exact-path redb lookup results.
- Replace `unreachable!()` arms in `database.rs` with `Error::Corrupt` for unknown database versions.
- Add size validation to attrs sidecar reader to reject oversized files before allocation
- Fix path ordinal resolution to return empty bitmap instead of None for empty lookups
- Optimize path index prefix lookup to stop early when keys no longer match prefix
- Fix prebuilt cache key to use SHA256 digest instead of raw ETag/Last-Modified header values
