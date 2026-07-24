# nixdex Audit vs nix-locate and Related Tools

## Purpose

Exhaustive gap analysis of the nixdex codebase against upstream `nix-index`/`nix-locate` and related tools (`nix-search-cli`, `search-nix`, `comma`, `nix-index-database`). The goal is to identify all gaps in performance, UI, UX, devex, installation, dropin support, configuration, and benchmark competitiveness so they can be addressed systematically.

---

## 1. What nixdex Already Has (vs Upstream nix-index/nix-locate)

nixdex is a **modern rewrite** of nix-index/nix-locate and already exceeds the upstream in several areas:

- Drop-in `nix-locate` and `nix-index` binaries (compatible v1 DB format)
- V2 database format with per-CPU frame grouping
- Secondary indexes: basename FST, path FST, entry index, ngram index, path trigram index, path entry index, command index
- Prebuilt index download with auto-refresh
- Resident daemon with LRU cache mode
- `nixdex` umbrella CLI with 14 subcommands (search, info, stats, completions, man, index, locate, which, update, generate-sidecars, command-not-found, daemon)
- Fuzzy search via frizbee (SIMD)
- Package metadata search (`packages.json` sidecar)
- Command-not-found integration with `--auto-install`, `--auto-run`, `--interactive`
- Shell integration scripts (bash, zsh, fish, nushell)
- NixOS and Home Manager modules
- Color/JSON/count/limit/sort/min-size/max-size/exclude-fhs output controls
- `--no-daemon` fallback for locate
- Path cache for incremental index builds
- Redb exact-path sidecar
- `--small` flag for `/bin/`-filtered database
- `--no-closure`, `--no-main-program`, `--no-overlays` flags
- `--select`, `--only-eval`, `--extra-scopes` for nix-eval-jobs
- `--filter-prefix` / `--exclude-prefix` for path filtering
- `--cache-url` / `--substituter` for binary cache URL
- `--prebuilt-*` flags for prebuilt index downloads
- `--format-version` for v1/v2 DB compatibility
- `--chunk-size` for v2 frame flushing control
- `--compression-level` (1–22)
- `--requests`, `--timeout`, `--retries` for HTTP tuning
- `--index-cache-mode` (resident/lru) for daemon
- `--admin-token` for daemon security
- `NIX_INDEX_DATABASE`, `NIXDEX_DATABASE`, `NIXDEX_DAEMON_ADDR`, `NIXDEX_NO_DAEMON`, `NIXDEX_ADMIN_TOKEN` env vars
- `NIX_AUTO_INSTALL`, `NIX_AUTO_RUN`, `NIX_AUTO_RUN_INTERACTIVE` env vars for command-not-found
- Benchmark scripts (locate, search, index, index-comparison)
- Comprehensive test suite with differential sidecar/pruning tests

---

## 2. Identified Gaps

### 2.1 Performance Gaps

| Gap | Description | Priority |
|-----|-------------|----------|
| No batch/bulk locate | No `--batch` or `--bulk` mode to process multiple queries in a single invocation (avoids repeated DB open overhead) | High |
| No streaming locate output | No `--stream` mode to emit results as they're found instead of buffering all in memory | Medium |
| No query result caching | No `--cache` or `--prefetch` option to cache frequent query results | Medium |
| No locate timeout | No `--timeout` option for locate queries (daemon has 10s spawn timeout but no query timeout) | Low |
| No parallel locate | No `--jobs`/`--parallel` flag for the locate command (only index has parallelism) | Medium |
| No progress indicator | No `--progress` option for long-running locate queries on large databases | Low |
| No verbose/debug mode | No `--verbose` or `--debug` flag for the locate command to aid performance tuning | Low |
| No benchmark built-in | No `--benchmark` flag to self-measure query performance | Low |
| No mmap control | No `--no-mmap` or `--mmap` option to control memory mapping behavior for the DB reader | Low |
| No read buffer tuning | No `--buffer-size` or `--read-buffer` option for controlling I/O buffering | Low |
| Daemon cold-start penalty | First query after daemon spawn pays full index load cost; no warm-ping or preload mechanism | Medium |
| No query result pagination | No cursor-based or offset-based pagination for large result sets beyond `--limit` | Medium |
| No async I/O for local search | The local search path (when daemon is disabled) uses synchronous I/O; could benefit from async | Low |

### 2.2 UI/UX Gaps

| Gap | Description | Priority |
|-----|-------------|----------|
| No TUI mode | No interactive text UI like `search-nix --tui` for the locate or search commands | High |
| No interactive locate | No interactive mode for the locate command (like `fzf` integration) | Medium |
| No `--no-color` alias | `--color=never` works but `--no-color` is not documented as an alias (common convention) | Low |
| No `--details`/`--verbose` output | No expanded detail mode for locate results showing package description, license, homepage | Medium |
| No table/CSV/TSV output | No structured tabular output formats for locate results (only human-readable and JSON) | Medium |
| No `--print0` / `--null` output | No null-delimited output mode for shell scripting safety (like `find -print0`) | Medium |
| No `--yaml` output | No YAML output format for locate results | Low |
| No `--markdown` output | No Markdown output format for locate results | Low |
| No column width control | No `--col-width` or `--width` option to control output column widths | Low |
| No result grouping control beyond `--no-group` | No `--group-by` option to group by attribute, size range, etc. | Low |
| No highlight control | No `--no-highlight` option to disable regex highlighting in output | Low |

### 2.3 Devex / Scripting Gaps

| Gap | Description | Priority |
|-----|-------------|----------|
| No `--lib` / `--library` mode | No library-mode output for programmatic consumption (e.g., just store paths) | Medium |
| No `--pipe` / `--pipeable` mode | No pipe-friendly output mode (like `nix-env -qP` pipe compatibility) | Medium |
| No `--json` with full metadata | JSON output for locate does not include package description, license, homepage, maintainers | High |
| No `--json` with package fields | JSON output for locate does not include `mainProgram`, `description`, `license`, `homepage`, `platforms` | High |
| No `--json` with store path details | JSON output for locate does not include the full store path derivation info | Medium |
| No `--output-format` | No unified `--output-format` option supporting `text`, `json`, `ndjson`, `csv`, `tsv`, `yaml` | Medium |
| No `--null` output delimiter | No `--print0` or `--null` delimiter for JSON lines output for safe shell parsing | Medium |
| No scripting-friendly exit codes | Locate exits 0 even when no matches found (should arguably exit 1 or have a `--strict` mode) | Medium |
| No `--version` output format control | `--version` only prints version string, no machine-parseable format | Low |
| No `--help-format` option | No option to output help in machine-parseable format (JSON schema, man page source) | Low |

### 2.4 Installation / Dropin Support Gaps

| Gap | Description | Priority |
|-----|-------------|----------|
| No `--install` / `--auto-install` for locate | No flag to automatically install the found package (like `nix-locate --install bin/foo`) | High |
| No `--run` / `--auto-run` for locate | No flag to automatically run the found command (like `nix-locate --run bin/foo`) | High |
| No `--shell` / `--shell-run` for locate | No flag to run in a temporary nix shell (like `nix-locate --shell bin/foo`) | High |
| No `--profile` / `--add-to-profile` | No flag to add the found package to the user's nix profile | Medium |
| No `--environment` / `--add-to-environment` | No flag to add to NIX_PATH or environment | Medium |
| No `--generation` / `--add-to-generation` | No flag to add to a specific nix profile generation | Low |
| No `--shell-hook` integration | No shell hook integration for locate (like `nix-index` command-not-found.sh) | Medium |
| No `NIX_PATH` auto-update | No mechanism to auto-update NIX_PATH when a new package is installed via locate | Low |
| No `PATH` auto-update | No mechanism to auto-update PATH when a new package is installed via locate | Low |
| No fhs-style package exclusion by default | `--exclude-fhs` is opt-in; no default exclusion for FHS packages in locate | Low |
| No `--only-eval` for locate | No way to do a metadata-only lookup without full path search | Low |

### 2.5 Configuration Gaps

| Gap | Description | Priority |
|-----|-------------|----------|
| No `--config` / `--config-file` | No config file support for any command (all configuration is CLI flags and env vars) | High |
| No `--defaults` / `--show-defaults` | No option to show current default values for all options | Medium |
| No `--dump-config` | No option to dump the effective configuration (including env vars and defaults) | Medium |
| No config file precedence docs | No documentation of config file precedence (env vars vs CLI flags vs config file) | Low |
| No XDG config dir support | No use of `$XDG_CONFIG_HOME` for config file location | Low |
| No config schema validation | No validation of config file schema | Low |

### 2.6 Missing Features vs nix-search-cli / search-nix

| Gap | Description | Priority |
|-----|-------------|----------|
| No `--channel` selection | `nixdex search` only searches local `packages.json`; no channel selection (unstable, 25.11, etc.) | High |
| No `--flakes` search | No ability to search flake-indexed packages | High |
| No `--program` search | No ability to search by installed program name (like `nix-search --program python`) | High |
| No `--version` search | No ability to search by version constraint | High |
| No `--query-string` / ES syntax | No Elasticsearch-style query string syntax for advanced searches | Medium |
| No `--reverse` order | No reverse sort order for search results | Low |
| No `--name` field search | No dedicated `--name` flag to search only by package name (vs `--field` which is more general) | Low |
| No `--exclude` / `--exclude-regex` | No exclusion pattern for search results | Medium |
| No `--details` / `--expanded` output | No expanded detail output showing full metadata for each result | Medium |
| No TUI for search | No interactive TUI mode for the `nixdex search` command | High |
| No channel auto-detection | No auto-detection of channel from nixos-version (like search-nix does) | Medium |
| No `--size` / `--size-limit` | No size-based filtering for search results | Low |
| No online search integration | No ability to query search.nixos.org Elasticsearch backend directly | High |
| No `--max-results` alias | `--limit` works but `--max-results` is not an alias (inconsistent with nix-search-cli) | Low |

### 2.7 Missing Features vs nix-index Upstream

| Gap | Description | Priority |
|-----|-------------|----------|
| No `--top-level` flag alias | Upstream v0.1.9+ made `--top-level` the default and added `--all` to restore old behavior; nixdex only has `--all` | Low |
| No `--no-group` in LONG_USAGE | `--no-group` flag exists but is not documented in the long help text | Low |
| No `--hash` in LONG_USAGE | `--hash` flag exists but is not documented in the long help text | Low |
| No `--package` in LONG_USAGE | `--package` flag exists but is not documented in the long help text | Low |
| No `--type` in LONG_USAGE | `--type` flag exists but is not documented in the long help text | Low |
| No `--regex` in LONG_USAGE | `--regex` flag exists but is not documented in the long help text | Low |
| No `--whole-name` in LONG_USAGE | `--whole-name` flag exists but is not documented in the long help text | Low |
| No `--at-root` in LONG_USAGE | `--at-root` flag exists but is not documented in the long help text | Low |
| No `--minimal` in LONG_USAGE | `--minimal` flag exists but is not documented in the long help text | Low |
| No `--color` in LONG_USAGE | `--color` flag exists but is not documented in the long help text | Low |
| No `--limit` in LONG_USAGE | `--limit` flag exists but is not documented in the long help text | Low |
| No `--count` in LONG_USAGE | `--count` flag exists but is not documented in the long help text | Low |
| No `--sort` in LONG_USAGE | `--sort` flag exists but is not documented in the long help text | Low |
| No `--min-size` in LONG_USAGE | `--min-size` flag exists but is not documented in the long help text | Low |
| No `--max-size` in LONG_USAGE | `--max-size` flag exists but is not documented in the long help text | Low |
| No `--exclude-fhs` in LONG_USAGE | `--exclude-fhs` flag exists but is not documented in the long help text | Low |
| No `--no-daemon` in LONG_USAGE | `--no-daemon` flag exists but is not documented in the long help text | Low |

### 2.8 Benchmark Competitiveness Gaps

| Gap | Description | Priority |
|-----|-------------|----------|
| No cold-start benchmark | Benchmark scripts don't measure cold-start (first query after daemon restart) vs warm query latency | High |
| No memory usage benchmark | No benchmark measuring memory footprint of locate vs nix-locate | High |
| No DB size comparison | No benchmark comparing nixdex DB size vs upstream nix-index DB size | Medium |
| No index build throughput | No benchmark comparing index build throughput (packages/sec) vs upstream | High |
| No sidecar overhead measurement | No benchmark measuring the overhead of secondary indexes on query latency | Medium |
| No concurrent query benchmark | No benchmark measuring concurrent query throughput (multiple simultaneous locate calls) | Medium |
| No daemon vs local comparison | Benchmark scripts don't explicitly compare daemon mode vs local search mode latency | Medium |
| No v1 vs v2 format comparison | No benchmark comparing v1 vs v2 database format query performance | Medium |
| No prebuilt vs local comparison | No benchmark comparing prebuilt index download + query vs local index build + query | Medium |
| No p99 latency tracking | Benchmark scripts use hyperfine's mean but don't track p99 latency specifically | Low |

### 2.9 DevOps / DX Gaps

| Gap | Description | Priority |
|-----|-------------|----------|
| No `--quiet` / `--silent` mode | No flag to suppress all non-error output (useful for scripting) | Medium |
| No `--log-format` option | No option to control log output format (JSON, text, etc.) for daemon and index commands | Low |
| No `--log-file` option | No option to write logs to a file instead of stderr | Low |
| No `--metrics` endpoint | No Prometheus/OpenMetrics endpoint on the daemon for query latency metrics | Medium |
| No `--health` endpoint | No `/health` HTTP endpoint on the daemon (only `/ready`) | Low |
| No `--reload` without SIGHUP | Daemon reload requires `POST /reload` with admin token; no SIGHUP-based reload | Low |
| No `--version` JSON output | `--version` only outputs plain text; no machine-parseable version format | Low |
| No `--license` output | No option to output the license information | Low |
| No `--contributors` output | No option to output contributor information | Low |
| No `--changelog` output | No option to output changelog for the current version | Low |

### 2.10 Installation / Packaging Gaps

| Gap | Description | Priority |
|-----|-------------|----------|
| No standalone `nixdex` package in nixpkgs | nixdex is not in nixpkgs; users must use `nix run github:w0wl0lxd/nixdex` or build from source | High |
| No `nix-locate` wrapper in nixpkgs | The `nix-locate` binary is not wrapped as a standalone package in nixpkgs | Medium |
| No `nix-index` wrapper in nixpkgs | The `nix-index` binary is not wrapped as a standalone package in nixpkgs | Medium |
| No `nixdex-daemon` systemd service | No systemd service file for the daemon (only manual invocation) | Medium |
| No `nixdex-daemon` socket activation | No systemd socket activation for the daemon | Low |
| No `nixdex-daemon` timer for auto-refresh | No systemd timer for automatic prebuilt index refresh | Medium |
| No `NIXDEX_DATABASE` env var in nixos module | The NixOS module sets `NIXDEX_DATABASE` but it's not documented as the primary env var for locate | Low |
| No `NIX_INDEX_DATABASE` env var in nixos module | The NixOS module doesn't set `NIX_INDEX_DATABASE` for upstream compatibility | Low |
| No `command-not-found` integration in nixos module | The NixOS module doesn't integrate with the system `command-not-found` mechanism | Medium |
| No `comma`-like tool | No tool to spawn ephemeral shells with found packages (like `comma` does with nix-index) | High |
| No `nix-locate` as a shell function | No shell function wrapper for `nix-locate` that auto-spawns the daemon | Medium |
| No `nix-index` as a shell function | No shell function wrapper for `nix-index` that auto-manages the daemon | Medium |

---

## 3. Benchmark Competitiveness Assessment

nixdex already has several performance advantages over upstream nix-locate:
- Secondary indexes (FST-based basename, path, ngram) for sub-millisecond lookups
- Resident daemon keeps indexes warm for sub-100ms queries
- mmap-backed database reader
- Per-CPU frame grouping for parallel decompression
- zstd frame-level streaming for memory-efficient decompression
- Trigram inverted index for fast literal substring queries

However, nixdex has not been benchmarked against:
1. Upstream `nix-locate` with a cold DB (no daemon)
2. Upstream `nix-locate` with a warm DB (daemon running)
3. `nix-search-cli` for package metadata search
4. `search-nix` for package metadata search
5. `nix-env -qaP` for package name search
6. `nix search` for flake-based package search

The existing benchmark scripts (`benchmark-locate.sh`, `benchmark-search.sh`, `benchmark-index.sh`, `benchmark-index-comparison.sh`) provide a foundation but need expansion to cover the gaps identified above.

---

## 4. Recommended Action Plan

### Phase 1: Critical Gaps (High Priority)
1. Add `--batch` / `--bulk` mode for locate (process multiple queries from stdin or file)
2. Add `--install`, `--run`, `--shell` flags to locate for auto-install/auto-run/auto-shell
3. Add `--channel` / `--flakes` / `--program` / `--version` search options to `nixdex search`
4. Add TUI mode for `nixdex search` (like `search-nix --tui`)
5. Add `--config` / `--config-file` support for all commands
6. Add JSON output with full package metadata for locate (`--json` should include description, license, homepage, etc.)
7. Add `--print0` / `--null` output mode for locate
8. Add nixpkgs packaging for `nixdex`, `nix-locate`, `nix-index` binaries
9. Add `command-not-found` integration to NixOS module
10. Add comma-like ephemeral shell tool

### Phase 2: Important Gaps (Medium Priority)
11. Add cold-start vs warm query latency benchmarks
12. Add memory usage benchmarks
13. Add DB size comparison benchmarks
14. Add index build throughput benchmarks
15. Add concurrent query throughput benchmarks
16. Add `--details` / `--verbose` output mode for locate
17. Add `--table` / `--csv` / `--tsv` output formats
18. Add `--exclude` / `--exclude-regex` for search
19. Add `--query-string` / ES syntax for search
20. Add `--quiet` / `--silent` mode
21. Add daemon metrics endpoint (Prometheus)
22. Add systemd service and timer for daemon
23. Add `NIX_INDEX_DATABASE` env var to NixOS module
24. Add shell function wrappers for `nix-locate` and `nix-index`

### Phase 3: Nice-to-Have Gaps (Low Priority)
25. Add `--no-color` alias
26. Add `--yaml` / `--markdown` output formats
27. Add `--no-mmap` option
28. Add `--buffer-size` option
29. Add `--version` JSON output
30. Add `--license`, `--contributors`, `--changelog` output
31. Add XDG config dir support
32. Add config schema validation
33. Add SIGHUP-based daemon reload
34. Add `/health` HTTP endpoint on daemon
35. Add `--top-level` flag alias for locate

---

## 5. Validation Plan

1. Run `cargo test --all` to verify no regressions
2. Run `cargo clippy --all` to verify lint compliance
3. Run `cargo fmt --all -- --check` to verify formatting
4. Run `just validate` (existing CI command)
5. Run benchmark scripts against upstream nix-locate and nix-index
6. Verify dropin compatibility: `nixdex locate` output matches `nix-locate` output for identical queries
7. Verify v1 DB format compatibility: nixdex can read upstream prebuilt v1 databases
8. Verify v2 DB format compatibility: nixdex can read its own v2 databases
9. Verify sidecar compatibility: nixdex sidecars work with upstream nix-index databases
10. Verify prebuilt index compatibility: nixdex can download and use upstream prebuilt indexes
