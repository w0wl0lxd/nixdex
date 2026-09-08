use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::{App, SearchDbCache, SearchOutcome, SearchRequest};
use crossterm::cursor::Show;
use crossterm::event::{
    EventStream, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
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
struct TerminalGuard {
    /// Whether `PushKeyboardEnhancementFlags` was issued, so the matching pop
    /// runs only when there is something to pop.
    keyboard_enhanced: bool,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // A failure here cannot be reported usefully: the process is on its way
        // out and the message would be drawn onto the alternate screen that is
        // being torn down. Restoring as much as possible beats aborting.
        if self.keyboard_enhanced {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

pub async fn run_tui(database: PathBuf) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // Ask the terminal to report Ctrl+H and Ctrl+J as themselves. Legacy
    // terminals send 0x08 and 0x0a for those, which decode as Backspace and
    // Enter, so the shortcuts never matched. Not every terminal supports the
    // request, hence the fallback bindings in `event.rs`.
    let keyboard_enhanced = matches!(supports_keyboard_enhancement(), Ok(true))
        && execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();
    let _terminal_guard = TerminalGuard { keyboard_enhanced };
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(database);
    app.cache_ttl = CACHE_TTL;
    let mut tick_interval = interval(Duration::from_millis(500));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

    // The search reads the database, which takes long enough to be visible.
    // Running it here would block the loop below, so no frame could be drawn
    // and no key read until it finished. A blocking worker owns the database
    // handle cache and answers requests instead; the loop only sends and
    // receives.
    let (search_tx, mut search_rx) = tokio::sync::mpsc::unbounded_channel::<SearchRequest>();
    let (outcome_tx, mut outcome_rx) = tokio::sync::mpsc::unbounded_channel::<SearchOutcome>();
    let search_handle = tokio::task::spawn_blocking(move || {
        let mut cache = SearchDbCache::new();
        while let Some(req) = search_rx.blocking_recv() {
            if req.reload {
                cache = SearchDbCache::new();
            }
            if outcome_tx.send(run_search(&mut cache, &req)).is_err() {
                break;
            }
        }
    });

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
    // `app.input` now advances on every keystroke, so it can no longer say
    // whether a query was already searched for. Track that separately.
    let mut dispatched_query: Option<String> = None;

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(app_event) => {
                        // A pinned detail owns the keyboard. Without this
                        // guard a printable character reached `set_input`,
                        // which clears `detail` and `detail_pinned`, so typing
                        // closed the pane the pin was meant to hold open.
                        if !app.detail_is_pinned() {
                            handle_input_event(
                                &mut app,
                                &app_event,
                                &mut pending_query,
                                &mut debounce_deadline,
                                &mut dispatched_query,
                            );
                        }
                        handle_event(&mut app, app_event);
                        dispatch_reload(&mut app, &search_tx);
                    }
                    None => break,
                }
            }
            _ = tick_interval.tick() => {
                app.tick();
            }
            maybe_outcome = outcome_rx.recv() => {
                if let Some(outcome) = maybe_outcome {
                    apply_search_outcome(&mut app, outcome);
                }
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
                    if dispatched_query.as_ref() != Some(&query) {
                        app.set_input(query.clone());
                        dispatch_search(&mut app, &query, &search_tx);
                        dispatched_query = Some(query);
                    }
                }
            }
        }
    }

    event_handle.abort();
    // Dropping the request sender ends the worker's `blocking_recv` loop.
    drop(search_tx);
    let _ = search_handle.await;

    // `_terminal_guard` restores raw mode, the alternate screen and the cursor
    // as it drops, on this path and on every `?` above.
    Ok(())
}

/// Answer a debounced query: from the cache when it is still warm, from the
/// worker otherwise.
fn dispatch_search(
    app: &mut App,
    query: &str,
    search_tx: &tokio::sync::mpsc::UnboundedSender<SearchRequest>,
) {
    if query.is_empty() {
        app.cancel_search();
        app.set_results(Vec::new());
        return;
    }
    if app.is_cache_valid(query)
        && let Some(cached) = app.get_cached_results(query).cloned()
    {
        app.set_results(cached);
        app.set_status(format!("Found {} result(s) (cached)", app.result_count()));
        return;
    }
    let request = app.search_request(query);
    send_search(app, request, search_tx);
}

/// Act on a Ctrl+R, which asked for the current query to be run again against
/// a freshly opened database.
fn dispatch_reload(app: &mut App, search_tx: &tokio::sync::mpsc::UnboundedSender<SearchRequest>) {
    if !app.reload_requested {
        return;
    }
    app.reload_requested = false;
    if app.input.is_empty() {
        app.set_status(String::from("Nothing to refresh"));
        return;
    }
    // Clone first: `search_request` takes `&mut self` to allocate the id.
    let input = app.input.clone();
    let mut request = app.search_request(&input);
    request.reload = true;
    send_search(app, request, search_tx);
}

fn send_search(
    app: &mut App,
    request: SearchRequest,
    search_tx: &tokio::sync::mpsc::UnboundedSender<SearchRequest>,
) {
    app.is_searching = true;
    app.active_request = Some(request.id);
    if search_tx.send(request).is_err() {
        app.cancel_search();
        app.set_status(String::from("Search worker stopped"));
    }
}

fn handle_input_event(
    app: &mut App,
    event: &AppEvent,
    pending_query: &mut Option<String>,
    debounce_deadline: &mut Option<Instant>,
    dispatched_query: &mut Option<String>,
) {
    match event {
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            // Every unmodified printable character is query text, `/`, `:` and
            // `?` included. Treating them as shortcuts dropped them from the
            // query in every mode: `bin/ls` in Locate mode searched for
            // `binls`, and `?` cannot be typed in a regex query at all.
            // Commands carry a modifier instead.
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
            // Show the character now. The debounce only delays the search; the
            // query line has to track every keystroke or the box looks frozen
            // until the user pauses.
            app.set_input(new_input.clone());
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
            app.set_input(new_input.clone());
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
            // Forget what was last dispatched. Escape and Tab clear the
            // results, so retyping the same text has to search again: the memo
            // used to suppress that dispatch and leave the screen empty.
            *dispatched_query = None;
            app.cancel_search();
        }
        _ => {}
    }
}

fn handle_event(app: &mut App, event: AppEvent) {
    if let Some(detail) = &app.detail {
        if detail.pinned {
            // A pinned detail takes the keyboard, so it has to offer a way out.
            // It used to accept only Esc and `q`, both of which close the pane
            // outright: Ctrl+D could not reach `toggle_detail_pin`, so the pin
            // the footer says to press Ctrl+D to release was one-way. `q` is
            // search input everywhere else, so it is not a command here either.
            if event.is_ctrl_d() {
                app.toggle_detail_pin();
                app.set_status(String::from("Detail unpinned"));
            } else if event.is_escape() {
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
    // Every remaining plain printable character is search input, not a
    // command. `handle_input_event` has already appended it to the query, so
    // treating it as a command key here would also fire an unrelated action --
    // typing a word containing `a`, `y`, `j` or `k` moved the selection or
    // copied a row. The shortcuts above are checked first because they are
    // characters too, and are excluded from the query for that reason.
    if event.as_char().is_some() {
        return;
    }
    if event.is_ctrl_h() {
        app.toggle_help();
        let message = if app.show_help {
            "Help overlay -- press any key to close"
        } else {
            "Help overlay closed"
        };
        app.set_status(String::from(message));
        return;
    }
    if event.is_ctrl_t() {
        app.cycle_theme();
        app.set_status(format!("Theme: {:?}", app.theme));
        return;
    }
    if event.is_escape() {
        // Esc is documented as clearing the search. It used to drop only the
        // queued keystrokes, so the committed query and its results stayed on
        // screen and there was no way to get back to an empty view.
        app.set_input(String::new());
        app.set_results(Vec::new());
        app.set_status(String::from("Search cleared"));
        return;
    }
    handle_navigation_key(app, &event);
    handle_mode_key(app, &event);
    handle_search_toggle_key(app, &event);
    handle_clipboard_key(app, &event);
    handle_enter_key(app, &event);
    handle_detail_pin_key(app, &event);
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
        // Ctrl+R used to set this message and nothing else, so the cached
        // results and the worker's open database handle both survived and a
        // rebuilt index needed a restart to show up.
        app.clear_search_cache();
        app.reload_requested = true;
        app.set_status(String::from("Refreshing..."));
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

fn handle_detail_pin_key(app: &mut App, event: &AppEvent) {
    if !event.is_ctrl_d() {
        return;
    }
    // `toggle_detail_pin` does nothing when no detail pane is open, so report
    // that instead of claiming a pin state the user cannot see.
    if app.detail.is_none() {
        app.set_status(String::from(
            "No detail pane to pin -- select a result first",
        ));
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

/// Run one search on the blocking worker thread.
///
/// Takes a settings snapshot instead of `&mut App` so it can run off the event
/// loop: the loop keeps drawing and reading keys while a database read is in
/// flight, which is what makes the "Searching..." indicator worth showing.
fn run_search(cache: &mut SearchDbCache, req: &SearchRequest) -> SearchOutcome {
    match req.mode {
        SearchMode::Search => perform_package_search(cache, req),
        SearchMode::Locate => perform_locate_search(req),
        SearchMode::Which => perform_which_search(req),
    }
}

/// Apply a finished search to the app.
///
/// A result that arrived for a query the user has already moved on from is
/// dropped: the debounce fires again for the new text, and applying the old
/// one would reset the selection to a list the user is not looking at.
fn apply_search_outcome(app: &mut App, outcome: SearchOutcome) {
    // Match on the request id, not on `(query, mode)`.  Those two do not
    // identify a request: Ctrl+R re-runs the same pair, so the pre-reload
    // outcome used to look current and stop the spinner while the reload was
    // still queued.  Escape and Tab retire a request without replacing it, and
    // its outcome then never matched, so the spinner ran forever.
    if app.active_request != Some(outcome.id) {
        // Leave `is_searching` alone. Clearing it here would hide the loading
        // indicator while the search the user is actually waiting for still
        // runs.
        return;
    }
    app.active_request = None;
    app.is_searching = false;
    if let Some(results) = outcome.results {
        app.cache_results(outcome.query, results.clone());
        app.set_results(results);
    }
    if let Some(status) = outcome.status {
        app.set_status(status);
    }
}

/// A package sidecar record as a result row.
///
/// Package search has no file entry behind it, so `path` and `size` stay empty.
fn package_meta_to_result(record: &nixdex_core::nixpkgs::PackageMeta) -> crate::app::SearchResult {
    crate::app::SearchResult {
        attr: record.attr.clone(),
        name: record.name.clone(),
        description: match record.description.as_deref() {
            Some(text) => text.to_string(),
            None => String::new(),
        },
        path: None,
        size: None,
        license: record.license.clone(),
        homepage: record.homepage.clone(),
        maintainers: match record.maintainers.as_deref() {
            Some(names) => names.to_vec(),
            None => Vec::new(),
        },
        main_program: record.main_program.clone(),
    }
}

fn perform_package_search(cache: &mut SearchDbCache, req: &SearchRequest) -> SearchOutcome {
    let query = req.query.as_str();
    let fail = |status: String| SearchOutcome {
        id: req.id,
        query: query.to_string(),
        mode: req.mode,
        results: None,
        status: Some(status),
    };

    let sidecar = req.database.join("packages.json");
    if !sidecar.exists() {
        return fail(String::from(
            "No package metadata sidecar found. Run nix-index first.",
        ));
    }

    // Package search cannot sort by size, so the request is honoured minus the
    // sort and the warning has to survive the "Found N result(s)" message.
    let size_sort_unsupported = matches!(req.sort, SearchSort::SizeAsc | SearchSort::SizeDesc);

    let sort = match req.sort {
        SearchSort::None | SearchSort::SizeAsc | SearchSort::SizeDesc => PkgSearchSort::None,
        SearchSort::AttrAsc => PkgSearchSort::Attr,
        SearchSort::Reverse => PkgSearchSort::Reverse,
    };

    let db = match cache.get_or_open(&sidecar) {
        Ok(db) => db,
        Err(err) => return fail(format!("Failed to open package database: {err}")),
    };

    let matches = if req.tiered_fuzzy {
        let fuzzy_results = db.search_fuzzy(
            query,
            req.field,
            req.case_sensitive,
            PkgSearchSort::None,
            req.limit,
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
                if let Some(limit) = req.limit {
                    scored.truncate(limit);
                }
                Ok(scored.into_iter().map(|(_, r)| r).collect())
            }
            Err(e) => Err(e),
        }
    } else if req.fuzzy {
        db.search_fuzzy(query, req.field, req.case_sensitive, sort, req.limit)
    } else {
        db.search(
            query,
            req.regex,
            req.field,
            req.case_sensitive,
            req.exact,
            sort,
            req.limit,
        )
    };

    match matches {
        Ok(records) => {
            let results: Vec<crate::app::SearchResult> =
                records.into_iter().map(package_meta_to_result).collect();
            let status = if size_sort_unsupported {
                String::from("Size sort is not available in package search mode")
            } else {
                format!("Found {} result(s)", results.len())
            };
            SearchOutcome {
                id: req.id,
                query: query.to_string(),
                mode: req.mode,
                results: Some(results),
                status: Some(status),
            }
        }
        Err(err) => fail(format!("Search error: {err}")),
    }
}

fn perform_locate_search(req: &SearchRequest) -> SearchOutcome {
    let query = req.query.as_str();
    let options = SearchOptions {
        database: req.database.clone(),
        // `nixdex-core` compiles `pattern` with `compile_search_regex`, so a
        // file name holding `.`, `+` or `[` was read as a regex: it matched too
        // much, or failed to parse. `literal_pattern` only steers the n-gram
        // candidate filter, so the escape has to happen here.
        pattern: regex::escape(query),
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
        limit: req.limit,
        count: false,
        sort: req.sort,
        min_size: None,
        max_size: None,
        exclude_fhs: false,
        null_output: false,
        quiet: false,
        details: req.details,
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
            let status = format!("Found {} result(s)", search_results.len());
            SearchOutcome {
                id: req.id,
                query: query.to_string(),
                mode: req.mode,
                results: Some(search_results),
                status: Some(status),
            }
        }
        Err(err) => SearchOutcome {
            id: req.id,
            query: query.to_string(),
            mode: req.mode,
            results: None,
            status: Some(format!("Locate error: {err}")),
        },
    }
}

fn perform_which_search(req: &SearchRequest) -> SearchOutcome {
    let query = req.query.as_str();
    let command = std::path::Path::new(query)
        .file_name()
        .and_then(|s| s.to_str())
        .map_or(query, |name| name);

    match nixdex_core::command_index::CommandIndex::open(&req.database) {
        Ok(index) => {
            let providers = match index.lookup_command(command.as_bytes()) {
                Ok(providers) => providers,
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
            let status = format!("Found {} provider(s)", results.len());
            SearchOutcome {
                id: req.id,
                query: query.to_string(),
                mode: req.mode,
                results: Some(results),
                status: Some(status),
            }
        }
        Err(_) => SearchOutcome {
            id: req.id,
            query: query.to_string(),
            mode: req.mode,
            results: None,
            status: Some(String::from(
                "Command index not available. Run nix-index first.",
            )),
        },
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
    // A non-zero exit means the copy failed. Reporting `wait()` succeeding as
    // success made every failure look like a copy, and on Linux it stopped the
    // `wl-copy` fallback from ever running after `xclip` failed.
    matches!(child.wait(), Ok(status) if status.success())
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
    use super::{
        App, AppEvent, Instant, KeyCode, KeyEvent, KeyModifiers, SearchMode, SearchOutcome,
        handle_input_event,
    };
    use crate::app::DetailView;
    use std::path::PathBuf;

    fn app() -> App {
        App::new(PathBuf::from("/nonexistent"))
    }

    fn key(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(code: KeyCode) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::CONTROL))
    }

    /// Feeds a string through the input handler one character at a time,
    /// returning the query that would be searched once the debounce fires.
    fn type_text(app: &mut App, text: &str) -> Option<String> {
        let mut pending = None;
        let mut deadline: Option<Instant> = None;
        let mut dispatched = None;
        for c in text.chars() {
            handle_input_event(
                app,
                &key(KeyCode::Char(c)),
                &mut pending,
                &mut deadline,
                &mut dispatched,
            );
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
    fn punctuation_is_query_text_not_a_shortcut() {
        // `/`, `:` and `?` were treated as commands, so they were stripped from
        // every query. Commands carry a modifier now and these are just text.
        assert_eq!(type_text(&mut app(), "a/b:c?d").as_deref(), Some("a/b:c?d"));
    }

    #[test]
    fn a_space_inside_the_pending_query_survives() {
        // The guard used to read `app.input`, which is still empty while the
        // debounce runs, so a fast "ab c" collapsed to "abc".
        assert_eq!(type_text(&mut app(), "ab c").as_deref(), Some("ab c"));
    }

    #[test]
    fn a_leading_space_is_still_dropped() {
        assert_eq!(type_text(&mut app(), " ab").as_deref(), Some("ab"));
    }

    #[test]
    fn tab_discards_the_queued_query() {
        // Tab switches mode and clears the visible input. A query left queued
        // then ran against the newly selected mode.
        let mut app = app();
        let mut pending = None;
        let mut deadline: Option<Instant> = None;
        let mut dispatched = None;
        for c in "firefox".chars() {
            handle_input_event(
                &mut app,
                &key(KeyCode::Char(c)),
                &mut pending,
                &mut deadline,
                &mut dispatched,
            );
        }
        assert_eq!(pending.as_deref(), Some("firefox"));
        handle_input_event(
            &mut app,
            &key(KeyCode::Tab),
            &mut pending,
            &mut deadline,
            &mut dispatched,
        );
        assert_eq!(pending, None);
        assert_eq!(deadline, None);
    }

    /// Ctrl+D pins the detail pane; a plain Space does not.
    ///
    /// Space used to be the pin key. Every unmodified printable character is
    /// search input now, so Space no longer reached the command handler and
    /// pinning had no key at all.
    #[test]
    fn ctrl_d_pins_the_detail_pane_and_space_does_not() {
        let mut app = app();
        app.set_detail(DetailView {
            attr: String::from("hello"),
            name: String::from("hello"),
            description: String::new(),
            path: None,
            size: None,
            license: None,
            homepage: None,
            maintainers: Vec::new(),
            main_program: None,
            pinned: false,
        });
        assert!(!app.detail_pinned);

        super::handle_key_event(&mut app, key(KeyCode::Char(' ')));
        assert!(
            !app.detail_pinned,
            "a plain Space is search input, not a command"
        );

        super::handle_key_event(&mut app, ctrl(KeyCode::Char('d')));
        assert!(app.detail_pinned, "Ctrl+D must pin the detail pane");

        super::handle_key_event(&mut app, ctrl(KeyCode::Char('d')));
        assert!(!app.detail_pinned, "Ctrl+D must unpin it again");
    }

    /// Ctrl+D with nothing selected says so rather than reporting a pin state.
    #[test]
    fn ctrl_d_without_a_detail_pane_reports_that_nothing_is_selected() {
        let mut app = app();
        super::handle_key_event(&mut app, ctrl(KeyCode::Char('d')));
        assert!(
            app.status_message.contains("select a result"),
            "unexpected status: {}",
            app.status_message
        );
        assert!(!app.detail_pinned);
    }

    fn result(attr: &str) -> crate::app::SearchResult {
        crate::app::SearchResult {
            attr: String::from(attr),
            name: String::from(attr),
            description: String::new(),
            path: None,
            size: None,
            license: None,
            homepage: None,
            maintainers: Vec::new(),
            main_program: None,
        }
    }

    fn outcome(query: &str, results: Option<Vec<crate::app::SearchResult>>) -> SearchOutcome {
        outcome_with_id(1, query, results)
    }

    fn outcome_with_id(
        id: u64,
        query: &str,
        results: Option<Vec<crate::app::SearchResult>>,
    ) -> SearchOutcome {
        SearchOutcome {
            id,
            query: String::from(query),
            mode: SearchMode::Search,
            results,
            status: Some(String::from("done")),
        }
    }

    /// A result for a request the user has moved on from must be dropped.
    ///
    /// The search runs on a worker now, so a slow answer can arrive after more
    /// typing. Applying it would reset the selection to a list that does not
    /// match what is in the search box.
    #[test]
    fn a_search_result_for_an_old_request_is_ignored() {
        let mut app = app();
        app.set_input(String::from("sqlite"));
        app.active_request = Some(2);
        super::apply_search_outcome(
            &mut app,
            outcome_with_id(1, "sql", Some(vec![result("sql")])),
        );
        assert!(app.results.is_empty(), "stale results must not be applied");
        assert_ne!(app.status_message, "done");
    }

    /// Switching mode retires the request in flight, so its answer is dropped.
    #[test]
    fn a_search_result_from_another_mode_is_ignored() {
        let mut app = app();
        app.set_input(String::from("sqlite"));
        app.active_request = Some(1);
        app.set_mode(SearchMode::Locate);
        super::apply_search_outcome(&mut app, outcome("sqlite", Some(vec![result("sqlite")])));
        assert!(app.results.is_empty());
    }

    /// The matching result is applied and cached.
    #[test]
    fn a_search_result_for_the_current_query_is_applied() {
        let mut app = app();
        app.set_input(String::from("sqlite"));
        app.active_request = Some(1);
        super::apply_search_outcome(&mut app, outcome("sqlite", Some(vec![result("sqlite")])));
        assert_eq!(app.results.len(), 1);
        assert_eq!(app.status_message, "done");
        assert_eq!(
            app.get_cached_results("sqlite").map(Vec::len),
            Some(1),
            "a successful search must fill the cache"
        );
    }

    /// A failed search reports the error and keeps the results already shown.
    #[test]
    fn a_failed_search_keeps_the_previous_results() {
        let mut app = app();
        app.set_input(String::from("sqlite"));
        app.set_results(vec![result("sqlite")]);
        app.active_request = Some(1);
        super::apply_search_outcome(&mut app, outcome("sqlite", None));
        assert_eq!(app.results.len(), 1, "a failure must not wipe the list");
        assert_eq!(app.status_message, "done");
    }

    /// The query line must track every keystroke.
    ///
    /// `app.input` was only updated when the debounce fired, so the search box
    /// showed stale text for the whole debounce window and looked frozen.
    #[test]
    fn the_query_line_updates_on_every_keystroke() {
        let mut app = app();
        let mut pending = None;
        let mut deadline: Option<Instant> = None;
        let mut dispatched = None;

        handle_input_event(
            &mut app,
            &key(KeyCode::Char('f')),
            &mut pending,
            &mut deadline,
            &mut dispatched,
        );
        assert_eq!(app.input, "f");
        handle_input_event(
            &mut app,
            &key(KeyCode::Char('o')),
            &mut pending,
            &mut deadline,
            &mut dispatched,
        );
        assert_eq!(app.input, "fo");
        handle_input_event(
            &mut app,
            &key(KeyCode::Backspace),
            &mut pending,
            &mut deadline,
            &mut dispatched,
        );
        assert_eq!(app.input, "f");
    }

    /// A stale outcome must not clear the loading indicator.
    ///
    /// `is_searching` was cleared before the staleness check, so a result for a
    /// query the user had moved on from hid the spinner while the current
    /// search was still running.
    #[test]
    fn a_stale_outcome_leaves_the_loading_indicator_alone() {
        let mut app = app();
        app.set_input(String::from("current"));
        app.is_searching = true;
        app.active_request = Some(2);

        super::apply_search_outcome(&mut app, outcome_with_id(1, "previous", Some(Vec::new())));
        assert!(
            app.is_searching,
            "a stale outcome must not stop the spinner"
        );

        super::apply_search_outcome(&mut app, outcome_with_id(2, "current", Some(Vec::new())));
        assert!(!app.is_searching, "the awaited outcome stops the spinner");
    }

    /// A reload of the same query must not be finished by the earlier answer.
    ///
    /// Staleness used to be judged on `(query, mode)`. Ctrl+R re-runs exactly
    /// that pair, so the pre-reload outcome looked current, stopped the spinner
    /// and applied results the reload was about to replace.
    #[test]
    fn a_reload_is_not_finished_by_the_pre_reload_outcome() {
        let mut app = app();
        app.set_input(String::from("firefox"));
        app.is_searching = true;
        // Request 1 is the original search; request 2 is the Ctrl+R reload.
        app.active_request = Some(2);

        super::apply_search_outcome(&mut app, outcome_with_id(1, "firefox", Some(Vec::new())));

        assert!(
            app.is_searching,
            "the pre-reload answer must not stop the spinner"
        );
        assert_eq!(app.active_request, Some(2));
    }

    /// Retiring a request without replacing it must stop the spinner.
    ///
    /// Escape and Tab used to leave `is_searching` true: the outcome still in
    /// flight no longer matched the (now empty) query, so it was dropped and
    /// nothing ever cleared the loading state.
    #[test]
    fn cancelling_a_search_stops_the_loading_indicator() {
        let mut app = app();
        app.set_input(String::from("firefox"));
        app.is_searching = true;
        app.active_request = Some(1);

        app.cancel_search();
        assert!(!app.is_searching);

        // The answer to the retired request is still dropped when it arrives.
        super::apply_search_outcome(&mut app, outcome_with_id(1, "firefox", Some(Vec::new())));
        assert!(app.results.is_empty());
        assert!(!app.is_searching);
    }

    /// Escape must let the same text be searched for again.
    ///
    /// `dispatched_query` survived Escape, so retyping the query that was just
    /// cleared matched the memo and no search was dispatched: the results
    /// stayed empty with no way to bring them back except editing the text.
    #[test]
    fn escape_lets_the_same_query_be_typed_again() {
        let mut app = app();
        let mut pending = None;
        let mut deadline: Option<Instant> = None;
        let mut dispatched = Some(String::from("firefox"));

        handle_input_event(
            &mut app,
            &key(KeyCode::Esc),
            &mut pending,
            &mut deadline,
            &mut dispatched,
        );

        assert_eq!(dispatched, None, "Escape must clear the dispatch memo");
    }

    /// Tab must do the same, for the same reason.
    #[test]
    fn tab_lets_the_same_query_be_typed_again() {
        let mut app = app();
        let mut pending = None;
        let mut deadline: Option<Instant> = None;
        let mut dispatched = Some(String::from("firefox"));

        handle_input_event(
            &mut app,
            &key(KeyCode::Tab),
            &mut pending,
            &mut deadline,
            &mut dispatched,
        );

        assert_eq!(dispatched, None);
    }

    /// Typing must not close a pinned detail pane.
    ///
    /// `set_input` clears `detail` and `detail_pinned`, and the input handler
    /// ran before the detail handler with no guard, so one keystroke dismissed
    /// the pane the pin was meant to hold open.
    #[test]
    fn a_pinned_detail_takes_the_keyboard() {
        let mut app = app();
        app.set_detail(DetailView {
            attr: String::from("firefox"),
            name: String::from("firefox"),
            description: String::new(),
            path: None,
            size: None,
            license: None,
            homepage: None,
            maintainers: Vec::new(),
            main_program: None,
            pinned: true,
        });

        assert!(
            app.detail_is_pinned(),
            "the guard the event loop reads must report the pin"
        );
    }

    /// A path typed in Locate mode must keep its separators.
    ///
    /// `/`, `:` and `?` were treated as shortcuts and dropped from the query in
    /// every mode, so `bin/ls` searched for `binls`.
    #[test]
    fn punctuation_stays_in_the_query() {
        // A fresh app per case: `app.input` now tracks every keystroke, so a
        // second query typed into the same app would append to the first.
        for text in ["bin/ls", "a:b?c"] {
            let mut app = app();
            app.set_mode(SearchMode::Locate);
            assert_eq!(type_text(&mut app, text), Some(String::from(text)));
        }
    }

    /// Help moved to Ctrl+H because `?` is query text now.
    #[test]
    fn ctrl_h_toggles_the_help_overlay() {
        let mut app = app();
        super::handle_key_event(&mut app, ctrl(KeyCode::Char('h')));
        assert!(app.show_help, "Ctrl+H must open the help overlay");
        super::handle_key_event(&mut app, key(KeyCode::Char('x')));
        assert!(!app.show_help, "any key must close it again");
    }

    /// Esc clears the committed query and its results, not just the queued keys.
    #[test]
    fn escape_clears_the_search_and_the_results() {
        let mut app = app();
        app.set_input(String::from("sqlite"));
        app.set_results(vec![result("sqlite")]);
        super::handle_key_event(&mut app, key(KeyCode::Esc));
        assert!(app.input.is_empty(), "Esc must clear the query");
        assert!(app.results.is_empty(), "Esc must clear the results");
    }

    /// A pinned detail must be releasable with the key the help advertises.
    #[test]
    fn ctrl_d_unpins_a_pinned_detail() {
        let mut app = app();
        app.set_detail(DetailView {
            attr: String::from("hello"),
            name: String::from("hello"),
            description: String::new(),
            path: None,
            size: None,
            license: None,
            homepage: None,
            maintainers: Vec::new(),
            main_program: None,
            pinned: true,
        });
        app.detail_pinned = true;

        super::handle_event(&mut app, ctrl(KeyCode::Char('d')));
        assert!(!app.detail_pinned, "Ctrl+D must release the pin");
        assert!(
            app.detail.is_some(),
            "unpinning must not close the detail pane"
        );
    }

    /// Ctrl+R must drop the cached results and ask for a fresh search.
    ///
    /// It used to set a "Refreshing..." status and change nothing else, so a
    /// rebuilt index needed a restart of the TUI to show up.
    #[test]
    fn ctrl_r_clears_the_cache_and_asks_for_a_reload() {
        let mut app = app();
        app.set_input(String::from("sqlite"));
        app.cache_results(String::from("sqlite"), vec![result("sqlite")]);
        assert!(app.get_cached_results("sqlite").is_some());

        super::handle_key_event(&mut app, ctrl(KeyCode::Char('r')));
        assert!(app.reload_requested, "Ctrl+R must request a reload");
        assert!(
            app.get_cached_results("sqlite").is_none(),
            "Ctrl+R must drop the cached results"
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        super::dispatch_reload(&mut app, &tx);
        let request = rx.try_recv().expect("a search must be dispatched");
        assert!(request.reload, "the worker must reopen the database");
        assert_eq!(request.query, "sqlite");
        assert!(!app.reload_requested, "the flag must be cleared");
    }

    /// Ctrl+R with an empty search box has nothing to re-run.
    #[test]
    fn ctrl_r_with_no_query_dispatches_nothing() {
        let mut app = app();
        app.reload_requested = true;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        super::dispatch_reload(&mut app, &tx);
        assert!(rx.try_recv().is_err(), "no search should be sent");
        assert_eq!(app.status_message, "Nothing to refresh");
    }
}
