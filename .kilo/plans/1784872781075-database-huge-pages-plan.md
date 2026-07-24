# Phase 4: Huge Pages + NUMA mmap — Implementation Plan

## Goal

Add huge page support and madvise-based prefaulting to the nixdex database reader. This reduces TLB misses for large database files and eliminates minor page faults during search.

## Status: IMPLEMENTED

The `database.rs` already contained the huge pages implementation (`mod huge_pages`, `#[cfg(feature = "huge_pages")]` blocks). The remaining work was Cargo.toml fixes and test compilation fixes.

## Changes Made

### 1. Fixed Cargo.toml dependencies

**Root `Cargo.toml`:**
- Added `libc = "0.2"` to `[workspace.dependencies]`
- Removed `features = ["simd"]` from `roaring` (requires nightly Rust, breaks on stable 1.97.1)

**`crates/nixdex-core/Cargo.toml`:**
- Changed `huge_pages = ["dep:memmap2"]` to `huge_pages = []` (no direct deps needed)
- Replaced `memmap2 = { workspace = true, optional = true }` with `libc = { workspace = true }`

### 2. Fixed pre-existing test compilation errors

Added `quiet: false, details: false` to all 10 `SearchOptions` struct constructions in test code (the uncommitted draft PR added these fields to the struct but didn't update test code).

### 3. `database.rs` — already implemented

- `mod huge_pages` with `advise_huge_pages()` and `advise_willneed()` functions using `libc::madvise`
- `#[cfg(feature = "huge_pages")]` block in `Reader::open()` calling `advise_huge_pages()` after `mmap_guard::map_file()`
- `#[cfg(feature = "huge_pages")]` block in `prefault_mmap()` calling `advise_willneed()` instead of manual page touching

## Verification

- `cargo check -p nixdex-core --features huge_pages` compiles successfully
- `cargo check -p nixdex-core` compiles successfully (without huge_pages)
- All 171 tests pass with and without `huge_pages` feature
- `cargo test -p nixdex-core` passes (167 + 4 = 171 tests)