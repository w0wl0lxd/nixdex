# nixdex-tui: Improved TUI Design & Implementation Plan

## Purpose

Design and implement an improved ratatui TUI for nixdex that copies the best
design patterns from competitors (ns-tui, search-nix, trx, nixbox) while
adding nixdex-specific features (locate mode, command-not-found, daemon
integration). The plan covers spec, design, and implementation steps.

## 1. Competitive Analysis Summary

### 1.1 ns-tui (briheet/ns-tui)
- **Strengths**: Fuzzy search, vim keybindings (j/k), Catppuccin theme,
  install methods (nix-shell, NixOS config, nix-env, nix profile),
  one-click copy, responsive layout
- **Weaknesses**: Only searches nixpkgs via search.nixos.org API, no
  locate/file search mode, no daemon integration
- **Best patterns to copy**: Fuzzy search, vim keybindings, install method
  cycling, Catppuccin theme, toast notifications

### 1.2 search-nix (DariusCorvus/search-nix)
- **Strengths**: TUI mode with channel switching, inline detail expansion
  (Enter to toggle, Space to pin), expand-all (`a`), copy commands
  (`y`/`e`/`p`/`r`/`o`), help page (`?`), non-blocking event loop
- **Weaknesses**: Requires search.nixos.org Elasticsearch backend (no
  local DB), no locate/file search mode
- **Best patterns to copy**: Inline detail expansion, Space to pin detail,
  expand-all toggle, copy-to-clipboard actions, channel switching,
  help overlay

### 1.3 trx (pie-314/trx)
- **Strengths**: Multi-backend (pacman, apt, brew), tiered fuzzy search
  (exact → prefix → word-boundary → consecutive → subsequence),
  configurable themes (Nord, Dracula), mouse support, self-updating,
  search result caching, non-blocking event loop with OS threads + mpsc
- **Weaknesses**: Not nix-specific, no locate mode
- **Best patterns to copy**: Tiered fuzzy search scoring, configurable
  themes, mouse support, search result caching, non-blocking event loop
  architecture

### 1.4 nixbox (utensils/nixbox)
- **Strengths**: ratatui TUI with search/installed/build tabs, managed
  file writing, auto-rebuild, nix search integration
- **Weaknesses**: Only manages packages in config files, no locate mode
- **Best patterns to copy**: Tab-based navigation, managed file integration

### 1.5 mica (gemologic/mica)
- **Strengths**: TUI for managing nix environments, search with filters
  (exact, bin:, name:, desc:), preset system, diff preview
- **Weaknesses**: Complex feature set, not a general-purpose package search
- **Best patterns to copy**: Search mode prefixes (exact, bin:, name:, desc:)

## 2. Design Decisions

### 2.1 Architecture

The TUI will be a standalone `nixdex tui` subcommand using the existing
`nixdex-tui` crate. It will use a non-blocking event loop architecture
similar to trx and search-nix:

```
Event Loop:
  - Main loop: draw UI → poll events → update state → repeat
  - Search: debounced (300ms after typing stops)
  - Event channel: tokio::sync::mpsc::unbounded_channel<AppEvent>
  - Event reader: blocking crossterm::event::read() in spawned task
```

### 2.2 Screens/Views

1. **Search Screen** (primary):
   - Header bar with mode indicator (SEARCH/LOCATE/WHICH)
   - Search input with live filtering (debounced)
   - Results list with highlighting and scroll
   - Status bar with result count, navigation keys, and mode

2. **Detail Screen** (overlay):
   - Opens on Enter on a result
   - Shows full metadata: attr, name, description, path, size, license,
     homepage, maintainers, main_program
   - Close with Esc, q, or Enter

3. **Command-not-found Screen** (for `which` mode):
   - Lists providers with install/run actions
   - Interactive selection with number keys

### 2.3 Key Bindings (improved over plan v1)

| Key | Action | Source (competitor) |
|-----|--------|---------------------|
| `Ctrl+C` / `q` | Quit | All competitors |
| `Esc` | Close detail / clear search | search-nix, ns-tui |
| `Enter` | Open detail / select result | All competitors |
| `Space` | Pin/unpin detail open | search-nix |
| `Up`/`Down` or `k`/`j` | Navigate results | ns-tui, search-nix, trx |
| `PageUp`/`PageDown` | Page through results | search-nix |
| `Home`/`End` | Jump to first/last | search-nix |
| `Tab` | Switch modes (search/locate/which) | nixdex plan |
| `Ctrl+R` | Refresh database | nixdex plan |
| `/` | Focus search input | search-nix, ns-tui |
| `:` | Command palette | nixdex plan |
| `a` | Toggle expand-all details | search-nix |
| `y` | Copy package name to clipboard | search-nix |
| `e` | Copy nix-env install command | search-nix |
| `p` | Copy nix profile install command | search-nix |
| `r` | Open nix-shell with package | search-nix |
| `o` | Open homepage in browser | search-nix |
| `?` | Toggle help overlay | search-nix |
| `n` | Toggle null-output mode | nixdex plan |
| `j` (Ctrl) | Toggle JSON output | nixdex plan |

### 2.4 Search Behavior

- **Debounced search**: 300ms delay after typing stops before executing
- **Fuzzy matching**: Enabled by default (uses frizbee, same as `nixdex search
  --fuzzy`)
- **Live filtering**: Results update as you type
- **Result limit**: Default 50, configurable
- **Search modes**:
  - SEARCH: Package metadata search (uses `packages.json` sidecar)
  - LOCATE: File path search (uses `files` database)
  - WHICH: Command-to-package lookup (uses command index)

### 2.5 UI Design

- **Theme**: Catppuccin Mocha (from ns-tui) as default, with Nord and Dracula
  alternatives (from trx)
- **Layout**:
  - Top: Header bar with mode badge and search input
  - Middle: Scrollable results list (or detail view)
  - Bottom: Status bar with result count and navigation hints
- **Highlighting**: Selected item highlighted with background color
- **Colors**: Cyan for mode badges, Yellow for selected items, Green for
  attribute paths

### 2.6 nixdex-Specific Features (not in competitors)

- **Locate mode**: Search for files in nixpkgs packages (unique to nixdex)
- **Command-not-found mode**: Find which package provides a command
- **Daemon integration**: Use resident daemon for sub-100ms queries
- **`--details`/`--quiet` flags**: Pass through from CLI
- **`--print0` support**: Null-delimited output mode
- **`--reverse` sort**: Reverse result order
- **`--exclude`/`--exclude-regex`**: Filter results by pattern

## 3. Implementation Plan

### Phase 1: Spec & Design (Day 1)
- [x] Competitive analysis (this document)
- [x] Design decisions documented
- [x] Key binding specification
- [x] UI layout specification

### Phase 2: Core TUI Improvements (Days 2-4)
- [ ] Implement debounced search (300ms delay)
- [ ] Add tiered fuzzy search scoring (exact → prefix → word-boundary →
  consecutive → subsequence) based on trx's approach
- [ ] Add Space to pin/unpin detail open (from search-nix)
- [ ] Add expand-all toggle (`a` key) (from search-nix)
- [ ] Add copy-to-clipboard actions (`y`/`e`/`p`) (from search-nix)
- [ ] Add help overlay (`?` key) (from search-nix)
- [ ] Add vim keybindings (`j`/`k` navigation) as alternative to arrows
- [ ] Add mouse support (from trx)
- [ ] Add search result caching for repeated queries (from trx)

### Phase 3: Theme & Polish (Days 5-6)
- [ ] Implement Catppuccin Mocha theme as default
- [ ] Add Nord and Dracula theme alternatives
- [ ] Add theme selection via command palette or config
- [ ] Improve status bar with better navigation hints
- [ ] Add loading spinner for search operations
- [ ] Add toast notifications for actions (copy, install, etc.)

### Phase 4: nixdex-Specific Features (Days 7-9)
- [ ] Implement command-not-found mode with install/run actions
- [ ] Add daemon integration for fast queries
- [ ] Add `--print0` / null-delimited output mode
- [ ] Add `--reverse` sort order support in TUI
- [ ] Add `--exclude`/`--exclude-regex` filtering in TUI
- [ ] Add `--details` mode for expanded metadata display
- [ ] Add `--quiet` mode support

### Phase 5: Testing & Validation (Day 10)
- [ ] Run `cargo test -p nixdex-tui`
- [ ] Run `cargo clippy -p nixdex-tui`
- [ ] Run `cargo fmt --check`
- [ ] Manual testing with real database
- [ ] Verify `nixdex tui` launches and renders correctly
- [ ] Test all key bindings
- [ ] Test search, locate, and which modes

## 4. Key Files to Modify

- `crates/nixdex-tui/src/app.rs` — Add debounce state, theme config,
  vim keybindings, mouse support, caching
- `crates/nixdex-tui/src/event.rs` — Add mouse event handling,
  additional key bindings
- `crates/nixdex-tui/src/tui.rs` — Add debounced search, tiered fuzzy
  scoring, Space to pin, expand-all, copy actions, help overlay
- `crates/nixdex-tui/src/ui.rs` — Add theme support, mouse rendering,
  help overlay, loading spinner, toast notifications
- `crates/nixdex-cli/src/tui.rs` — Update shim if needed
- `crates/nixdex-cli/src/bin/nixdex.rs` — Add any new CLI flags

## 5. Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| TUI complexity delays other work | Scope to core improvements first, defer advanced features |
| Fuzzy search scoring changes behavior | Keep existing frizbee-based search as default; add tiered scoring as opt-in |
| Theme system adds maintenance burden | Start with one theme (Catppuccin), add others later |
| Mouse support adds complexity | Implement basic mouse support first (click to select) |
| Debounce adds latency | Use 300ms debounce, which is standard for search UIs |

## 6. Validation Plan

1. Run `cargo test -p nixdex-tui` after each phase
2. Run `cargo clippy -p nixdex-tui` after each phase
3. Run `cargo fmt --check` after each phase
4. Manual test: `nixdex tui` launches and renders correctly
5. Manual test: All key bindings work as specified
6. Manual test: Search, locate, and which modes function correctly
7. Manual test: Theme switching works
8. Manual test: Mouse support works
9. Manual test: Copy-to-clipboard actions work
10. Verify `nixdex tui` returns same results as CLI commands