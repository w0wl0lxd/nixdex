# nixdex: Comprehensive Audit, Ratatui TUI, and Specify Workflow Plan

## Purpose

Exhaustive gap analysis of nixdex against nix-locate/nix-index and related tools,
followed by scoping and implementing a ratatui TUI, search ordering/accuracy
improvements, CLI/UIX polish, and a worktree-based PR using the specify/speckit
full workflow.

## 1. Audit Findings: Gaps vs nix-locate and Related Tools

### 1.1 Already Covered (nixdex exceeds upstream)

- Drop-in `nix-locate` and `nix-index` binaries (v1 DB format compatible)
- V2 database format with per-CPU zstd frames
- Secondary indexes: basename FST, path FST, entry index, ngram index, path trigram, path entry, command index
- Prebuilt index download with auto-refresh
- Resident daemon with LRU cache mode
- `nixdex` umbrella CLI with 14+ subcommands (search, info, stats, completions, man, index, locate, which, update, generate-sidecars, command-not-found, daemon)
- Fuzzy search via frizbee (SIMD)
- Package metadata search (`packages.json` sidecar)
- Command-not-found integration with `--auto-install`, `--auto-run`, `--interactive`
- Shell integration scripts (bash, zsh, fish, nushell)
- NixOS and Home Manager modules
- Color/JSON/count/limit/sort/min-size/max-size/exclude-fhs output controls
- `--no-daemon` fallback for locate
- Path cache for incremental index builds
- Redb exact-path sidecar (opt-in)
- `--small` flag for `/bin/`-filtered database
- `--no-closure`, `--no-main-program`, `--no-overlays` flags
- `--select`, `--only-eval`, `--extra-scopes` for nix-eval-jobs
- `--filter-prefix` / `--exclude-prefix` for path filtering
- `--cache-url` / `--substituter` for binary cache URL
- `--prebuilt-*` flags for prebuilt index downloads
- `--format-version` for v1/v2 DB compatibility
- `--chunk-size` for v2 frame flushing control
- `--compression-level` (1-22)
- `--requests`, `--timeout`, `--retries` for HTTP tuning
- `--index-cache-mode` (resident/lru) for daemon
- `--admin-token` for daemon security
- Env vars: `NIX_INDEX_DATABASE`, `NIXDEX_DATABASE`, `NIXDEX_DAEMON_ADDR`, `NIXDEX_NO_DAEMON`, `NIXDEX_ADMIN_TOKEN`, `NIX_AUTO_INSTALL`, `NIX_AUTO_RUN`, `NIX_AUTO_RUN_INTERACTIVE`
- Benchmark scripts (locate, search, index, index-comparison)
- Comprehensive test suite with differential sidecar/pruning tests

### 1.2 Performance Gaps

| Gap | Priority | Notes |
|-----|----------|-------|
| No ngram result caching | High | `resolve_ngram_ordinals_multi` recomputes on every query; daemon would benefit from LRU cache |
| No parallel trigram fast path | High | `search_path_trigram` iterates candidates sequentially; `search_entries` already uses `par_iter()` |
| No SIMD bitmap intersection | High | `roaring` 0.10 has `simd` feature flag not enabled; AVX2/AVX-512 for array container intersections |
| No query selectivity estimation | Medium | No early-exit when intersection is empty before iterating candidates |
| No incremental sidecar updates | Medium | Daemon refresh regenerates all sidecars from scratch |
| No mmap prefault per-sidecar | Medium | `prefault()` exists on Reader but not per-sidecar |
| No cold/warm query latency tracking | Medium | No built-in latency measurement for locate queries |
| No concurrent query benchmark | Low | No benchmark for concurrent locate calls |
| No v1 vs v2 format comparison | Low | No benchmark comparing format performance |

### 1.3 UI/UX Gaps

| Gap | Priority | Notes |
|-----|----------|-------|
| No TUI/interactive mode | **Critical** | No ratatui TUI for locate or search; all interaction is CLI-only |
| No interactive locate with fzf integration | High | No `--fzf` or interactive selection mode |
| No `--no-color` alias | Low | `--color=never` works but `--no-color` is not a documented alias |
| No `--details`/`--verbose` output | Medium | No expanded detail mode showing package description, license, homepage |
| No `--print0` / `--null` output for locate | Medium | Null-delimited output for shell scripting safety |
| No `--yaml` output format | Low | YAML output format for locate results |
| No column width control | Low | No `--col-width` or `--width` option |
| No result grouping beyond `--no-group` | Low | No `--group-by` option |
| No highlight control | Low | No `--no-highlight` option |
| No `--quiet`/`--silent` mode | Medium | No flag to suppress all non-error output |

### 1.4 Devex/Scripting Gaps

| Gap | Priority | Notes |
|-----|----------|-------|
| No `--lib`/library mode output | Medium | No machine-parseable output for programmatic consumption |
| No `--pipe`/pipeable mode | Medium | No pipe-friendly output mode |
| No JSON with full metadata for locate | High | JSON output for locate does not include description, license, homepage, maintainers |
| No unified `--output-format` | Medium | No single flag supporting text/json/ndjson/csv/tsv/yaml |
| No scripting-friendly exit codes | Medium | Locate exits 0 even when no matches found |
| No `--config`/`--config-file` | High | No config file support; all configuration is CLI flags and env vars |
| No `--defaults`/`--show-defaults` | Medium | No option to show current default values |
| No `--dump-config` | Medium | No option to dump effective configuration |
| No XDG config dir support | Low | No use of `$XDG_CONFIG_HOME` for config file location |

### 1.5 Installation/Dropin Gaps

| Gap | Priority | Notes |
|-----|----------|-------|
| No standalone `nixdex` package in nixpkgs | High | Users must use `nix run github:w0wl0lxd/nixdex` or build from source |
| No `--install`/`--auto-install` for locate | High | No flag to auto-install the found package |
| No `--run`/`--auto-run` for locate | High | No flag to auto-run the found command |
| No `--shell`/`--shell-run` for locate | High | No flag to run in a temporary nix shell |
| No `--profile`/`--add-to-profile` | Medium | No flag to add found package to user's nix profile |
| No fhs-style exclusion by default | Low | `--exclude-fhs` is opt-in |
| No `--only-eval` for locate | Low | No metadata-only lookup without full path search |

### 1.6 Configuration Gaps

| Gap | Priority | Notes |
|-----|----------|-------|
| No `--config`/`--config-file` | High | No config file support |
| No `--defaults`/`--show-defaults` | Medium | No option to show default values |
| No `--dump-config` | Medium | No option to dump effective configuration |
| No config file precedence docs | Low | No documentation of precedence |
| No XDG config dir support | Low | No use of `$XDG_CONFIG_HOME` |

### 1.7 Search Ordering/Accuracy Gaps

| Gap | Priority | Notes |
|-----|----------|-------|
| `SearchSort::None` does not sort by relevance | **Critical** | Non-fuzzy search returns results in insertion order; `mise` appears at bottom instead of top |
| No relevance scoring for non-fuzzy search | **Critical** | `sort_records` returns early for `SearchSort::None`, preserving insertion order |
| No fuzzy search for locate (file search) | High | Only `nixdex search` supports fuzzy matching; `nixdex locate` does not |
| No `--name` field search alias | Low | `--field name` works but `--name` is not an alias |
| No `--exclude`/`--exclude-regex` | Medium | No exclusion pattern for search results |
| No `--reverse` order | Low | No reverse sort order |

### 1.8 Missing Features vs Related Tools

| Gap | Priority | Notes |
|-----|----------|-------|
| No TUI for search | **Critical** | No interactive TUI mode for `nixdex search` |
| No TUI for locate | **Critical** | No interactive TUI mode for `nixdex locate` |
| No online search integration | High | No ability to query search.nixos.org Elasticsearch backend |
| No `--channel` selection | High | `nixdex search` only searches local `packages.json`; no channel selection |
| No `--flakes` search | High | No ability to search flake-indexed packages |
| No `--program` search | High | No ability to search by installed program name |
| No `--version` search | High | No ability to search by version constraint |
| No HTTP API endpoints for search/info/history/options | Medium | Daemon only serves `/nix-locate`; no `/search`, `/info`, `/history`, `/options` |
| No streaming locate output | Medium | No `--stream` mode for locate to emit results as found |
| No batch locate with JSON output | Medium | Batch mode exists but no JSON output option |

## 2. Ratatui TUI Implementation Plan

### 2.1 Design

The TUI will be a standalone `nixdex tui` subcommand that provides interactive
search for both package metadata and file location. It will use ratatui for
rendering and crossterm for terminal input handling.

### 2.2 TUI Screens

1. **Search Screen** — Primary screen with:
   - Search input bar at top (text input with live filtering)
   - Results list in the main area (scrollable, with highlighting)
   - Status bar at bottom showing query stats, result count, and navigation keys
   - Support for both `nixdex search` and `nixdex locate` modes (toggleable)

2. **Detail Screen** — Press Enter on a result to see:
   - Full attribute path, name, description
   - Store path, size, file type
   - License, homepage, maintainers (for package search)
   - Main program, outputs

3. **Command-not-found Screen** — For `nixdex which`-style lookups:
   - List of providers with install/run actions
   - Interactive selection with number keys

### 2.3 Key Bindings

| Key | Action |
|-----|--------|
| `Ctrl+C` / `q` | Quit |
| `Esc` | Close detail screen / clear search |
| `Enter` | Open detail screen for selected result |
| `Up`/`Down` | Navigate results |
| `PageUp`/`PageDown` | Page through results |
| `Home`/`End` | Jump to first/last result |
| `Tab` | Switch between search modes (search/locate/which) |
| `Ctrl+R` | Refresh database (reopen DB) |
| `Ctrl+N` | Toggle null-output mode |
| `Ctrl+J` | Toggle JSON output mode |
| `/` | Focus search input |
| `:` | Open command palette |

### 2.4 Implementation Details

#### New Dependencies (add to `nixdex-cli/Cargo.toml`)

```toml
ratatui = "0.25"
crossterm = "0.27"
tokio = { workspace = true, features = ["rt-multi-thread"] }
```

#### New Files

- `crates/nixdex-cli/src/tui.rs` — TUI application logic (app state, event handling, rendering)
- `crates/nixdex-cli/src/tui/app.rs` — Application state struct and update logic
- `crates/nixdex-cli/src/tui/ui.rs` — Ratatui widget rendering
- `crates/nixdex-cli/src/tui/event.rs` — Terminal event handling
- `crates/nixdex-cli/src/bin/nixdex.rs` — Add `Cmd::Tui(TuiOpts)` variant

#### TUI State Machine

```
AppState {
    mode: SearchMode,        // Search | Locate | Which
    input: String,           // Current search input
    results: Vec<SearchResult>,
    selected: usize,         // Currently selected result index
    scroll: u16,             // Scroll offset
    detail: Option<Detail>,  // Open detail view
    status: Status,          // Status bar info
    config: TuiConfig,       // Colors, layout
}
```

#### Rendering

- Use ratatui's `Paragraph` for the input bar
- Use `List` with `ListState` for results
- Use `Block` with borders for the detail view
- Use `Span` with `Style` for syntax highlighting (attr in green, name in bold)
- Use `Gauge` or `Spinner` for loading states

#### Event Loop

```rust
loop {
    render(&mut terminal, &app)?;
    match event::read()? {
        Event::Key(key) => app.handle_key(key)?,
        Event::Mouse(mouse) => app.handle_mouse(mouse)?,
        Event::Resize(w, h) => app.resize(w, h)?,
    }
}
```

### 2.5 Integration Points

- Reuses `nixdex_core::search_database` and `nixdex_core::search_database_results` for actual queries
- Reuses `nixdex_core::database::SearchOptions` for query configuration
- Uses `nixdex_cli::locate::Opts` for locate mode configuration
- Uses `nixdex_cli::bin/nixdex.rs` `run_search` logic for rendering results
- TUI runs as a subcommand of the `nixdex` binary (no new binary needed)

## 3. Search Ordering/Accuracy Improvements

### 3.1 Relevance Sorting for `SearchSort::None`

**Problem**: `nixdex search mise` with default `SearchSort::None` returns results in insertion order; exact attr match appears at bottom.

**Fix** (in `package_search.rs`):
- Modify `sort_records` to compute relevance scores when `sort == SearchSort::None`
- Scoring hierarchy:
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
- For regex searches: score based on match position (whole > prefix > contains)
- For exact searches: all matches equal, sort by attr

### 3.2 Fuzzy Search for Locate

**Problem**: `nixdex locate` does not support fuzzy matching; only `nixdex search` does.

**Fix**: Add `--fuzzy` flag to `locate::Opts` that uses frizbee to fuzzy-match against path basenames, similar to how `nixdex search --fuzzy` works for package metadata.

## 4. CLI/UIX Improvements

### 4.1 `--print0` / `--null` Output for Locate

Add `--print0` / `--null` flag to `locate::Opts` that uses NUL (`\0`) as the output delimiter instead of newline, making output safe for shell parsing with `xargs -0`.

### 4.2 `--quiet`/`--silent` Mode

Add `--quiet` flag that suppresses all non-error output (useful for scripting and exit-code-only usage).

### 4.3 `--details`/`--verbose` Output for Locate

Add `--details` flag that shows expanded metadata (description, license, homepage, maintainers) alongside the standard locate output.

### 4.4 Unified `--output-format` Flag

Add `--output-format` flag supporting `text`, `json`, `ndjson`, `csv`, `tsv`, `yaml` as a unified alternative to the current per-command format flags.

### 4.5 `--no-color` Alias

Add `--no-color` as a documented alias for `--color=never`.

### 4.6 `--exclude`/`--exclude-regex` for Search

Add `--exclude` and `--exclude-regex` flags to `SearchOpts` that filter out results matching the given pattern.

### 4.7 `--reverse` Sort Order

Add `Reverse` variant to `SearchSort` that reverses the current sort order.

## 5. Specify/Speckit Full Workflow

### 5.1 Initialize Specify Project

```bash
cd /home/w0w/dev/nixdex
specify init nixdex --integration claude
```

This creates a `.specify/` directory with:
- `specs/` — specification files
- `scripts/` — automation scripts
- `workflows/` — workflow definitions
- Templates for spec-driven development

### 5.2 Create Spec for Each Feature Area

Each feature area gets a spec file in `.specify/specs/`:

1. `ratatui-tui.md` — TUI implementation spec
2. `search-relevance-sort.md` — Relevance scoring spec
3. `locate-fuzzy-search.md` — Fuzzy search for locate spec
4. `cli-ux-improvements.md` — CLI/UIX polish spec
5. `performance-optimizations.md` — Performance improvements spec

### 5.3 Worktree + PR Workflow

```bash
# Create worktree for the TUI feature
git worktree add /home/w0w/dev/nixdex-wt/tui -b feat/ratatui-tui

# In the worktree, implement the feature following the spec
# Run specify workflow to track progress
specify workflow run tui-implementation

# When done, push and create PR
git push origin feat/ratatui-tui
gh pr create --title "feat(tui): add ratatui TUI for interactive search" \
  --body-file .specify/specs/ratatui-tui.md
```

### 5.4 Workflow Steps

1. **Init**: `specify init` in the project root
2. **Spec**: Write spec for each feature in `.specify/specs/`
3. **Plan**: Use `specify workflow run plan` to generate implementation plan from spec
4. **Implement**: Work in a feature branch worktree
5. **Review**: Use `specify workflow run review` to run code review checks
6. **PR**: Push branch and create PR with spec as body
7. **Merge**: Use `specify workflow run merge` to validate and merge

## 6. Implementation Order

### Phase 1: Audit and Planning (Day 1)
- [x] Complete audit (this document)
- [x] Create specify project and specs
- [x] Prioritize gaps by impact

### Phase 2: Search Ordering/Accuracy (Day 2)
- [ ] Implement relevance scoring for `SearchSort::None` in `package_search.rs`
- [ ] Add regression tests for relevance scoring
- [ ] Benchmark and verify

### Phase 3: CLI/UIX Improvements (Day 3)
- [ ] Add `--print0`/`--null` output for locate
- [ ] Add `--quiet`/`--silent` mode
- [ ] Add `--details`/`--verbose` output for locate
- [ ] Add `--no-color` alias
- [ ] Add `--exclude`/`--exclude-regex` for search
- [ ] Add `Reverse` sort order

### Phase 4: Ratatui TUI (Days 4-7)
- [ ] Add ratatui and crossterm dependencies
- [ ] Implement TUI app state and event handling
- [ ] Implement search screen rendering
- [ ] Implement detail screen rendering
- [ ] Implement command-not-found screen
- [ ] Add `nixdex tui` subcommand
- [ ] Test with real database

### Phase 5: Performance Optimizations (Days 8-10)
- [ ] Enable `roaring` SIMD feature
- [ ] Add ngram result caching
- [ ] Parallelize trigram fast path
- [ ] Add query selectivity estimation

### Phase 6: Testing and Validation (Day 11)
- [ ] Run `cargo test --workspace --no-fail-fast`
- [ ] Run `cargo clippy --all-features -- -D warnings`
- [ ] Run `cargo fmt --all -- --check`
- [ ] Run `just benchmark-locate` and compare
- [ ] Run `just benchmark-search` and compare
- [ ] Verify TUI works with real database

### Phase 7: PR and Merge (Day 12)
- [ ] Push worktree branch
- [ ] Create PR with spec as body
- [ ] Run CI checks
- [ ] Merge

## 7. Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| Ratatui adds significant binary size | Make TUI a separate feature flag (`tui`) in Cargo.toml |
| TUI complexity delays other work | Scope TUI to search/locate modes only; defer command-not-found TUI |
| Relevance scoring changes existing behavior | Keep `SearchSort::None` as the default but add relevance; document change |
| Performance optimizations introduce regressions | Run full test suite after each optimization phase |
| Specify workflow adds overhead | Use specify for spec tracking only; implementation follows standard git workflow |

## 8. Validation Plan

1. Run `cargo test --workspace --no-fail-fast` after each phase
2. Run `cargo clippy --all-features -- -D warnings` after each phase
3. Run `cargo fmt --all -- --check` after each phase
4. Run `just benchmark-locate` and compare against nix-locate
5. Run `just benchmark-search` and compare against nix-search
6. Run `just benchmark-p99` to verify p99 latency targets
7. Manual test: `nixdex tui` launches and renders correctly
8. Manual test: `nixdex search mise` shows `mise` at the top (relevance sort)
9. Manual test: `nixdex locate --print0 bin/ls` outputs NUL-delimited results
10. Verify `nixdex locate bin/ls` returns same results as `nix-locate bin/ls` on same database
11. Verify `nixdex search <query>` returns same results as before (no regressions)

## 9. Open Questions

1. Should the TUI be a separate feature flag or always included?
2. Should the TUI support mouse interaction?
3. Should the TUI support theme customization?
4. Should the TUI support multi-select (for batch install/run)?
5. Should the TUI support live result streaming (like `--stream`)?
6. What is the current p99 latency for cold vs warm searches? Need baseline measurement.
7. What is the actual bottleneck in `search_path_trigram` — candidate iteration, memchr calls, or entry lookup?
8. Should the ngram cache use `lru` crate or a simple bounded HashMap?
