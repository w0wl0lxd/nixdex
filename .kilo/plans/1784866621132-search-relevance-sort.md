# Fix Search Ordering: Relevance Sort for SearchSort::None

## Problem

When `nixdex search mise` is run with the default `SearchSort::None`, results are returned in the order they appear in `packages.json` (insertion order). The `mise` package (exact attr match) appears at the bottom instead of the top. The `search_fuzzy` method already sorts by descending score when `SearchSort::None`, but the non-fuzzy `search` method does not.

## Root Cause

In `package_search.rs`, `sort_records` returns early when `sort == SearchSort::None`, preserving insertion order. The `SearchSort::None` variant is also aliased as `"relevance"` in `from_str`, confirming the intent is relevance ordering.

## Fix

### 1. Add relevance scoring to `sort_records` in `package_search.rs`

Modify `sort_records` to accept search parameters and sort by relevance when `sort == SearchSort::None`:

- For **literal substring searches**: compute a score per record based on match quality:
  - Exact attr match: 3000
  - Attr prefix match: 2000
  - Attr substring match: 1000
  - Exact description match: 300
  - Description prefix match: 200
  - Description substring match: 100
  - Exact mainProgram match: 300
  - mainProgram prefix match: 200
  - mainProgram substring match: 100
  - Tie-break: ascending attr

- For **regex searches**: compute score based on match position:
  - Whole attr match (regex spans entire string): 3000
  - Prefix attr match (regex matches from start): 2000
  - Contains attr match (regex matches anywhere): 1000
  - Same for description/mainProgram with lower weights

- For **exact searches**: all matches are equal, sort by attr

### 2. Update `sort_records` signature

Change `sort_records(matches, sort)` to `sort_records(matches, sort, pattern, regex, field, case_sensitive, exact)` so it has the context needed for relevance scoring.

Update the call site in the fast path (exact attr lookup) as well.

### 3. Add regression tests

Add tests to `package_search.rs` verifying:
- Exact attr match ranks above prefix/substring matches
- Prefix attr match ranks above substring attr matches
- Attr matches rank above description matches
- Description matches rank above mainProgram matches
- Tie-breaking by attr name (ascending)
- Case-insensitive relevance scoring works correctly

### 4. Update existing test `sort_orders_results`

The test at line 855 uses `SearchSort::None` with regex `.*` and only checks `len() == 3`, not the order. No changes needed for that test.

### 5. Verify benchmarks still pass

The `bench_search_sort` benchmark in `benches/search.rs` uses `SearchSort::None` among other sort orders. The added relevance sort will change timing slightly but should not cause failures.

## Files to Modify

- `crates/nixdex-core/src/package_search.rs` — add relevance scoring, update `sort_records`
- `crates/nixdex-core/src/package_search.rs` tests — add regression tests

## Verification

1. Run `cargo test -p nixdex-core` to verify all unit tests pass
2. Run `cargo test -p nixdex-cli` to verify CLI tests pass
3. Run `cargo bench -p nixdex-core -- search` to verify benchmarks still work
4. Manual test: `nixdex search mise` should show `mise` at the top