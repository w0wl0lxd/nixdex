# nixdex modernization roadmap 002 (2026-07-16)

Evidence-backed expansion of `research/modernization-2026.md` after Wave A
(recursive closure traversal + hydra resilience) and Wave B
(`meta.mainProgram` synthetic `/bin/<mainProgram>` listings) were merged to
`main`. Claims are tagged **Verified** (primary source read), **Reported**
(credible secondary), or **Unverified**.

## 0. Methods and scope

- Research date: **2026-07-16**.
- Current tree: `nixdex` @ `732ce1d`.
- Tools used: `codegraph` (repo symbol/call-graph queries), `exa` (upstream
  commits, PRs, releases, ecosystem pages), `context7` (crate docs for `fst`,
  `roaring`, `zstd`, `redb`, `bincode`, `rkyv`, `tokio`), `thoughtbox`
  (structured synthesis of findings), `arxiv` (academic grounding on Roaring
  bitmaps / compressed string dictionaries), direct file reads.

## 1. Executive delta from modernization-2026-001

What changed since the first roadmap:

| Capability | 001 status | 002 status |
|------------|------------|------------|
| NIXI v2 multi-frame **read** | P0 planned | **Implemented** (`database.rs` supports v1/v2) |
| NIXI v2 multi-frame **write** | P0 planned | **Implemented** (`Writer::do_finish` v2 branch) |
| Basename FST + postings sidecar | P1 planned | **Built and wired to `nix-locate`** |
| Recursive closure traversal | P2 planned | **Implemented** (`listings.rs`) |
| Hydra retries / `.ls` zstd+xz sniffing | P2 planned | **Implemented** (`hydra.rs`) |
| `meta.mainProgram` synthesis | Not listed | **Implemented** (`nixpkgs.rs`, `listings.rs`) |
| `--path-cache` | P2 planned | Still **stub** (`Error::NotImplemented`) |
| `nixdex-daemon` | P3 planned | Still **scaffold** (one-shot log/sleep) |
| Selective frame decompression | Not listed | Identified as **highest-leverage next win** |

**Verified** from direct `read` of `database.rs`, `basename_index.rs`,
`listings.rs`, `hydra.rs`, `nixpkgs.rs`, `index.rs`, and `nix-locate.rs`.

## 2. Current nixdex state

### 2.1 Crate responsibilities

| Crate | Role |
|-------|------|
| `nixdex-core` | Database format, search, index build, evaluation, fetch |
| `nixdex-cli` | `nix-index` and `nix-locate` CLIs (clap) |
| `nixdex-daemon` | Stub binary around `nixdex_core::daemon::run` |

### 2.2 On-disk artifacts

`nixdex` writes the same `files` blob as upstream, plus three sidecars in the
same directory:

| File | Format | Purpose |
|------|--------|---------|
| `files` | NIXI v2: `NIXI` magic + `u64` LE version + concatenated independent zstd frames + trailing skippable seek table | Primary frcode stream |
| `files.basename.fst` | `fst::Map` (byte key → `u64` cookie) | Exact-basename lookup |
| `files.basename.postings` | Magic + version + packed `(count, [u32 ordinal...])*` | Cookie → package ordinals |
| `files.packages.names` | Magic + version + length-prefixed strings | Ordinal → `attr.output` label |

`Writer::do_finish` (v2) splits the frcode stream into one frame per CPU,
compresses frames in parallel with `rayon`, then appends the custom skippable
frame used by upstream. Sidecars are flushed after the main file.
`SUPPORTED_VERSIONS = [1, 2]` and `DEFAULT_WRITE_VERSION = 2`.

**Verified** from `database.rs` lines 31–42, 218–304.

### 2.3 Search pipeline

`nix-locate` parses flags and, when `--whole-name` is used with a literal
pattern containing `/`, extracts the basename and sets
`SearchOptions::exact_basename`. `database::search` then:

1. Opens the FST sidecars via `BasenameIndex::open`.
2. Looks up `exact_basename` to get an `IndexSet<String>` of candidate package
   labels.
3. Opens `Reader`, which parses the v2 seek table into `(offset, len)` frames.
4. `search_entries` does a `rayon` `par_iter` over frames, decompresses each,
   scans frcode, and filters by regex, package pattern, hash, and the candidate
   label set.
5. Prints results (minimal or full with color/grouping).

**Verified** from `nix-locate.rs` lines 109–127 and `database.rs` lines
737–790.

### 2.4 Build pipeline

`nixdex` uses `nix-eval-jobs --meta` with NDJSON output (`nixpkgs.rs` line
359), then:

1. `list_packages_with_scopes` evaluates the root + extra scopes.
2. `Package::main_program` is attached only to the `out` output.
3. `fetch_listings` runs a bounded tokio worker pool (`Semaphore(jobs)`),
   recursively walks narinfo `References`, deduplicates by store-path hash, and
4. Emits a synthetic `/bin/<mainProgram>` tree when a root narinfo or `.ls` is
   missing.
5. `IndexBuilder` serially writes each `(StorePath, FileTree)` to `Writer::add`
   and finishes.

**Verified** from `index.rs` and `listings.rs`.

### 2.5 Hardening already present

- `database.rs` caps `MAX_FRAME_COUNT`, `MAX_DATABASE_BYTES`, and validates
  the seek table with `checked_*` arithmetic.
- `hydra.rs` retries on 5xx/timeouts, caps per-request time, sniffs zstd/xz/plain.
- `frcode.rs` rejects `\0` and `\n` in paths and uses `checked_add` on the
  shared-prefix differential.
- `basename_index.rs` caps `MAX_FST_BYTES` (128 MiB), `MAX_POSTINGS_BYTES`
  (1 GiB), and `MAX_ORDINALS_PER_BASENAME` (1 M).
- Workspace lints: `unsafe_code = "forbid"`, `unwrap_used = "deny"`,
  `panic = "deny"`, `todo = "deny"`, `unimplemented = "deny"`.

**Verified** from `Cargo.toml` and the relevant source files.

### 2.6 Gaps and pain points

| Gap | Evidence | Impact |
|-----|----------|--------|
| `--path-cache` returns `Error::NotImplemented` | `index.rs` lines 89–93 | Re-fetching `.ls` trees on every build is wasteful |
| `nixdex-daemon` is a 10 ms sleep scaffold | `daemon.rs` lines 34–49 | No scheduled refresh / notification |
| `nixdex` always passes `--meta` and has no `--no-main-program` toggle | `nixpkgs.rs` line 359, `nix-index.rs` | Slightly higher eval output; no parity with upstream flag |
| `Reader::open` reads the entire `files` blob into a `Vec<u8>` | `database.rs` line 378 | O(DB-size) startup allocation and copy |
| `BasenameIndex::open` reads FST + postings into `Vec<u8>` | `basename_index.rs` lines 223, 229 | Same issue for the sidecars |
| `search_entries` decompresses **all** frames for non-basename queries | `database.rs` lines 451–475 | Full DB scans remain expensive even when FST could skip frames |
| `list_packages_with_scopes` buffers all eval lines before returning | `nixpkgs.rs` lines 404–426 | Memory scales with attr count |
| `Writer` buffers all raw frcode before `finish` | `database.rs` lines 180, 224 | Memory scales with total indexed paths |
| No prebuilt-index consumption / NixOS modules | — | Users must build from scratch |
| No shell wrappers (`command-not-found.sh`, `.nu`) | repo layout | Cannot replace upstream out-of-the-box |

## 3. Competitive and upstream baseline

### 3.1 Upstream `nix-index` (2026-07-16)

- **v0.1.11** added zstd-compressed `.ls` support (PR #320). **Verified** from
  release notes.
- **Multi-frame v2** landed in commit `b779316` (PR #325). It stores
  independent zstd frames cut at package boundaries and a trailing skippable
  seek table. Benchmark on full nixpkgs, 16 cores:
  - before: **2.225 s ± 0.041 s**
  - after: **0.690 s ± 0.027 s** (~3.2× faster)
  **Verified** from the commit message.
- **`meta.mainProgram`** landed in commit `ceecc0c` (PR #318). It adds a
  `--no-main-program` flag to `nix-index` and synthesizes `/bin/<mainProgram>`
  entries when Hydra has not built the wrapper. It rewrites the `nix-env --query
  --xml` parser with `quick-xml`/Serde, which is ~4× faster, but `--meta`
  makes the XML ~10× larger so overall parsing is ~3× slower.
  **Verified** from the commit and PR discussion.
- `nixdex` is already doing equivalent work but uses `nix-eval-jobs --meta`
  (NDJSON) instead of XML, avoiding the XML size/speed penalty.
  **Verified** from `nixpkgs.rs`.

### 3.2 `nix-index-database`

Weekly prebuilt indexes are the dominant UX path for most users:

| Asset (2026-06-28 release) | Size |
|----------------------------|------|
| `index-x86_64-linux` | 57.4 MB |
| `index-x86_64-linux-small` | 1.3 MB |
| `index-aarch64-linux` | 56.0 MB |
| `index-aarch64-darwin` | 39.7 MB |

**Verified** from
https://github.com/nix-community/nix-index-database/releases/tag/2026-06-28-064642.

The repository provides `nixosModules.default` and `homeModules.default` that
wrap `nix-locate` to use the downloaded database and can optionally install
`comma`.

### 3.3 Nix tooling consumers

- **`comma`** runs `nix-index` (via `nix-index-database`) for one-shot command
  execution.
- **`nh`** (Nix helper) and other wrapper packages are affected when only the
  `-unwrapped` variant is built by Hydra; `meta.mainProgram` fixes this.
- **`nix-search`** (Bluge) and **`rippkgs`** (SQLite) solve a different
  problem: package name/description search, not file locate.

## 4. 2026 modernization options and benchmarks

### 4.1 Index search primitives

| Approach | Best for | Pros | Cons |
|----------|----------|------|------|
| FST + Roaring postings (current) | Exact basename (`bin/ls`) | Tiny, mmap-friendly, <5 ms cold | Multi-value postings; no substring |
| Multi-frame zstd parallel scan | Full regex / arbitrary patterns | Matches upstream v2; 3.2× speedup | Still O(frames) work |
| Selective frame decompression | Candidate-frame-only queries | Avoids decoding 90%+ of frames | Needs package→frame sidecar |
| Full-path / suffix FST | Prefix or suffix queries | Fast `lib/libfoo.so` lookups | Larger index; needs careful key design |
| Aho-Corasick / trigram index | Arbitrary substring | Can beat ripgrep 5–14× on code | Heavy build, overkill for nix paths |
| Tantivy / full-text | Package descriptions | BM25, fuzzy, scoring | Much larger; not path-oriented |

**FST + Roaring** remains the right primary index. `roaring` supports
`intersection_len`, union/difference operators, and `serialize_into` for the
postings sidecar. **Verified** from `context7` docs for `roaring-rs`.

### 4.2 Embedded stores and serialization

| Store / Format | Role for nixdex | Notes |
|----------------|-----------------|-------|
| `postcard` | `paths.cache` candidate | `no_std`, compact, serde-based; fast enough |
| `bincode` v2 | `paths.cache` candidate | Varint config, owned decode ~165 ns |
| `rkyv` | zero-copy cache / index | Fastest access but alignment-sensitive; risky for mmap |
| `redb` | incremental package metadata | Pure Rust, ACID, MVCC; bulk load ~17 s vs LMDB ~9 s on large benchmark; **larger on disk than RocksDB/SQLite** |
| `heed`/LMDB | high-read KV | Faster than redb in micro-benchmarks; C dependency |
| SQLite FTS5 | package description search | Flexible, but heavy and not needed for file locate |

`redb` is attractive for an **incremental/delta** layer (package metadata,
changed attr tracking) but should **not** replace the NIXI `files` format,
because prebuilt upstream DBs and `zstd -d` interop are critical.

### 4.3 Async and concurrency patterns

The build already uses `tokio` + `rayon`. Further wins:

- **Bounded backpressure**: `fetch_listings` output channel is bounded to
  `jobs * 2`; good. The eval side should stream lines instead of buffering all.
- **`spawn_blocking` for compression**: zstd frame encoding is CPU-bound and
  currently blocks async workers via `rayon` inside `do_finish`.
- **`tokio::sync::Semaphore` for HTTP**: already used.
- **`io_uring` (`tokio-uring`/`monoio`)**: possible for high-QPS fetch, but
  `reqwest` is tokio-native and the cache servers are not latency-sensitive
  enough to justify the portability cost.

### 4.4 `nix-eval-jobs` capabilities

`nix-eval-jobs` 2.33+ supports:

- `--meta` (NDJSON `meta` field) — already used.
- `--check-cache-status` → `cacheStatus` key.
- `--select` expression for scoped evaluation.
- `--no-instantiate` for read-only fast evaluation.
- Streaming NDJSON output (no need to buffer).

PR #418 (cache-status async) improved 500 negative narinfo lookups from ~57 s
 to ~3–5 s.

## 5. Roadmap 002: ranked program

### P0 — Operational parity and observability

1. **`--path-cache` implementation**
   - Serialize fetched `(hash, FileTree, narinfo refs, fetched_at)` to a
     `paths.cache` file using `postcard` or `bincode` v2 with a version header.
   - On build, check the cache before HTTP; skip `nix-eval-jobs` output paths
     that are already cached.
   - Refuse to use a stale cache silently; include a `--force` flag or checksum.

2. **Progress and metrics**
   - `tracing` counters and a `indicatif` (or `tracing`-derived) progress bar for
     eval packages/s, fetch packages/s, bytes downloaded, skip/error rates.
   - Emit a final summary (`indexed`, `failed`, `cached`, `size_bytes`).

3. **`--no-main-program` toggle**
   - Add `main_program: bool` to `UpdateOptions` and `nix-index` CLI.
   - When disabled, skip `--meta` and `main_program` synthesis to reduce eval
     output size and match upstream behavior.

4. **`nixdex-daemon` real loop**
   - Replace the scaffold with a configurable loop that can:
     - Run `update_index` once per interval.
     - Optionally compare `nix-index-database` release hash and refresh when it
       advances.
     - Exit cleanly on `SIGTERM`.

### P1 — Search performance

1. **Selective frame decompression**
   - Build a package-ordinal → frame-index map during `Writer::finish`.
   - For queries with a non-empty `exact_basename` (or future full-path
     candidate set), decompress only the frames that contain at least one
     candidate package.
   - Target: cold `nix-locate bin/ls` on the full x86_64-linux DB **< 100 ms**,
     beating upstream's 0.690 s.

2. **Mmap `Reader` and `BasenameIndex`**
   - Replace `std::fs::read` in `Reader::open` and `BasenameIndex::open` with
     `memmap2` (or `fs::File` + `mmap`). `fst::Map::new` accepts `AsRef<[u8]>`
     including mmap-backed slices.
   - Eliminates startup copy and memory spikes for the 57 MB full DB.

3. **Return borrowed labels from `lookup_basename`**
   - Avoid cloning every `package_names` string for common basenames.

### P2 — Build scaling

1. **Streaming eval**
   - Convert `nixpkgs::list_packages_async` from `Result<Vec<EvalJobLine>>` to
     an `mpsc` channel or async stream.
   - `IndexBuilder` can start fetching and writing as soon as the first package
     is evaluated, instead of waiting for all attrs.

2. **Streaming / bounded writer**
   - Flush completed frames to disk when the in-memory raw frcode buffer
     exceeds a size threshold, rather than buffering the whole index.
   - Keep the final skippable seek table rewrite at the end.

3. **CPU-offload compression**
   - Run zstd frame compression in `tokio::task::spawn_blocking` or `rayon`
     inside `Writer::finish` (already parallel, but ensure it does not block the
     async runtime).

### P3 — Ecosystem integration

1. **Prebuilt `nix-index-database` support**
   - `nix-locate` can already read upstream `files` v1/v2. Add a `nixdex`
     wrapper/module to download weekly `nix-index-database` releases, verify
   - hash, and use them.

2. **Shell wrappers and NixOS/home-manager modules**
   - Add `command-not-found.sh` and `command-not-found.nu` templates, mirroring
     upstream. Package as a NixOS module and home-manager module.
   - Integrate `comma` suggestion in the not-found message (upstream PR #314).

3. **Package attr / description search (optional `nixdex search`)**
   - Use the already-fetched `meta` JSON to build a small sidecar of package
     names, descriptions, and main programs. Keep it separate from the file
     locate DB.

### P4 — Stretch

1. **Incremental / delta updates**
   - Store the previous `attr → hash` map in `redb` or a plain sidecar.
   - On rebuild, diff the new eval stream and only fetch/store changed attrs.

2. **Suffix / substring index**
   - A second FST over path suffixes or a lightweight trigram index for
     arbitrary substring queries.

3. **`nixdex-daemon` HTTP API**
   - Provide a resident search server; defer until P0–P2 are proven.

## 6. Alternatives explicitly not recommended

| Alternative | Why it is not the next step |
|-------------|----------------------------|
| Replace NIXI with SQLite/FTS5 | Breaks upstream prebuilt DB interop and `zstd -d` mental model |
| Embed FST inside `files` body | Breaks prebuilt DBs and `zstd -d` compatibility |
| Adopt `rkyv` for zero-copy sidecars | Alignment-sensitive; not worth the risk for a CLI tool's cache |
| `Tantivy` as primary path index | Overkill for path suffix search; larger and slower to build than FST |
| `io_uring` for Hydra fetches | `reqwest` + bounded concurrency already sufficient; portability cost high |

## 7. Acceptance metrics and targets

| Scenario | Metric | Target |
|----------|--------|--------|
| `small.nix` build wall | full build | stay ≤ ~2 s |
| `nix-locate bin/ls` small DB | wall | stay ≤ ~1 ms |
| `nix-locate bin/ls` full `index-x86_64-linux` | cold wall | ≤ 0.7 s (upstream v2), then < 0.1 s with selective frames |
| `nix-locate --whole-name --at-root /bin/ls` full DB | warm FST | < 5 ms |
| `nix-index` full `<nixpkgs>` build | wall | ≤ 5 min (upstream claim) |
| `paths.cache` reuse | avoided HTTP fetches | ≥ 90% on unchanged nixpkgs |
| Binary size | `nixdex-cli` release | < 30 MB (LTO + strip already enabled) |
| CI | `just validate` | green; no new `unwrap`/`expect`/`todo` |

## 8. Confidence summary

| Claim | Tier | Source |
|-------|------|--------|
| nixdex implements v2 read/write, FST sidecars, recursive traversal, mainProgram | **Verified** | direct source read |
| Upstream multi-frame 3.2× speedup numbers | **Verified** | commit `b779316` message |
| Upstream mainProgram `--no-main-program` and XML cost | **Verified** | PR #318 page |
| `nix-index-database` release sizes | **Verified** | release assets page |
| `roaring` set ops / serialization API | **Verified** | `context7` `roaring-rs` docs |
| `redb` benchmark numbers | **Reported** | `cberner/redb` README / benchmark table |
| `fst` memory-map construction | **Verified** | `docs.rs/fst` (`MapBuilder::new`, `Map::new`) |
| Exact full-DB `bin/ls` latency after selective frames | **Unverified** | no measurement on this hardware yet |

## 9. Sources

1. Local tree: `crates/nixdex-core/src/{database,basename_index,listings,hydra,nixpkgs,index}.rs`, `crates/nixdex-cli/src/bin/{nix-index,nix-locate}.rs`, `crates/nixdex-core/src/daemon.rs`, `Cargo.toml`, `justfile`, `research/modernization-2026.md`, `research/baseline.md`, `research/wave4-results.md`.
2. Upstream commit `b779316` (multi-frame): https://github.com/nix-community/nix-index/commit/b7793161a4d8f0281133b611a4baaf15916c6413
3. Upstream PR #318 (`meta.mainProgram`): https://github.com/nix-community/nix-index/pull/318
4. Upstream `nix-index-database` release 2026-06-28: https://github.com/nix-community/nix-index-database/releases/tag/2026-06-28-064642
5. `nix-eval-jobs` repo and PR #418: https://github.com/NixOS/nix-eval-jobs
6. `roaring-rs` docs via `context7`.
7. `redb` benchmarks: https://github.com/cberner/redb
8. `fst` crate docs: https://docs.rs/fst/latest/fst/
9. `zstd` crate docs: https://docs.rs/zstd/latest/zstd/
10. Academic: Chambi et al. "Better bitmap performance with Roaring bitmaps" (arXiv:1402.6407).
