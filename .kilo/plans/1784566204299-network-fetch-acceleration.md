# Network-fetch acceleration for nixdex

Date: 2026-07-20. Based on July-2026 research (reqwest 0.13 docs, aria2/axel
range-download technique, FastCDC/zsync/casync delta literature, arxiv
2409.06066 "Content-Defined Chunking Algorithms" + 2508.05797 "VectorCDC").

## Problem

Two network-fetch paths are bottlenecks (benchmark report 2026-07-18):

1. **Prebuilt index download** (`prebuilt.rs::download_to`) — a single serial
   `GET` + `.chunk()` loop. No HTTP Range, no parallelism, no resume.
   `~95 MB` from the GitHub releases CDN (CloudFront/S3 → **supports Range**).
2. **Index build** `.ls`/`.narinfo` fetch (`hydra.rs` + `listings.rs`) — already
   parallelized via the `jobs` semaphore, but the `reqwest` client uses default
   pool settings (`pool_max_idle_per_host = 2`), throttling concurrency, and
   does not enable `http2_adaptive_window` / `tcp_nodelay`.

Goal: drastically cut wall-clock network time, client-only (no upstream
cooperation required for Tiers 1–2).

## Decisions (grounded in 2026 research)

- **Tier 1 — Parallel segmented Range download (prebuilt).** Split the file
  into N segments, download each via a concurrent `Range` GET streamed to its
  own temp part file, concatenate, validate, atomic rename. reqwest supports
  `Range` via `.header()` and body streaming via `Response::chunk()`
  (context7 confirmed; no new deps). aria2/axel show near-linear speedup to
  bandwidth saturation. GitHub CDN honors `bytes=` ranges (206).
- **Tier 2 — Resumable + conditional.** Keep a per-run fallback to the serial
  path when the server does not return `206` (or `Content-Length` is unknown /
  file is small). Daemon already gates on ETag (`daemon.rs::should_download_index`),
  so unchanged releases are skipped; extend the download to honor `If-Range` for
  partial resume in a later iteration (out of scope for first cut).
- **Tier 3 — CDC delta (roadmap, cross-repo).** A client-only rsync/zsync over
  the current single zstd frame is ineffective (compression destroys content
  locality), so it needs a server-published FastCDC+BLAKE3 chunk manifest
  (casync/zchunk-style; `fastcdc` 4.0.1 / `chunkrs` 0.9.0 available). nixdex
  would then fetch only missing chunks by hash via Range. **This plan implements
  Tiers 1–2 only; Tier 3 is documented as a follow-up requiring nix-index-database
  cooperation.**
- **HTTP/2 tuning (build path).** In `hydra.rs::FetcherBuilder::build`, enable
  `http2_adaptive_window(true)`, `tcp_nodelay(true)`,
  `pool_max_idle_per_host(32)`. Reduces connection overhead for the many small
  `.ls` requests. (`http3`/QUIC is experimental in reqwest 0.13 and left as a
  stretch: needs `--cfg reqwest_unstable` + `http3` feature + CDN support.)

## Implementation (worktree branch `net-fetch-accel`)

### `crates/nixdex-core/src/prebuilt.rs`
- Add `max_connections: usize` to `PrebuiltConfig` (default `8`).
- Add `fn build_client(timeout: Duration) -> Result<reqwest::Client>` applying
  `user_agent`, `timeout`, `http2_adaptive_window(true)`, `tcp_nodelay(true)`,
  `pool_max_idle_per_host(32)`. Replace the three inline client builds
  (`check_update`, `download_and_validate`, `download_to`) with calls to it.
- Add `fn plan_segments(total: u64, max_conn: usize) -> Vec<(u64, u64)>` returning
  inclusive `(start, end)` byte ranges. Pure, unit-tested.
- Rewrite `download_to`:
  1. `HEAD` to obtain `Content-Length`. If absent or `< 1 MiB` → serial path.
  2. `plan_segments` → N ranges.
  3. Probe segment 0: if response status `!= 206` → remove parts, serial fallback.
  4. Else spawn N−1 remaining segment downloads concurrently (`tokio::task::spawn`),
     each streaming `Range` bytes into `dest.part-{i}` via `Response::chunk()`.
  5. Join; on any error → cleanup parts, serial fallback.
  6. Concatenate part files into `dest.tmp` (`tokio::io::copy`), remove parts,
     `validate_nixi`, atomic `rename`, then `generate_sidecars` (existing,
     `spawn_blocking`).
  - Serial fallback preserves the existing `.chunk()` loop exactly.

### `crates/nixdex-core/src/hydra.rs`
- In `FetcherBuilder::build`, chain `.http2_adaptive_window(true)`
  `.tcp_nodelay(true)` `.pool_max_idle_per_host(32)` on the client builder.

### Tests
- `prebuilt.rs`: unit tests for `plan_segments` (exact size = whole file,
  small < MIN → single range, multi-segment boundary alignment, last range open-ended).
- `prebuilt.rs`: a `tower`-based mock server (dev-dep already present) that
  honors `Range` and returns `206`; assert parallel download reconstructs the
  original bytes and fails over to serial when the server ignores `Range`
  (returns `200`).

## Risks / non-goals
- Servers without Range support → automatic serial fallback (no regression).
- Rate-limiting from many connections: default `max_connections = 8`, tunable;
  documented.
- No `unwrap`/`expect`/`HashMap`; use `scc`/`ahash`/errors as in repo rules.
- Tier 3 (CDC delta) explicitly out of scope for this change.
## Validation

- `cargo check -p nixdex-core` (and with `--features prebuilt,daemon`).
- `cargo test -p nixdex-core --features prebuilt` (new segmentation + mock-server
  range tests; existing `validate_nixi` tests still pass).
- Manual: `nixdex` prebuilt download with `RUST_LOG=debug` to confirm N concurrent
  range requests; compare wall time vs serial on the ~95 MB asset.

## Status — follow-up phases completed

### Tier 2 deferred item: resumable partial download (DONE)
- `try_segmented_download` now keeps per-segment part files between attempts and
  retries only the still-missing segments (up to `MAX_SEGMENT_ATTEMPTS = 3`) instead
  of restarting the whole file. It sends `If-Range: <etag>` (ETag taken from the
  initial `HEAD`) so a changed resource makes the server answer `200`, which aborts
  the resume and falls back to the serial path (fresh full download). Transient
  errors (`503`/timeouts/`5xx`) only retry the missing parts.
- Tests: `segmented_download_resumes_after_transient_failure` (mock fails one
  non-probe segment once with `503`, download still reconstructs).

### Tier 3 client foundation: CDC chunking (DONE, server cooperation still required)
- New feature-gated module `crates/nixdex-core/src/cdc.rs` (`feature = "cdc"`,
  deps `fastcdc` 3.2.1 + `blake3` 1) implementing the client side of delta sync:
  - `CdcConfig` + `Chunk` (offset/length/BLAKE3 `[u8;32]`) + `ChunkManifest`.
  - `chunk_bytes` / `chunk_file` / `build_manifest` — FastCDC content-defined
    cutting with BLAKE3 per-chunk fingerprints (deterministic, server-independent).
  - `verify_manifest` (hash check), `reconstruct` (reassemble from chunk payloads
    by hash), and postcard (de)serialization of the manifest.
- This is enough for a client to fetch only missing chunks *by hash* once an
  upstream `nix-index-database` release publishes a CDC+BLAKE3 chunk manifest and
  serves chunks via `Range`. **Server-side manifest publishing + chunk store are
  out of scope and require nix-index-database cooperation** — that remains the only
  unimplemented part of Tier 3.
- Tests: deterministic chunking, contiguity/coverage, length bounds, manifest
  round-trip, corruption detection, and full reconstruct.
