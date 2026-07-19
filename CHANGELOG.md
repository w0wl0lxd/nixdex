# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Expanded the benchmark suite for `nixdex-core` and the `nixdex` CLI.
- Added Criterion baselines for `SearchDb` open/load, literal/exact/regex/description/fuzzy package search, and sort orders across dataset sizes.
- Parameterized locate and index Criterion benches by package/file count and added `Reader::open`, `search_entries`, `search_results`, and `generate_sidecars` baselines with element throughput reporting.
- Added `scripts/benchmark-locate.sh`, `scripts/benchmark-search.sh`, and `scripts/benchmark-index-comparison.sh` to compare `nixdex` against upstream `nix-index`/`nix-locate` via `hyperfine`.
- Updated `just benchmark` to run all Criterion benches and added `just benchmark-locate`, `just benchmark-search`, and `just benchmark-index-compare` recipes for CLI-level comparisons.
- Updated `.github/workflows/benchmark.yml` to trigger the fast `searchdb-benchmark` job on `push` to `main`/`master` and on `pull_request` when relevant files change, while keeping the heavy `index-benchmark` job on scheduled and dispatched runs.
- `command-not-found.sh` suggests running a missing command with `comma` when it is on `$PATH`.
- `command-not-found.sh` supports `NIX_AUTO_RUN_INTERACTIVE` to prompt for a provider before auto-running a missing command.
- `command-not-found.nu` suggests `comma` for one-time command execution when available.
- Add integration tests verifying that `nixdex` subcommands default to `~/.cache/nixdex` while standalone `nix-index` and `nix-locate` retain the upstream-compatible `~/.cache/nix-index` default.
- Add Criterion benchmark harness (`benches/index.rs`, `benches/search.rs`, `benches/locate.rs`) and `criterion.toml`.
- Add verification workflows for coverage, exhaustive checks, hax, Kani, and Miri.
- Restructure `ci.yml` into fmt/check/clippy/test/deny/benchmark jobs with stable/beta and Ubuntu/macOS matrices.
- Add Kani proof for `basename_index::read_u32_le` and set `deny.toml` `all-features = true`.
- Added `nixdex generate-man` and installed man pages for `nixdex`, `nix-index`, and `nix-locate` through `flake.nix`.
- Added `frcode` (Divan) and `basename_index` (Criterion) benchmark suites covering hot codec and secondary-index paths.
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
- Added `nixdex generate-completions` and installed bash, zsh, and fish completion files for `nixdex`, `nix-index`, and `nix-locate` via `flake.nix`.
- `nix-index --cache-url` now accepts `--substituter` as a visible alias for compatibility with upstream nix-index.

### Changed
- Track `.config/rail.toml` and wire `cargo-rail` into `mise.toml` and `justfile` for dependency linting.
- Added `--chunk-size` to `nix-index` to control uncompressed v2 frame buffering; the default is now 64 MiB to reduce peak memory usage during builds.
- Daemon now loads index components into an atomic `IndexSnapshot` so requests never observe a partially loaded index during reload.
- Add `--admin-token` / `NIXDEX_ADMIN_TOKEN` authentication for `POST /reload`; without a token the endpoint is restricted to loopback addresses.
- `nix-index` now includes the `darwin` package set in the default extra scopes when the target system is Darwin.
- Change the default database directory for `nixdex` subcommands and the `nixdex` umbrella binary to `~/.cache/nixdex`. The standalone `nix-index` and `nix-locate` binaries retain the upstream-compatible `~/.cache/nix-index` default.
- Exact attribute lookups in package search now use a pre-built index, making `nixdex info` and `nixdex search --exact --attr` O(1).
- Apply `cargo fmt` to the redb index and CLI command-not-found code.
- Rework `frcode::Decoder`'s `ResizableBuf` so it is pre-zeroed and only grows when an entry exceeds the current allocation. This removes per-chunk `Vec::extend_from_slice` overhead and lets `read_to_nul` / `copy_shared` use `copy_from_slice` / `copy_within` directly.
- Package fuzzy search now uses the `frizbee` SIMD matcher, dramatically improving matching throughput and reducing query latency.
- `nix-index` builds now display `indicatif` progress bars for nixpkgs evaluation and `.ls` fetching, including per-second rates and ETA.
- Updated `.gitignore` to ignore the `research/` directory and local dev configs such as `.cargo/config.toml`, `.config/sccache/config.toml`, and `mise.toml`.
- Make the `redb` exact-path sidecar opt-in via a new `--redb` flag on `nixdex index` / `nix-index`. The sidecar is no longer built by default, which substantially reduces database size and improves query startup latency. Users who need fast `--at-root` or exact full-path lookups can enable it explicitly.
- Use `memchr::memmem` directly for literal `PathMatcher` block candidate search instead of a `grep` line matcher, and use `copy_within` for `frcode` shared-prefix copies and residual shifts.
- Switch the `nixdex` CLI to a synchronous `main` with a manually-built Tokio runtime only for async subcommands, eliminating runtime startup overhead for `locate`, `which`, and `command-not-found`.
- Rewrote `PathMatcher` to build a `grep`-style line matcher and `search_frame_decoder` to jump directly to candidate lines, decoding and verifying only path matches instead of every frcode line.
- Replaced the fixed-size frcode decode buffer with a `Vec`-backed resizable buffer and `copy_within` for residual shifting, and added a fast literal-substring `PathMatcher` using `memchr::memmem::Finder` for non-anchored `nix-locate` queries.
- Database searches reuse a per-thread zstd decompression context, reducing allocator pressure and improving repeated `nixdex locate` query latency.
- Open `zstd::stream::read::Decoder` with `with_buffer` / `single_frame` for `&[u8]` inputs, avoiding an extra `BufReader` layer, the `Cursor` wrapper, and an unnecessary multi-frame scan during database locate operations.

### Fixed
- Cache `uses_nix_profile` in the shell wrapper to avoid repeated filesystem checks inside loops.
- Filter empty `XDG_STATE_HOME`/`HOME` values when locating the `nix profile` manifest.
- Guard shell-completion generation with a can-run check so cross-compiled builds do not fail.
- Install Bash completion files under their command names so `bash-completion` loads them on demand.
- Make `scripts/publish-crates.sh` idempotent by checking crates.io before publishing and by re-checking on each retry, allowing reruns to resume after partial failures.
- Publish dependency crates before the umbrella `nixdex` crate.
- Add `nixdex-core` as a dependency of the umbrella `nixdex` crate and re-export its public API.
- Fixed `nixdex-core` compilation without the `daemon` feature by gating Unix signal helpers behind `#[cfg(feature = "daemon")]`.
- Use `~/.cache/nixdex` as the default database directory for `nixdex index`, `nixdex locate`, and `nixdex search`, while `nix-index` and `nix-locate` continue defaulting to `~/.cache/nix-index`.
- Skip file entries whose paths or symlink targets contain NUL or newline instead of failing to encode the entire package.
- Collect all top-level package entries before traversing closure references in the listing fetcher. This prevents packages like `coreutils` from being indexed under a bare closure label (`.out`) or hidden entirely when they appear as dependencies of earlier packages.
- Address CodeRabbit review comments: forward full `$argv` in Fish handler, default prebuilt architecture to host, normalize rooted locate patterns, cap result limits in daemon endpoints, make minimal `nix-locate` results omit unused fields, coalesce `/reload` requests over a bounded one-slot channel, and apply `spawn_blocking`/`spawn_blocking` for CPU-bound searches.
- Address review feedback for PR #33: batch Tokio Mutex acquisitions when feeding root package entries.
- Parallelize v2 frame compression with a per-task `zstd::bulk::Compressor` instance.
- Remove stale `files.redb`/`files.pathcache` sidecars when the `--redb` option is disabled.
- Stream-decode NIXI v1 (upstream prebuilt) zstd frames during search and sidecar generation, allowing `nixdex update` to process the upstream `nix-index-database` releases without hitting the `MAX_ZSTD_FRAME_BYTES` in-memory decode limit.
- Fix redb index compilation: remove unused `FileTree` import and iterate over entry slice directly.
- Fix redb index to use the same filtered file entries as the NIXI database so excluded prefixes are not cached.
- Apply `--hash` and `--package` filters to exact-path redb lookup results.
- Replace `unreachable!()` arms in `database.rs` with `Error::Corrupt` for unknown database versions.
- Replace `mapfile` in `command-not-found.sh` with a POSIX-friendly read loop so Zsh can load the hook without errors.
- Read interactive prompts from `/dev/tty` instead of stdin in `command-not-found.sh`.
- Validate the selected package number before passing it to `sed`.
- Leave the index fetch progress bar indeterminate because the closure fetcher discovers additional store paths as it runs.
- Add size validation to attrs sidecar reader to reject oversized files before allocation
- Fix path ordinal resolution to return empty bitmap instead of None for empty lookups
- Optimize path index prefix lookup to stop early when keys no longer match prefix
- Fix prebuilt cache key to use SHA256 digest instead of raw ETag/Last-Modified header values
- `nix-locate` output highlighting now reuses the same defensively size-limited regex used for the search, avoiding a second unbounded compilation.
- Fix package-ordinal tracking when scanning frcode blocks during v1 database searches so basename-prefix filters resolve to the correct packages instead of rejecting all candidates.
- `command-not-found.sh` and `nixdex command-not-found --auto-install` now detect `nix profile` via `$XDG_STATE_HOME/nix/profile/manifest.json` before falling back to `~/.nix-profile/manifest.json`.
- Switch `nixdex-core` database compression from multi-threaded `zstd::Encoder` to single-threaded `zstd::bulk::Compressor`. This removes the per-worker ~1 GiB memory allocation during index builds and drops peak RSS for large databases from ~11.7 GB to ~628 MB.
