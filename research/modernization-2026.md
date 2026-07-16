# nixdex modernization research (2026-07-16)

Evidence-backed survey of how to modernize, accelerate, and surpass
`nix-community/nix-index` / `nix-locate`. Claims are tagged
**Verified** (primary source read), **Reported** (credible secondary), or
**Unverified**.

## 0. Date and scope

- Research date: **2026-07-16** (host `date -u`).
- Codebase: `nixdex` @ main (Wave 4 complete: NIXI v1 write/search, eval, hydra `.ls`).
- Competitive target: upstream `nix-index` including July 2026 multi-frame work.

## 1. Current nixdex state (Verified — local tree)

| Component | Status |
|-----------|--------|
| NIXI magic + version 1 + single zstd frame + frcode | Implemented (`database.rs`) |
| Parallel lazy `search_entries` (per-CPU zstd frames) | Implemented |
| `Reader::query_fst` | Implemented (`basename_index.rs`) |
| `--path-cache` | Hard-errors (by design) until real |
| Index pipeline eval → concurrent fetch → serial write | Implemented (`index.rs`) |
| Workspace deps `fst`, `redb`, `roaring`, `moka`, `scc`, `postcard` | `fst`, `roaring` used; `redb`, `moka`, `scc`, `postcard` removed |
| Wave 4 small.nix locate | ~700 µs (startup-dominated) |
| Full `<nixpkgs>` index | Not measured; costly |

Non-goals already decided (`.plan/DECISIONS.md`): NIXI first; redb dual-write and API server deferred.

## 2. Competitive: upstream multi-frame zstd (Verified)

**Commit:** [`b779316`](https://github.com/nix-community/nix-index/commit/b7793161a4d8f0281133b611a4baaf15916c6413)
(Mic92 / Jörg Thalheim, **2026-07-02**). Local fetch of full `src/database.rs` from that
revision confirms:

### Format

- Header unchanged: `NIXI` + `u64` LE version (`1` or **`2`**).
- **v1:** single zstd frame over concatenated frcode (legacy; still readable).
- **v2:** multiple **independent** zstd frames, cut only at **package boundaries**
  (frcode reset at each package footer → each frame is a self-contained frcode stream).
- Trailing **zstd skippable frame** magic `0x184D2A50` carrying:
  - `frame_count: u32`
  - `compressed_len: u32` per frame
  - trailing `payload_len: u32` (mirror so table can be found from EOF)
- Plain `zstd -d` skips the trailer; consumers that only know v1 still work for v1 DBs.

### Search

- Load **entire** `files` into `Vec<u8>`.
- Parse seek table → frame `(offset, len)` list.
- `rayon` `par_iter` over frames: `zstd::decode_all` + frcode + (grep-style) matchers.
- Benchmark claimed on full nixpkgs index, 16 cores, `nix-locate bin/ls`:
  - before: **2.225 s ± 0.041 s**
  - after: **0.690 s ± 0.027 s** (~**3.2×**)

### Compatibility

- `--format-version 1` retained for **nix-index-database** and older `nix-locate`.
- nixdex now writes v2 by default and reads both v1 and v2 (format selected via `--format-version`).

**Implication:** A pure FST on basenames without multi-frame parallel decode will lose
the full-index cold-scan race to upstream. **Best end-state = FST (or postings) for
candidate packages + multi-frame (or selective frame) refine.**

## 3. Prebuilt distribution (Verified / Reported)

- [nix-index-database](https://github.com/nix-community/nix-index-database): weekly
  prebuilt indexes; modules wrap `nix-locate` to use them.
- Release assets (e.g. **2026-03-29**): `index-x86_64-linux` ~**89 MB**, `-small` ~1.6 MB.
- Users often **never** run local `nix-index`; latency of **locate against prebuilts**
  dominates UX (command-not-found, comma).

## 4. FST secondary indexes (Verified)

Primary sources:

- [BurntSushi transducers essay](https://burntsushi.net/transducers/)
- [docs.rs/fst](https://docs.rs/fst/latest/fst/) — crate **0.4.x**, stable API
- GitHub [BurntSushi/fst](https://github.com/BurntSushi/fst)

Facts:

- Ordered sets/maps over byte keys as **deterministic acyclic FSTs**; excellent prefix
  compression for path/basename corpora.
- **Memory-map friendly** (`Map::new` over `&[u8]`); construction via `MapBuilder` can
  stream to a file with ~constant memory if keys are inserted **sorted**.
- Query with automata: exact key, range, regex via `regex-automata` + `fst::Automaton`
  (when features enabled). Levenshtein is memory-heavy (PoC quality).
- Values are **u64 only** — multi-package basenames need **external postings**
  (offset/length into a roaring or `u32` list), not one FST value per hit list.

**Recommended W5 shape for nixdex:**

| Artifact | Role |
|----------|------|
| `files` | Canonical NIXI (v1 now; v2 multi-frame next) |
| `files.basename.fst` | Sidecar: basename → postings cookie (`u64`) |
| `files.basename.postings` | Cookie → packed package ordinals (prefer Roaring or LE `u32` runs) |

Why **sidecar not embedded in `files`:**

- Prebuilt upstream DBs and `zstd -d` stay valid.
- Optional: locate works without FST (linear / multi-frame fallback).
- Building FST never corrupts the interop surface.

**Open algorithm choice:**

1. **Basename FST + postings** — optimal for `--whole-name --at-root /bin/$cmd` (cnf).
2. **Full-path FST** — larger; better for random `bin/foo` substrings if keys are
   normalized (drop leading `/`, index path suffixes).
3. **Automaton ∩ FST** for `--regex` — only when pattern is prefix/literal-heavy;
   otherwise fall back to multi-frame scan.

## 5. Compression / string dictionaries (Verified academic)

- Front-coding is exactly what **frcode** already does between consecutive paths.
  Hierarchical front-coding + LCP tricks improve **random access** in string
  dictionaries ([arXiv:1911.08372](https://ar5iv.labs.arxiv.org/html/1911.08372) —
  *Improved Compressed String Dictionaries*, Brisaboa et al., CIKM 2019).
- “Fast & Strong” RPFC work (ACM 2019 / VLDB J 2020) shows **grammar + front-coding**
  can beat plain front-coding, but with higher build cost — better for a static
  offline rebuild of full nixpkgs than for every local `nix-index` run.
- Dictionary-compressed text indexes (attractors, CFGs) give asymptotic locate/count
  ([Optimal-Time Dictionary-Compressed Indexes](https://doi.org/10.1145/3426473)) but are
  research-grade for path corpora; not a near-term crate drop-in.

**Practical takeaway:** Keep frcode + zstd for the log; add FST/postings for
**lookup**, not as a full frcode replacement.

## 6. Multi-frame / seekable zstd ecosystem (Verified)

- Zstd frames are independently decompressible when concatenated
  ([format doc](https://github.com/facebook/zstd/blob/master/doc/zstd_compression_format.md),
  RFC 8878).
- **Seekable format** places a seek table in a skippable trailer
  ([contrib/seekable_format](https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md)).
- Upstream nix-index uses a **simpler custom** skippable payload (frame lengths only),
  not the full Facebook seekable footer schema — **interop with seekable tools is not
  free**; matching **nix-index v2** is the interop that matters.

## 7. Alternatives considered (and when not to use them)

| Approach | Pros | Cons for nix-locate |
|----------|------|---------------------|
| Multi-frame zstd (upstream v2) | 3× on full DB; interop | Still decompresses all frames unless filtered |
| FST + postings | Sub-ms cnf; skips most packages | Extra build; multi-value postings needed |
| redb / LMDB secondary | ACID, incremental | Larger; weak vs mmap FST for pure string keys; **explicit non-goal near-term** |
| SQLite FTS5 | Flexible text search | Heavier deps (upstream already has sqlite for channel index only) |
| Tantivy / full IR | Scoring, fuzzy | Overkill for path substring; large index |
| rippkgs (SQLite package meta) | Fast *package name* search | Different problem (not file paths) |
| nix-search (Bluge) | Fast *attr* search | Not file locate |
| Full FM-index / CSA | Strong substring | Complex; build memory |

## 8. Index *build* path improvements (Verified local + Reported)

Current bottlenecks in nixdex `index.rs`:

1. **Hold all fetch tasks** then write serially — fine for small; memory risk on 100k+.
2. **No retry/backoff** on `.ls` 404/5xx.
3. **No path-cache** (dev loop tax).
4. **Writer is single zstd stream** — cannot compress packages in parallel (upstream v2 can).
5. Eval via `nix-eval-jobs` is already preferred over full `nix-env` where possible
   (`research/eval-cache-api.md`).

Recommended build scaling (W6–W7):

- Stream write packages as they complete (or bounded channel of ready trees).
- Optional `paths.cache` = versioned **postcard** of `(hash, tree)` — never silent stub.
- Parallel frame compress on finish (align with v2).
- HTTP: cap concurrency, exponential backoff, circuit-break 404s (hydra partial).
- Progress: tracing counters (packages/s, bytes, skip rates).

## 9. Daemon / UX (Reported + local stub)

- `nixdex-daemon` is a stub (`NotImplemented`).
- High-value future: scheduled refresh against pinned nixpkgs, notify when
  nix-index-database hash advances, `command-not-found` latency budget **\<5 ms** p99
  via resident FST mmap.

## 10. Ranked modernization program

### P0 — Correctness / competitive parity

1. **Read NIXI v2 multi-frame** (and write optionally) — match Mic92 format exactly.
2. **Parallel locate** over frames (`rayon`) for full-index cold queries.
3. Keep **v1 write** as default or flag until nix-index-database migrates.

### P1 — FST acceleration (roadmap W5)

1. Sidecar basename FST + postings; `query_fst` real.
2. Wire `search()`: if FST present and pattern is whole-name/at-root friendly,
   restrict to candidate packages (later: skip non-candidate frames).
3. Unit tests + hyperfine on medium fixture.

### P2 — Build ergonomics (W6–W7)

1. Real `--path-cache`.
2. Streaming writer / memory bound.
3. Retry, metrics, optional zstd level defaults closer to upstream 19.

### P3 — Stretch

1. Selective frame decompress using package→frame map.
2. Optional reverse-suffix FST for free-form substrings.
3. Daemon + prebuilt hybrid.
4. redb only if incremental package updates prove necessary.

### Explicit do-nots (near term)

- Do **not** embed FST inside `files` body (breaks prebuilt / `zstd -d` mental model).
- Do **not** replace NIXI with SQLite as primary format.
- Do **not** silently no-op stubs.
- Do **not** depend on full Facebook seekable API until/unless upstream does.

## 11. Acceptance metrics (targets)

| Scenario | Metric | Target |
|----------|--------|--------|
| small.nix locate | wall | stay ≤ ~1 ms (no reg) |
| full nixpkgs locate `bin/ls` | wall, 8–16c | **≤ upstream v2 (~0.7 s)** then beat |
| cnf query `/bin/$cmd` with FST | wall warm | **\< 5 ms** goal |
| FST build cost | extra % of index write | budget ≤ +15% wall on medium |
| Interop | read upstream v1 DBs | must keep |
| Interop | read upstream v2 DBs | after P0 |

## 12. Sources

1. Local: `.plan/*`, `research/baseline.md`, `research/wave4-results.md`, crates under `crates/`.
2. Upstream commit b779316 full `database.rs` (raw.githubusercontent.com + local `.upstream` fetch).
3. https://github.com/nix-community/nix-index
4. https://github.com/nix-community/nix-index-database
5. https://burntsushi.net/transducers/ and docs.rs/fst
6. https://github.com/facebook/zstd docs (frames, skippable, seekable)
7. arXiv/ar5iv 1911.08372; ACM TOA optimal dictionary-compressed indexes DOI 10.1145/3426473
8. redb vs LMDB/RocksDB bench tables on cberner/redb README (2025–2026 era numbers)

## 13. Confidence summary

| Claim | Tier |
|-------|------|
| Upstream multi-frame design + 3.2× bench numbers | **Verified** (commit message + source) |
| nixdex unused indexing deps / FST stub | **Verified** (local grep) |
| Prebuilt ~89 MB x86_64-linux (Mar 2026) | **Verified** (release page assets) |
| FST best for basename cnf path | **Verified** (fst docs + query shape) |
| Full grammar dictionaries beat frcode for offline rebuilds | **Reported** (CIKM/VLDB literature) |
| Exact full-nixpkgs wall on this machine | **Unverified** (not rebuilt here) |
| arXiv MCP tool uptime this session | **Unverified** (tool errored; used ar5iv/web) |
