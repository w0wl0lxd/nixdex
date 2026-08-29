# Expand nixdex Features to Cover Outdated nix-locate Similar Tools

## Context

nixdex is a modern rewrite of `nix-index` / `nix-locate` — fast package file indexing and nix-locate-compatible search. It already replaces upstream nix-index, the Perl `command-not-found.pl` hook, and provides package metadata search, daemon mode, and HTTP query endpoints.

The nix ecosystem has several tools that are outdated, slow, clunky, or make nix hard to use. This plan identifies those tools and specifies which features to fold into nixdex (directly or as new `nixdex-*` crates) and which to leave out.

---

## Landscape: Tools That Are Outdated, Slow, or Clunky

| Tool | Approach | Why It's Outdated/Slow/Clunky | nixdex Coverage |
|------|----------|-------------------------------|-----------------|
| **upstream nix-index/nix-locate** | Rust, file index | No daemon, no fuzzy search, no batch mode, no JSON output, slow cold queries | **Already replaced** |
| **command-not-found.pl** | Perl, NixOS hook | Requires channels, slow, no flakes support, no auto-install | **Already replaced** by `nixdex command-not-found` |
| **nix-search-cli** (peterldowns) | HTTP to search.nixos.org | Network-dependent, no offline mode, parses HTML API | Partially covered by `nixdex search` (offline) |
| **nps** (OleMussmann) | `nix-env -qaP` cache | Slow indexing (`nix-env` evaluation), no file search, no version history | `nixdex search` is faster; version history missing |
| **rippkgs** (Replit) | SQLite, nixpkgs eval | Separate index step, no file search, no daemon, no HTTP API | `nixdex search` covers package search; file search + daemon + API missing |
| **spam** (feel-co) | Nim, packages + NixOS options | Requires Nim + libzstd, no daemon, no HTTP API, no version history | NixOS options search missing; file search + daemon + API covered |
| **nxv** (utensils) | SQLite FTS5, version history, HTTP API | Separate tool, no file search, no nix-locate compatibility | Version history + HTTP API missing; file search covered |
| **searchix** | Web-based, multi-source | Web-only, no CLI, no offline mode, no file search | Multi-source search missing |
| **nixard** (manelinux) | TUI, closure analysis | Interactive TUI only, no CLI, no file search, no daemon | Out of scope (different tool category) |

---

## Scope Decision

**In scope** (justifiably relates to nixdex's core scope of file indexing + nix-locate-compatible search + package metadata):

1. **Package version history** — extends the indexing/searching capabilities to cover when packages existed (nxv-like)
2. **NixOS module options search** — extends metadata search to cover NixOS options (spam-like)
3. **HTTP API server** — extends the daemon to serve queries programmatically (nxv-like)
4. **Enhanced batch/streaming search** — better support for scripting and large result sets

**Out of scope** (different tool category, no justifiable relation):

- Interactive TUI (nixard) — different paradigm, not a CLI search tool
- Web UI (searchix) — not a CLI tool
- Multi-source search aggregation (searchix) — requires external API integration, out of scope for a local indexing tool

---

## Feature Plan

### 1. Package Version History (`nixdex-crates/nixdex-history`)

**Problem**: nxv provides version history but is a separate tool with its own index. Users wanting to know when a package version existed or what versions are available must use a different tool.

**Approach**: Create a new `nixdex-history` crate that extends the `packages.json` sidecar with version history data. During `nixdex update` (prebuilt index download), also download the version history sidecar. During `nixdex index`, extract version info from derivation names and store it.

**New crate**: `crates/nixdex-history/`
- Reads `packages.json` sidecar and augments it with version history
- Provides `nixdex history <attr>` subcommand showing when versions existed
- Provides `nixdex search --history` to show version info in search results
- Uses the same `packages.json` NDJSON format with an additional `versions` field

**Key types** (in new crate):
- `VersionHistory` — maps attribute path to list of `(version, commit, date)` entries
- `VersionEntry` — single version record with version string, nixpkgs commit, and date
- `HistoryDb` — opens and queries the version history sidecar

**Integration points**:
- `nixdex update` downloads version history alongside the prebuilt `files` database
- `nixdex index` extracts version info from derivation names during indexing
- `nixdex search` can show version info when `--history` flag is used
- `nixdex info` shows version history for a single attribute

### 2. NixOS Module Options Search (`nixdex-crates/nixdex-options`)

**Problem**: spam provides NixOS module options search but is a separate Nim-based tool. NixOS users frequently need to search module options alongside package search.

**Approach**: Create a new `nixdex-options` crate that indexes NixOS module options and provides a search CLI. This is a natural extension of nixdex's metadata search capabilities.

**New crate**: `crates/nixdex-options/`
- Downloads and indexes NixOS module options from the nixpkgs repository
- Provides `nixdex options <pattern>` subcommand for searching module options
- Supports the same search modes as `nixdex search` (regex, fuzzy, field selection)
- Stores options in a sidecar file (`options.json`) alongside the database

**Key types** (in new crate):
- `OptionRecord` — attribute path, type, description, default value, example
- `OptionsDb` — opens and queries the options sidecar
- `OptionsIndex` — builds the options index from nixpkgs

**Integration points**:
- `nixdex index` can optionally build the options sidecar with `--options` flag
- `nixdex update` downloads options sidecar alongside prebuilt index
- `nixdex options` subcommand searches module options
- `nixdex search --field options` extends the existing search to include options

### 3. HTTP API Server (`nixdex-crates/nixdex-api`)

**Problem**: nxv provides an HTTP API for programmatic access, but nixdex's daemon only serves `nix-locate` queries. Users and tools that need programmatic access to package metadata, version history, or options search cannot use nixdex.

**Approach**: Extend the existing `nixdex-daemon` to serve additional HTTP endpoints for search, info, stats, version history, and options queries.

**Changes to existing crate**: `crates/nixdex-daemon/`
- Add `/search` endpoint for package metadata search (mirrors `nixdex search`)
- Add `/info` endpoint for single-attribute lookup (mirrors `nixdex info`)
- Add `/history` endpoint for version history (mirrors `nixdex history`)
- Add `/options` endpoint for NixOS options search (mirrors `nixdex options`)
- Add `/stats` endpoint for database statistics (already partially exists)
- Add `/locate` endpoint already exists; keep as-is
- All endpoints return JSON
- Support query parameters for search options (pattern, regex, field, sort, limit, etc.)

**Key changes**:
- Extend `DaemonConfig` to include options for enabling/disabling endpoints
- Add new route handlers in `daemon.rs`
- Reuse existing `SearchDb`, `HistoryDb`, `OptionsDb` types

### 4. Enhanced Batch/Streaming Search

**Problem**: `nixdex search` already supports batch mode via stdin, but the output is not streamable for large result sets. Tools like rippkgs handle streaming output well.

**Approach**: Enhance the existing `nixdex search` and `nixdex locate` commands to support streaming output with `--stream` flag that flushes after each result. Also add `--format` flag for output format selection (table, json, ndjson, csv).

**Changes to existing crates**:
- `crates/nixdex-cli/src/locate.rs` — add `--stream` flag to `Opts`
- `crates/nixdex-cli/src/nixdex.rs` — add `--stream` and `--format` flags to `SearchOpts`
- `crates/nixdex-core/src/database.rs` — add `stream` mode to `SearchOptions`

---

## Crate Structure

```
crates/
  nixdex/           # Umbrella crate (existing)
  nixdex-core/      # Core library (existing)
  nixdex-cli/       # CLI tools (existing)
  nixdex-daemon/    # Background daemon (existing)
  nixdex-history/   # NEW: package version history
  nixdex-options/   # NEW: NixOS module options search
  nixdex-api/       # NEW: HTTP API server (or merge into nixdex-daemon)
```

**Decision**: Merge `nixdex-api` into `nixdex-daemon` rather than creating a separate crate, since the API server is a natural extension of the daemon and shares the same HTTP infrastructure (axum). This avoids duplication and keeps the daemon as the single entry point for both background indexing and programmatic queries.

---

## Implementation Order

1. **nixdex-history** — version history is the highest-value addition (nxv-like feature, no existing replacement in nixdex)
2. **nixdex-options** — NixOS options search extends the metadata search naturally
3. **nixdex-daemon API extensions** — add HTTP endpoints for search, info, history, options
4. **Enhanced batch/streaming** — polish the existing CLI with streaming and format options

---

## Risks and Open Questions

1. **Version history data source**: nxv uses Hydra channel-release snapshots from releases.nixos.org. nixdex would need a similar data source or could extract version info from derivation names during indexing. The latter is simpler but less complete.
2. **Options index size**: NixOS module options could be large. Need to estimate the sidecar size and decide on compression strategy.
3. **API authentication**: The daemon already has `--admin-token` for `/reload`. Should `/search`, `/history`, etc. also require authentication, or be open?
4. **Prebuilt index compatibility**: Version history and options sidecars need to be distributed alongside prebuilt indexes. Need to coordinate with nix-index-database releases.
5. **Scope creep**: Adding too many features could dilute nixdex's focus on file indexing and nix-locate-compatible search. Each new feature should be justified by a specific tool it replaces.

---

## Validation Plan

1. Run `cargo check --all-features` and `cargo clippy --all-features` to verify compilation
2. Run `cargo nextest run --workspace` to verify existing tests pass
3. For `nixdex-history`: verify version history sidecar generation and query with a real database
4. For `nixdex-options`: verify options index build and query with a real database
5. For `nixdex-daemon` API: verify all new endpoints return correct JSON responses
6. For batch/streaming: verify streaming output works with large result sets
7. Run `just validate` (fmt + check + clippy + test + changelog-check) before merging
