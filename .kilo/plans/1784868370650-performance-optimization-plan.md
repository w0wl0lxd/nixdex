# nixdex Performance Optimization Plan

## Goal

Make nixdex faster than all competitors (nix-index/nix-locate, nix-search, rippkgs, nps, neix, xgrep) by a massive margin across every search path: locate (file search), search (package metadata), and command-not-found. Target: sub-10ms cold search, sub-1ms daemon search, sub-100ms full nixpkgs locate.

## Context

nixdex already has a strong foundation: v2 database format with per-CPU zstd frames, trigram ngram index, path trigram + entry sidecars, basename FST, command index, resident daemon, mmap'd readers, and prefault. The last 10 commits focused on regex prefix/suffix trigram intersection, ngram multi-resolve, and daemon sidecar loading. Competitors like nix-index still take 1.3s for cold nix-locate queries; nix-search achieves ~33ms but requires a Bluge index; rippkgs uses SQLite.

## Recent Work Analyzed (last 20 commits)

- `70b9391` perf: extract regex literal prefix/suffix and intersect ngram candidates
- `654b8da` feat: intersect prefix and suffix trigram candidates for regex queries
- `f486b43` refactor: lazy-init Reader sidecars with OnceCell
- `75964c9` chore: finish ngram-roaring-postings merge
- `a156354` perf: reduce frcode/zstd overhead during streaming locate
- `e780e4f` perf: v1 cache, footer indexing, and warning cleanup
- `258f4fe` perf: expand benches, fix gate, reuse daemon reader
- `2f068e8` feat: path trigram + entry sidecars for fast locate
- `5f7f2b9` feat: network-fetch acceleration and command index
- `49e4145` feat: content-defined chunking client and CNF benchmark

## Cutting-Edge Research Findings

### 1. Trigram Index + Roaring Bitmap Intersection (fast-grep-rust, cix, instant-grep)
- Sparse n-gram index with Roaring bitmaps achieves 6-25x speedup over ripgrep
- Two-tier Roaring bitmaps (Tier 1 doc-id, Tier 2 line-level) eliminate >99% of I/O before regex engine
- mmap'd posting lists load in 17ms regardless of corpus size
- nixdex already has this for path trigrams; gap: no two-tier posting structure for ngram index

### 2. mmap Prefaulting (tiverse-mmap, qdrant, minidex)
- Prefaulting all pages eliminates minor page faults during search
- nixdex already has `prefault_mmap` in database.rs; gap: no per-sidecar prefault, no huge pages

### 3. Parallel zstd Frame Decompression (nix-index PR #325)
- Independent zstd frames per package enable parallel decompression
- nixdex v2 already uses per-CPU frames; `search_entries` already uses `par_iter()` for parallel frame scanning
- The real gap: `search_results_with_reader` does NOT use parallel frame decompression for the trigram fast path — it falls through to `search_entries` (parallel) only when no fast path applies

### 4. SIMD-Accelerated String Search (memchr 2.7, CRoaring v4.7)
- memchr uses SSE2/AVX2/AVX-512 for substring search
- CRoaring v4.7 adds SIMD Quad for array contains (2x faster warm, 46% faster cold)
- nixdex already uses memchr; gap: verify roaring-rs 0.10 uses SIMD for intersection

### 5. Command Index (nixdex already has this)
- FST-based command-provider index for sub-ms command-not-found
- Recent daemon.rs changes already added `/command` HTTP endpoint and `daemon_command_lookup` in CLI

### 6. Incremental Index Updates (Fast-Indexer/cix, trigrep)
- cix supports filesystem watch with debounced rebuild
- nixdex has daemon with prebuilt refresh; gap: no incremental update of sidecars

### 7. Hybrid Search (pg_biscuit, Biscuit 2.5)
- Roaring bitmap position indexes for wildcard pattern matching
- Biscuit is 5.6x faster than pg_trgm for LIKE queries
- Gap: nixdex has no position-aware ngram index

## Validated Plan

### Phase 1: Baseline Benchmarking (1 day)

**Problem**: No baseline measurements exist for cold/warm search latency, p99, or competitive comparisons.

**Changes**:
1. Run `cargo bench --benches` and record baseline numbers
2. Run `just benchmark-locate` against nix-locate on the same prebuilt database
3. Run `just benchmark-p99` and record p99 latency
4. Profile `search_results_with_reader` with `perf record` to identify hot spots
5. Write baseline numbers to `research/perf-baseline.md`

**Files**: `research/perf-baseline.md`

### Phase 2: Parallelize Trigram Fast Path (1 week) — IMPLEMENTED

**Problem**: `search_results_with_reader` uses `search_path_trigram` which iterates candidates sequentially in a single thread. The fallback `search_entries` already uses `par_iter()` for parallel frame scanning, but the fast path does not.

**Changes** (already committed as `db953a1`):
1. Convert `candidates` RoaringBitmap to `Vec<u32>` and use `par_iter()` for parallel iteration
2. Each candidate path is processed in parallel across available CPU cores
3. Results are collected via `map` + `flatten` + `collect`
4. Error handling uses `ok()`/`return Vec::new()` instead of `?` to allow parallel continuation

**Files**: `crates/nixdex-core/src/database.rs`

### Phase 3: Ngram Result Caching (1 week) — PENDING VALIDATION

**Problem**: `resolve_ngram_ordinals_multi` recomputes trigram candidate bitmaps on every query. For the daemon (resident indexes), the same queries repeat frequently.

**Changes**:
1. Add a bounded `HashMap<String, Arc<RoaringBitmap>>` cache to the `Reader` struct
2. Use `std::sync::Mutex` for thread-safe access (Reader is shared across queries in the daemon)
3. Cache is per-Reader, invalidated when the database is reloaded
4. Use a simple LRU eviction policy with a configurable max size (e.g. 256 entries)
5. Benchmark: target 10x speedup for repeated queries in daemon mode

**Files**: `crates/nixdex-core/src/database.rs`

**Blocked on**: Deciding whether to add the `lru` crate as a dependency or implement a simple bounded HashMap with manual eviction.

### Phase 4: Huge Pages + NUMA-Aware Mmap (1 week) — PENDING VALIDATION

**Problem**: Standard 4KB pages cause TLB misses on large databases.

**Changes**:
1. Add `huge_pages` feature flag to `nixdex-core` Cargo.toml
2. Use `mmap2::MmapOptions::huge_pages()` on Linux where available
3. Add `MADV_HUGEPAGE` advice on mmap'd regions
4. Add `prefault_mmap` variant that uses `MADV_WILLNEED` for sequential pre-fetch
5. Benchmark: measure 10-20% reduction in page-fault latency on warm queries
6. If no measurable benefit, disable by default

**Files**: `crates/nixdex-core/src/database.rs`, `crates/nixdex-core/Cargo.toml`

### Phase 5: Query Plan Optimization (1 week) — PENDING VALIDATION

**Problem**: The search path has 4 fast paths but no selectivity-based ordering. For regex queries with both prefix and suffix trigrams, the intersection happens before the candidate limit check, but there's no early-exit if the intersection is empty.

**Changes**:
1. Add selectivity estimation before executing fast paths
2. For regex queries, check if prefix+suffix intersection is empty before iterating candidates
3. Execute the most selective fast path first; fall back to next if candidate limit exceeded
4. Add `search_plan` debug metric to trace which path was chosen and why
5. Benchmark: target 2-5x speedup for borderline queries that currently fall through to full scan

**Files**: `crates/nixdex-core/src/database.rs`

### Phase 6: Incremental Sidecar Updates (2 weeks) — PENDING VALIDATION

**Problem**: When the daemon refreshes a prebuilt index, all sidecars are regenerated from scratch. For large databases, this wastes time recomputing unchanged trigram/posting data.

**Changes**:
1. Add content-addressable sidecar storage (hash of each frame's content)
2. On refresh, compare hashes and skip unchanged frames' sidecar regeneration
3. Add `sidecar diff` command that outputs which sidecars need rebuilding
4. Modify `generate_sidecars` to accept a `--diff` mode
5. Benchmark: target 10x speedup for incremental index updates

**Files**: `crates/nixdex-core/src/database.rs`, `crates/nixdex-core/src/prebuilt.rs`

### Phase 7: SIMD-Optimized Bitmap Intersection (1 week) — VALIDATED

**Problem**: Roaring bitmap intersection uses scalar operations for array containers by default. CRoaring v4.7's SIMD Quad optimization achieves 2x speedup on warm contains and 46% on cold. The `roaring` 0.10 Rust crate has a `simd` feature flag that enables SIMD vectorized intersection for array containers, but it is not enabled by default.

**Changes**:
1. Enable `simd` feature on `roaring` dependency in `nixdex-core/Cargo.toml`
2. This automatically uses AVX2/AVX-512 SIMD for array container intersections
3. Focus on the `candidates & &suffix_ids` intersection in `search_path_trigram` and `resolve_ngram_ordinals_multi`
4. Benchmark: target 1.5-2x speedup on trigram candidate intersection

**Files**: `crates/nixdex-core/Cargo.toml`

**Validation**: The `simd` feature is a zero-cost opt-in — no code changes needed, just the Cargo.toml feature flag.

### Phase 8: Benchmark Suite Expansion (ongoing)

**Changes**:
1. Add `criterion` benchmarks for parallel trigram search, ngram caching, SIMD intersection
2. Add `hyperfine` comparison benchmarks against nix-locate and nix-search
3. Add p99 latency tracking in CI (already have `p99-guard.sh`)
4. Add regression tests for each optimization phase
5. Target: all benchmarks show >2x improvement over baseline

**Files**: `crates/nixdex-core/benches/`, `.github/workflows/benchmark.yml`

## Dependencies and Risks

| Risk | Mitigation |
|------|-----------|
| Parallel trigram fast path increases memory usage | Cap concurrent workers to `num_cpus`; use chunked iteration |
| Ngram cache increases resident memory | Use LRU with size cap; invalidate on DB reload |
| Huge pages require root/admin on some systems | Make it opt-in via feature flag; fall back to 4KB pages |
| SIMD code is platform-specific | Guard with `cfg(target_arch = "x86_64")`; fall back to scalar |
| Incremental updates add complexity to sidecar format | Version the sidecar format; old sidecars are regenerated on mismatch |
| Query planner adds overhead for simple queries | Only activate planner for regex queries; literal queries use existing fast paths |

## Validation Plan

1. Run `cargo bench --benches` after each phase and compare against baseline
2. Run `just benchmark-locate` against nix-locate on full nixpkgs database
3. Run `just benchmark-search` and compare against nix-search
4. Run `just benchmark-p99` to verify p99 latency targets
5. Run `cargo nextest run --workspace --no-fail-fast` after each phase
6. Run `cargo clippy --all-features -- -D warnings` after each phase
7. Verify `nixdex locate bin/ls` returns same results as `nix-locate bin/ls` on same database
8. Verify `nixdex search <query>` returns same results as `nix search <query>` on same database

## Open Questions

1. What is the current p99 latency for cold vs warm searches? Need baseline measurement (Phase 1).
2. What is the actual bottleneck in `search_path_trigram` — is it the candidate iteration, the memchr calls, or the entry lookup? (Phase 1 profiling will answer this.)
3. Should the ngram cache use `lru` crate or a simple bounded HashMap? (Phase 3 decision.)
4. Is the two-tier ngram index worth the complexity, or would ngram caching + parallel trigram alone achieve the target?