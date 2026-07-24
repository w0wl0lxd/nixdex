# nixdex-tui Refinement Plan

## Purpose

Refine the nixdex TUI based on research of top TUI projects (yazi, gitui, nixmate, ratatui-themes, Monospace Design TUI) and best practices for layout, widgets, and interaction patterns.

## 1. Research Findings

### 1.1 Top TUI Layout Patterns

**yazi** (40k+ stars):
- Three-pane layout: file list (left), preview (right), status bar (bottom)
- Bottom-up command bar with async input
- Rounded border style for popups
- Theme system with semantic color roles (input, normal, selected, etc.)
- Uses `ratatui::layout::Layout` with `Constraint::Percentage` for pane splits
- Partial redraw: only redraws changed regions using `draw_partial`

**gitui** (22k+ stars):
- Tab-based navigation with `Tabs` widget
- Master-detail layout: file list on left, diff on right
- Command bar at bottom with prompt
- Semantic theming: `selected_bg`, `selection_fg`, `disabled_fg`, `command_fg`
- Uses `Rc<Theme>` for shared theme state
- Context-based help (no need to memorize keys)

**nixmate** (NixOS-specific TUI):
- Module-based navigation (number keys 1-0 switch modules)
- Sub-tab navigation with `[`/`]`
- j/k navigation, g/G top/bottom
- `/` for search, `?` for help
- 13 themes including Gruvbox, Nord, Catppuccin, Dracula, Tokyo Night

**Monospace Design TUI** (design system):
- Standardized keyboard conventions across apps
- Master-detail layout pattern
- Expand-to-focus behavior
- Command palette pattern
- Named palettes with semantic color roles
- Footer command bar pattern

### 1.2 Top Widget Libraries

**ratatui-themes** (crates.io):
- 15+ built-in themes with semantic color palettes
- `ThemePalette` provides: `accent`, `secondary`, `bg`, `fg`, `muted`, `selection`, `error`, `warning`, `success`, `info`
- Theme cycling with `next()`/`prev()`
- Light/dark detection
- Default theme: Dracula

**ratkit** (alpha-innovation-labs):
- Component-based architecture with `Component` trait
- Built-in widgets: TreeView, ResizableGrid, Dialog, Toast, Button, Pane, MenuBar, StatusLine, Scroll
- Markdown preview, AI chat, code diff, file system tree
- Feature-gated components

**ratatui-bubbletea** (akitaonrails):
- Charm's Bubble Tea look and feel for ratatui
- Spinner with 12 preset types
- Text input with validation
- Help view with short/full modes
- Tree list with collapsible groups
- Paginated list with item delegation
- Semantic color palette with 5 built-in presets

**ono** (nullorder):
- Themeable widgets as a library
- Components: splash, boot, dashboard, statusbar
- Elements: progress, spinner, box
- Four built-in themes: Forest (default), Retro, Minimal, Cyber

### 1.3 Key Design Decisions from Research

1. **Layout**: Use `Constraint::Percentage` for responsive splits (yazi pattern)
2. **Theming**: Semantic color roles (bg, fg, accent, selection, muted, error, warning, success, info) — not raw RGB values
3. **Navigation**: j/k + arrow keys, g/G for top/bottom, `/` for search focus
4. **Help**: `?` key toggles help overlay (standard across all top TUIs)
5. **Command bar**: Bottom-aligned with prompt (yazi, gitui pattern)
6. **Partial redraw**: Only redraw changed regions for performance
7. **Focus management**: Visual indicator for focused element
8. **Border style**: Rounded borders for popups/overlays (yazi pattern)

## 2. Refinement Tasks

### 2.1 Layout Improvements

- [ ] Replace fixed `Constraint::Length(1)` header/footer with percentage-based layout
- [ ] Add `Constraint::Min(1)` for the main content area to ensure it fills available space
- [ ] Implement responsive layout that adapts to terminal size
- [ ] Add `Margin` for padding around the main content area
- [ ] Consider a two-column layout for search results (attr + name + description)

### 2.2 Theme System Overhaul

- [ ] Replace custom `ThemeColors` struct with `ratatui_themes` crate integration
- [ ] Use `ThemePalette` semantic colors (accent, secondary, bg, fg, muted, selection, error, warning, success, info)
- [ ] Add theme cycling with `next()`/`prev()` methods
- [ ] Add light/dark detection for auto theme selection
- [ ] Keep the 4 theme variants (TokyoNight, CatppuccinMocha, Nord, Dracula) but map them to semantic roles
- [ ] Add `ThemePicker` widget for in-app theme selection

### 2.3 Widget Improvements

- [ ] Add `Scrollbar` widget to results list (ratatui built-in)
- [ ] Add `Table` widget for multi-column result display (attr, name, description)
- [ ] Add `Tabs` widget for mode switching (Search/Locate/Which) instead of Tab key
- [ ] Add `Paragraph` with `Wrap` for long descriptions in detail view
- [ ] Add `Gauge` or `Spinner` widget for loading state
- [ ] Add `Block` with rounded borders for popups and overlays

### 2.4 Interaction Patterns

- [ ] Add `g`/`G` keys for jumping to first/last result (nixmate pattern)
- [ ] Add `/` key to focus search input (standard across all TUIs)
- [ ] Add `?` key to toggle help overlay (already partially implemented)
- [ ] Add `Ctrl+L` to clear screen/redraw
- [ ] Add `Ctrl+G` to go to top, `Ctrl+T` to go to bottom
- [ ] Implement expand-to-focus: clicking a result focuses it
- [ ] Add visual focus indicator (not just selected highlight)

### 2.5 Help Overlay

- [ ] Implement full help overlay as a modal popup
- [ ] Group keys by category: Navigation, Search, Actions, Modes
- [ ] Use rounded borders and semantic colors
- [ ] Show key bindings with descriptions in a two-column layout

### 2.6 Status Bar Enhancement

- [ ] Add `StatusLine` widget (powerline-style) from ratkit
- [ ] Show mode, result count, sort order, and filter state
- [ ] Add keyboard shortcut hints in footer (like nixmate)

### 2.7 Performance

- [ ] Implement partial redraw: only redraw changed regions
- [ ] Use `Terminal::draw` with `CompletedFrame` for frame reuse
- [ ] Cache rendered widgets where possible
- [ ] Reduce unnecessary redraws on tick events

### 2.8 Code Architecture

- [ ] Separate `ui/` module into submodules: `header.rs`, `body.rs`, `footer.rs`, `detail.rs`, `help.rs`, `toast.rs`
- [ ] Extract theme application into a `theme.rs` module
- [ ] Extract layout calculations into a `layout.rs` module
- [ ] Consider component-based architecture (like ratkit) for complex widgets

## 3. Layout Specification

```
┌─────────────────────────────────────────────────────┐
│ HEADER: [MODE Badge] [Search Input / Prompt]        │
├─────────────────────────────────────────────────────┤
│                                                     │
│  MAIN CONTENT (flexible, min 1 row)                 │
│  ┌─────────────────────────────────────────────┐    │
│  │  Results List / Detail View / Help Overlay  │    │
│  │  (scrollable with Scrollbar)                │    │
│  └─────────────────────────────────────────────┘    │
│                                                     │
├─────────────────────────────────────────────────────┤
│ FOOTER: [Status Message] [Result Count] [Mode] [Key Hints] │
└─────────────────────────────────────────────────────┘
```

## 4. Theme Palette (Semantic)

Using `ratatui_themes` `ThemePalette` semantic roles:

| Role | Purpose | Example |
|------|---------|---------|
| `bg` | Main background | Terminal background |
| `fg` | Primary text | Default text color |
| `accent` | Primary accent | Mode badge, links |
| `secondary` | Secondary accent | Borders, dim text |
| `muted` | Dimmed text | Placeholders, hints |
| `selection` | Selected item bg | Highlighted row |
| `error` | Error states | Error messages |
| `warning` | Warning states | Status warnings |
| `success` | Success states | Toast notifications |
| `info` | Info states | Help text |

## 5. Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| `ratatui_themes` adds dependency | It's a lightweight crate with no transitive deps beyond ratatui |
| Layout refactoring breaks existing functionality | Keep existing layout as fallback, refactor incrementally |
| Theme system change breaks existing code | Maintain backward-compatible `Theme` enum, just change the rendering |
| Partial redraw complexity | Start with full redraw, add partial redraw as optimization later |

## 6. Validation Plan

1. `cargo +nightly check -p nixdex-tui` after each task group
2. `cargo +nightly clippy -p nixdex-tui` after each task group
3. `cargo +nightly fmt` after each task group
4. Manual test: `nixdex tui` launches and renders correctly
5. Verify all key bindings work as specified
6. Verify theme switching works
7. Verify help overlay renders correctly
8. Verify partial redraw doesn't cause visual artifacts

## 7. Open Questions

1. Should we add `ratatui_themes` as a dependency or keep the custom theme system?
   - Recommendation: Add `ratatui_themes` for semantic color roles, but keep custom themes for the specific color palettes
2. Should we implement the component-based architecture from ratkit?
   - Recommendation: Not yet — the current architecture works well. Consider for a future refactor.
3. Should we add `nixmate`-style module navigation (number keys 1-0)?
   - Recommendation: No — nixdex has only 3 modes (Search/Locate/Which), which are already accessible via Tab
4. Should we implement the `draw_partial` optimization from yazi?
   - Recommendation: Yes, but as a separate task after the layout and theme refactoring is complete