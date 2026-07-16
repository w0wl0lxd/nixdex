# nixdex modernization roadmap 003 (2026-07-16)

Second follow-up to `research/modernization-2026-002.md`. This pass scans upstream
`nix-index` issues/PRs and the current `feat/p4-stretch` tree to identify the next
5–10 concrete features for the `feat/p5` batch.

## Methods

- Current tree: `nixdex` @ `feat/p4-stretch`.
- Tools: direct file reads of `crates/nixdex-cli/src/{index,locate,nixdex}.rs`,
  `crates/nixdex-core/src/{database,daemon,package_search}.rs`,
  `crates/nixdex-daemon/src/main.rs`, shell assets; `web_search` on
  `nix-index` releases, issues, and PRs.

## Verified gaps

| # | Feature | Motivation | Evidence |
|---|---------|------------|----------|
| 1 | `nix-locate --json`, `--limit`, `--count` | Upstream `nix-locate` has `--minimal` and `--color`; downstream tooling wants NDJSON, pagination, and fast counts. | `database.rs` `search()` only supports `Full`/`Minimal` text output. |
| 2 | `command-not-found.{sh,nu}` suggest `comma` | nix-index issue #286 / PR #314. Many users run `, <cmd>` once rather than installing. | Current scripts only suggest `nix shell` / `nix-shell`. |
| 3 | `nixdex daemon` subcommand | The multi-tool already wraps `index` and `locate`; `nixdex-daemon` is a separate binary users must discover. | `nixdex.rs` has no `Daemon` variant. |
| 4 | `nixdex-daemon` `/health` endpoint | Container/ systemd health checks need a cheap probe. | `daemon.rs` only exposes `/locate` and `/nix-locate`. |
| 5 | `nixdex-daemon` `/search` endpoint | The CLI `nixdex search` is useful; exposing it over HTTP lets shell/ editor integrations query package metadata without forking. | `package_search.rs` exists but daemon HTTP surface does not use it. |
| 6 | `nixdex-daemon` `/nix-locate` endpoint | `/locate` only maps basename → package list. A full `/nix-locate` query supports `regex`, `package`, `hash`, `type`, `at_root`, `whole_name`, `limit`, `json`. | `database.rs` `SearchOptions` already captures these flags; daemon just needs a handler. **Note:** already implemented in `feat/p4-stretch`. |
| 7 | `nixdex index generate-sidecars` | Upstream `nix-index-database` ships plain `files` DBs without nixdex sidecars. A subcommand to generate sidecars for a downloaded prebuilt DB enables fast basename/prefix queries. | `database::generate_sidecars` exists but is only called internally. |
| 8 | `nix-locate --min-size` / `--max-size` | Filesystem-adjacent queries often want to filter by file size (e.g. find large static libraries). | `SearchOptions` has `file_type` but no size bounds; `FileTreeEntry` carries size. |
| 9 | `nixdex-daemon` Prometheus-style `/metrics` | Operators running the daemon want request counts, refresh failures, and cache hit rates. | No metrics endpoint in `daemon.rs`. |
| 10 | `nix-index` `--exclude-prefix` (or multiple `--filter-prefix`) | Some users want to drop `/share/doc/` or `/lib/debug/` to shrink DBs or speed queries. | `filter_prefix` is a single string today. |

## Recommended P5 batch

Implement #1–#5 first; they are small, mostly orthogonal, and immediately user-facing.
#6 is the natural follow-up that reuses #1, but it is already present in `feat/p4-stretch`.
#7–#10 are larger or lower-leverage and should be deferred to P6 unless a specific
use-case arises.

## Acceptance

- `just validate` green.
- `just secrets` clean.
- New CLI flags appear in `--help`.
- New daemon endpoints respond with correct JSON and handle missing index gracefully.
