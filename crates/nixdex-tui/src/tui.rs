use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::App;
use crossterm::cursor::Show;
use crossterm::event::{
    EventStream, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use nixdex_core::database::{SearchOptions, SearchSort};
use nixdex_core::package_search::SearchSort as PkgSearchSort;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::time::{interval, sleep};

use crate::app::DetailView;
use crate::app::SearchMode;
use crate::event::AppEvent;
use crate::ui;

const DEBOUNCE_DELAY: Duration = Duration::from_millis(300);
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Restores the terminal when the TUI exits, however it exits.
///
/// Raw mode and the alternate screen are entered before the event loop, and the
/// loop propagates I/O errors with `?` -- `terminal.draw` runs every frame.
/// Restoring only after a normal `break` therefore left the terminal in raw
/// mode, on the alternate screen and with no cursor on every error path, which
/// the user could escape only with a manual `reset`.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // A failure here cannot be reported usefully: the process is on its way
        // out and the message would be drawn onto the alternate screen that is
        // being torn down. Restoring as much as possible beats aborting.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

pub async fn run_tui(database: PathBuf) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let _terminal_guard = TerminalGuard;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(database);
    app.cache_ttl = CACHE_TTL;
    let mut tick_interval = interval(Duration::from_millis(500));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

    // `crossterm::event::read()` blocks the calling thread until a key arrives,
    // which ties up a runtime worker for the whole session. `EventStream` is
    // the async reader for the same source and the `event-stream` feature is
    // already enabled.
    let event_handle = tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(Ok(event)) = events.next().await {
            let app_event = AppEvent::from(event);
            let quit = app_event.is_quit();
            let _ = tx.send(app_event);
            if quit {
                break;
            }
        }
    });

    let mut debounce_deadline: Option<Instant> = None;
    let mut pending_query: Option<String> = None;

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(app_event) => {
                        handle_input_event(&app, &app_event, &mut pending_query, &mut debounce_deadline);
                        handle_event(&mut app, app_event);
                    }
                    None => break,
                }
            }
            _ = tick_interval.tick() => {
                app.tick();
            }
            () = async {
                if let Some(deadline) = debounce_deadline {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return;
                    }
                    sleep(remaining).await;
                } else {
                    sleep(Duration::from_secs(60)).await;
                }
            } => {
                if let Some(query) = pending_query.take() {
                    debounce_deadline = None;
                    if query != app.input {
                        app.set_input(query.clone());
                        // Draw once with the flag set. The search is
                        // synchronous, so setting and clearing the flag around
                        // it inside one `select!` branch meant no frame was
                        // ever rendered while it was true and the loading
                        // indicator was unreachable.
                        app.is_searching = true;
                        terminal.draw(|frame| ui::render(frame, &app))?;
                        perform_search(&mut app, &query);
                        app.is_searching = false;
                    }
                }
            }
        }
    }

    event_handle.abort();

    // `_terminal_guard` restores raw mode, the alternate screen and the cursor
    // as it drops, on this path and on every `?` above.
    Ok(())
}

fn handle_input_event(
    app: &App,
    event: &AppEvent,
    pending_query: &mut Option<String>,
    debounce_deadline: &mut Option<Instant>,
) {
    match event {
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            // `/`, `:` and `?` are documented shortcuts, so they are commands
            // rather than query text. Letting them through here both typed
            // them into the search box and left the shortcuts unreachable.
            if matches!(c, '/' | ':' | '?') {
                return;
            }
            let mut new_input = pending_query.take().unwrap_or_else(|| app.input.clone());
            // Drop a leading space. The guard has to read the pending buffer,
            // not `app.input`: while the debounce is still running `app.input`
            // is empty even though characters are already queued, so typing
            // "ab c" quickly used to lose the space and produce "abc".
            if new_input.is_empty() && *c == ' ' {
                *pending_query = Some(new_input);
                return;
            }
            new_input.push(*c);
            *pending_query = Some(new_input);
            *debounce_deadline = Some(Instant::now() + DEBOUNCE_DELAY);
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Backspace,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            let mut new_input = pending_query.take().unwrap_or_else(|| app.input.clone());
            new_input.pop();
            *pending_query = Some(new_input);
            *debounce_deadline = Some(Instant::now() + DEBOUNCE_DELAY);
        }
        // Esc clears the query. Tab switches mode, which clears the visible
        // input, so the queued query has to go with it: leaving it in place ran
        // the previous mode's text against the newly selected mode once the
        // debounce fired.
        AppEvent::Key(KeyEvent {
            code: KeyCode::Esc | KeyCode::Tab,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            *pending_query = None;
            *debounce_deadline = None;
        }
        _ => {}
    }
}

fn handle_event(app: &mut App, event: AppEvent) {
    if let Some(detail) = &app.detail {
        if detail.pinned {
            if matches!(
                event,
                AppEvent::Key(
                    KeyEvent {
                        code: KeyCode::Esc,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } | KeyEvent {
                        code: KeyCode::Char('q'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    }
                )
            ) {
                app.close_detail();
            }
            return;
        }
        handle_detail_event(app, event);
        return;
    }

    match &event {
        AppEvent::Key(_) => handle_key_event(app, event),
        AppEvent::Mouse(_) => handle_mouse_event(app, event),
        _ => {}
    }
}

fn handle_key_event(app: &mut App, event: AppEvent) {
    if app.show_help {
        app.show_help = false;
        app.set_status(String::from("Help overlay closed"));
        return;
    }
    if event.is_quit() {
        app.set_status(String::from("Quitting..."));
        return;
    }
    if event.is_slash() {
        app.set_status(String::from(
            "Focus: search input — type to search, Esc to clear",
        ));
        return;
    }
    if event.is_colon() {
        app.set_status(String::from("Command palette — not yet implemented"));
        return;
    }
    if event.is_question() {
        app.toggle_help();
        if app.show_help {
            app.set_status(String::from("Help overlay — press ? to close"));
        } else {
            app.set_status(String::from("Help overlay closed"));
        }
        return;
    }
    // Every remaining plain printable character is search input, not a
    // command. `handle_input_event` has already appended it to the query, so
    // treating it as a command key here would also fire an unrelated action --
    // typing a word containing `a`, `y`, `j` or `k` moved the selection or
    // copied a row. The shortcuts above are checked first because they are
    // characters too, and are excluded from the query for that reason.
    if event.as_char().is_some() {
        return;
    }
    if event.is_ctrl_t() {
        app.cycle_theme();
        app.set_status(format!("Theme: {:?}", app.theme));
        return;
    }
    handle_navigation_key(app, &event);
    handle_mode_key(app, &event);
    handle_search_toggle_key(app, &event);
    handle_clipboard_key(app, &event);
    handle_enter_key(app, &event);
    handle_space_key(app, &event);
}

fn handle_navigation_key(app: &mut App, event: &AppEvent) {
    if event.is_up() {
        app.select_prev();
    } else if event.is_down() {
        app.select_next();
    } else if event.is_page_up() {
        app.page_up();
    } else if event.is_page_down() {
        app.page_down();
    } else if event.is_home() {
        app.selected = 0;
        app.scroll = 0;
    } else if event.is_end() {
        app.selected = app.result_count().saturating_sub(1);
        app.ensure_visible();
    }
}

fn handle_mode_key(app: &mut App, event: &AppEvent) {
    if event.is_tab() {
        let next_mode = match app.mode {
            SearchMode::Search => SearchMode::Locate,
            SearchMode::Locate => SearchMode::Which,
            SearchMode::Which => SearchMode::Search,
        };
        app.set_mode(next_mode);
        app.set_status(format!(
            "Switched to {} mode",
            match next_mode {
                SearchMode::Search => "search",
                SearchMode::Locate => "locate",
                SearchMode::Which => "which",
            }
        ));
    }
}

fn handle_search_toggle_key(app: &mut App, event: &AppEvent) {
    if event.is_ctrl_r() {
        app.set_status(String::from("Refreshing..."));
    } else if event.is_ctrl_n() {
        app.search_quiet = !app.search_quiet;
        app.set_status(format!(
            "Quiet mode {}",
            if app.search_quiet { "on" } else { "off" }
        ));
    } else if event.is_ctrl_j() {
        app.search_json = !app.search_json;
        app.set_status(format!(
            "JSON output {}",
            if app.search_json { "on" } else { "off" }
        ));
    } else if event.is_ctrl_a() {
        app.toggle_expand_all();
        app.set_status(format!(
            "Expand all {}",
            if app.expand_all { "on" } else { "off" }
        ));
    }
}

fn handle_clipboard_key(app: &mut App, event: &AppEvent) {
    if !event.is_ctrl_y() && !event.is_ctrl_e() && !event.is_ctrl_p() {
        return;
    }
    let Some(result) = app.selected_result() else {
        return;
    };
    if event.is_ctrl_y() {
        let attr = result.attr.clone();
        let copied = copy_to_clipboard(&attr);
        app.add_toast(clipboard_toast(copied, &format!("Copied: {attr}")));
    } else if event.is_ctrl_e() {
        let cmd = format!("nix-env -iA nixpkgs.{}", result.attr);
        let copied = copy_to_clipboard(&cmd);
        app.add_toast(clipboard_toast(copied, "Copied install command"));
    } else if event.is_ctrl_p() {
        let cmd = format!("nix profile install nixpkgs#{}", result.attr);
        let copied = copy_to_clipboard(&cmd);
        app.add_toast(clipboard_toast(copied, "Copied profile command"));
    }
}

/// Build a toast message for a clipboard action based on whether it succeeded.
fn clipboard_toast(copied: bool, success_message: &str) -> String {
    if copied {
        success_message.to_string()
    } else {
        String::from("Clipboard unavailable (no xclip/wl-copy/pbcopy/clip found)")
    }
}

fn handle_enter_key(app: &mut App, event: &AppEvent) {
    if !event.is_enter() {
        return;
    }
    let Some(result) = app.selected_result() else {
        return;
    };
    let detail = DetailView {
        attr: result.attr.clone(),
        name: result.name.clone(),
        description: result.description.clone(),
        path: result.path.clone(),
        size: result.size,
        license: result.license.clone(),
        homepage: result.homepage.clone(),
        maintainers: result.maintainers.clone(),
        main_program: result.main_program.clone(),
        pinned: false,
    };
    app.set_detail(detail);
}

fn handle_space_key(app: &mut App, event: &AppEvent) {
    if !event.is_space() {
        return;
    }
    app.toggle_detail_pin();
    app.set_status(format!(
        "Detail {}",
        if app.detail_pinned {
            "pinned"
        } else {
            "unpinned"
        }
    ));
}

fn handle_mouse_event(app: &mut App, event: AppEvent) {
    match event {
        AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            row,
            ..
        }) => {
            let result_count = app.result_count();
            if result_count > 0 {
                // The list is scrolled, so a screen row maps to
                // `app.scroll + offset`, not to the offset alone. Without the
                // scroll term a click after scrolling selected a different
                // entry than the one under the pointer.
                let first_result_row = 2u16;
                let row_offset = usize::from(row.saturating_sub(first_result_row));
                let index = usize::from(app.scroll).saturating_add(row_offset);
                if index < result_count {
                    app.selected = index;
                    app.ensure_visible();
                }
            }
        }
        AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..
        }) => {
            app.select_next();
        }
        AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            ..
        }) => {
            app.select_prev();
        }
        _ => {}
    }
}

fn handle_detail_event(app: &mut App, event: AppEvent) {
    match event {
        AppEvent::Key(
            KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                ..
            },
        ) => {
            app.close_detail();
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char(' '),
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.toggle_detail_pin();
            app.set_status(format!(
                "Detail {}",
                if app.detail_pinned {
                    "pinned"
                } else {
                    "unpinned"
                }
            ));
        }
        _ => {}
    }
}

fn perform_search(app: &mut App, query: &str) {
    if query.is_empty() {
        app.set_results(Vec::new());
        return;
    }

    if app.is_cache_valid(query)
        && let Some(cached) = app.get_cached_results(query)
    {
        app.set_results(cached.clone());
        app.set_status(format!("Found {} result(s) (cached)", app.result_count()));
        return;
    }

    match app.mode {
        SearchMode::Search => {
            perform_package_search(app, query);
        }
        SearchMode::Locate => {
            perform_locate_search(app, query);
        }
        SearchMode::Which => {
            perform_which_search(app, query);
        }
    }
}

fn perform_package_search(app: &mut App, query: &str) {
    let sidecar = app.database.join("packages.json");
    if !sidecar.exists() {
        app.set_status(String::from(
            "No package metadata sidecar found. Run nix-index first.",
        ));
        return;
    }

    let size_sort_unsupported =
        matches!(app.search_sort, SearchSort::SizeAsc | SearchSort::SizeDesc);
    if size_sort_unsupported {
        app.set_status("Size sort is not available in package search mode".to_string());
    }

    let sort = match app.search_sort {
        SearchSort::None | SearchSort::SizeAsc | SearchSort::SizeDesc => PkgSearchSort::None,
        SearchSort::AttrAsc => PkgSearchSort::Attr,
        SearchSort::Reverse => PkgSearchSort::Reverse,
    };

    let db = match app.search_db_cache.get_or_open(&sidecar) {
        Ok(db) => db,
        Err(err) => {
            app.set_status(format!("Failed to open package database: {}", err));
            return;
        }
    };

    let matches = if app.search_tiered_fuzzy {
        let fuzzy_results = db.search_fuzzy(
            query,
            app.search_field,
            app.search_case_sensitive,
            PkgSearchSort::None,
            app.search_limit,
        );
        match fuzzy_results {
            Ok(records) => {
                let mut scored: Vec<_> = records
                    .into_iter()
                    .map(|r| {
                        let score = crate::app::fuzzy_score(query, &r.attr);
                        (score, r)
                    })
                    .collect();
                scored.sort_by(|(score_a, a), (score_b, b)| {
                    score_b.cmp(score_a).then_with(|| a.attr.cmp(&b.attr))
                });
                if let Some(limit) = app.search_limit {
                    scored.truncate(limit);
                }
                Ok(scored.into_iter().map(|(_, r)| r).collect())
            }
            Err(e) => Err(e),
        }
    } else if app.search_fuzzy {
        db.search_fuzzy(
            query,
            app.search_field,
            app.search_case_sensitive,
            sort,
            app.search_limit,
        )
    } else {
        db.search(
            query,
            app.search_regex,
            app.search_field,
            app.search_case_sensitive,
            app.search_exact,
            sort,
            app.search_limit,
        )
    };

    match matches {
        Ok(records) => {
            let results: Vec<crate::app::SearchResult> = records
                .into_iter()
                .map(|r| crate::app::SearchResult {
                    attr: r.attr.clone(),
                    name: r.name.clone(),
                    #[allow(clippy::unnecessary_lazy_evaluations)]
                    description: r.description.as_deref().unwrap_or_else(|| "").to_string(),
                    path: None,
                    size: None,
                    license: r.license.clone(),
                    homepage: r.homepage.clone(),
                    #[allow(clippy::unnecessary_lazy_evaluations)]
                    maintainers: r.maintainers.as_deref().unwrap_or_else(|| &[]).to_vec(),
                    main_program: r.main_program.clone(),
                })
                .collect();
            app.cache_results(query.to_string(), results.clone());
            app.set_results(results);
            if !size_sort_unsupported {
                app.set_status(format!("Found {} result(s)", app.result_count()));
            }
        }
        Err(err) => {
            app.set_status(format!("Search error: {}", err));
        }
    }
}

fn perform_locate_search(app: &mut App, query: &str) {
    let options = SearchOptions {
        database: app.database.clone(),
        pattern: query.to_string(),
        hash: None,
        package_pattern: None,
        exact_basename: None,
        exact_path: None,
        path_prefix: None,
        literal_pattern: Some(query.to_string()),
        file_type: &[],
        mode: nixdex_core::database::SearchMode::Minimal,
        json: false,
        yaml: false,
        limit: app.search_limit,
        count: false,
        sort: app.search_sort,
        min_size: None,
        max_size: None,
        exclude_fhs: false,
        null_output: false,
        quiet: app.search_quiet,
        details: app.search_details,
    };

    match nixdex_core::database::search_results(&options, None) {
        Ok(results) => {
            let search_results: Vec<crate::app::SearchResult> = results
                .into_iter()
                .map(|(store_path, entry)| {
                    let size = match &entry.node {
                        nixdex_core::files::FileNode::Regular { size, .. } => Some(*size),
                        _ => None,
                    };
                    crate::app::SearchResult {
                        attr: store_path.origin().attr.clone(),
                        name: store_path.origin().output.clone(),
                        description: String::new(),
                        path: Some(String::from_utf8_lossy(&entry.path).to_string()),
                        size,
                        license: None,
                        homepage: None,
                        maintainers: Vec::new(),
                        main_program: None,
                    }
                })
                .collect();
            app.cache_results(query.to_string(), search_results.clone());
            app.set_results(search_results);
            app.set_status(format!("Found {} result(s)", app.result_count()));
        }
        Err(err) => {
            app.set_status(format!("Locate error: {}", err));
        }
    }
}

#[allow(clippy::unnecessary_lazy_evaluations)]
fn perform_which_search(app: &mut App, query: &str) {
    let command = std::path::Path::new(query)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| query);

    match nixdex_core::command_index::CommandIndex::open(&app.database) {
        Ok(index) => {
            let providers = match index.lookup_command(command.as_bytes()) {
                Ok(p) => p,
                Err(_) => Vec::new(),
            };
            let results: Vec<crate::app::SearchResult> = providers
                .into_iter()
                .map(|p| crate::app::SearchResult {
                    attr: p.attr,
                    name: p.output,
                    description: String::new(),
                    path: None,
                    size: None,
                    license: None,
                    homepage: None,
                    maintainers: Vec::new(),
                    main_program: None,
                })
                .collect();
            app.cache_results(query.to_string(), results.clone());
            app.set_results(results);
            app.set_status(format!("Found {} provider(s)", app.result_count()));
        }
        Err(_) => {
            app.set_status(String::from(
                "Command index not available. Run nix-index first.",
            ));
        }
    }
}

/// Spawn `command` (with optional args), feed `text` to its stdin, and wait for
/// it to finish. Returns `true` if the copy succeeded.
fn pipe_to_clipboard_command(command: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = std::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
        // Drop stdin so the child sees EOF before we wait on it.
        drop(stdin);
    }
    child.wait().is_ok()
}

/// Copy `text` to the system clipboard. Returns `true` if a clipboard backend
/// accepted the text. Never writes to stdout/stderr, which would corrupt the
/// TUI while the terminal is in raw mode + the alternate screen.
fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        pipe_to_clipboard_command("xclip", &["-selection", "clipboard"], text)
            || pipe_to_clipboard_command("wl-copy", &[], text)
    }
    #[cfg(target_os = "macos")]
    {
        pipe_to_clipboard_command("pbcopy", &[], text)
    }
    #[cfg(windows)]
    {
        pipe_to_clipboard_command("clip", &[], text)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = text;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{App, AppEvent, Instant, KeyCode, KeyEvent, KeyModifiers, handle_input_event};
    use std::path::PathBuf;

    fn app() -> App {
        App::new(PathBuf::from("/nonexistent"))
    }

    fn key(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Feeds a string through the input handler one character at a time,
    /// returning the query that would be searched once the debounce fires.
    fn type_text(app: &App, text: &str) -> Option<String> {
        let mut pending = None;
        let mut deadline: Option<Instant> = None;
        for c in text.chars() {
            handle_input_event(app, &key(KeyCode::Char(c)), &mut pending, &mut deadline);
        }
        pending
    }

    #[test]
    fn a_typed_q_is_not_a_quit() {
        // The search box always has focus, so `q` is a character. Treating it
        // as quit closed the application mid-word.
        assert!(!key(KeyCode::Char('q')).is_quit());
    }

    #[test]
    fn ctrl_c_is_still_a_quit() {
        assert!(AppEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)).is_quit());
    }

    #[test]
    fn shortcut_characters_stay_out_of_the_query() {
        // `/`, `:` and `?` are documented shortcuts. Appending them here both
        // typed them into the box and left the shortcuts unreachable.
        assert_eq!(type_text(&app(), "a/b:c?d").as_deref(), Some("abcd"));
    }

    #[test]
    fn a_space_inside_the_pending_query_survives() {
        // The guard used to read `app.input`, which is still empty while the
        // debounce runs, so a fast "ab c" collapsed to "abc".
        assert_eq!(type_text(&app(), "ab c").as_deref(), Some("ab c"));
    }

    #[test]
    fn a_leading_space_is_still_dropped() {
        assert_eq!(type_text(&app(), " ab").as_deref(), Some("ab"));
    }

    #[test]
    fn tab_discards_the_queued_query() {
        // Tab switches mode and clears the visible input. A query left queued
        // then ran against the newly selected mode.
        let app = app();
        let mut pending = None;
        let mut deadline: Option<Instant> = None;
        for c in "firefox".chars() {
            handle_input_event(&app, &key(KeyCode::Char(c)), &mut pending, &mut deadline);
        }
        assert_eq!(pending.as_deref(), Some("firefox"));
        handle_input_event(&app, &key(KeyCode::Tab), &mut pending, &mut deadline);
        assert_eq!(pending, None);
        assert_eq!(deadline, None);
    }
}
