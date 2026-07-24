use std::io;
use std::path::PathBuf;

use app::App;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use nixdex_core::database::{SearchOptions, SearchSort};
use nixdex_core::package_search::{SearchDb, SearchField, SearchSort as PkgSearchSort};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::time::{interval, Duration};

use crate::app::DetailView;
use crate::app::SearchMode;
use crate::event::AppEvent;

pub async fn run_tui(database: PathBuf) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(database);
    let mut tick_interval = interval(Duration::from_millis(500));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

    let event_handle = tokio::spawn(async move {
        loop {
            match crossterm::event::read() {
                Ok(event) => {
                    let app_event = AppEvent::from(event);
                    if app_event.is_quit() {
                        let _ = tx.send(app_event);
                        break;
                    }
                    let _ = tx.send(app_event);
                }
                Err(_) => break,
            }
        }
    });

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        tokio::select! {
            maybe_event = rx.recv() => {
                match maybe_event {
                    Some(app_event) => {
                        handle_event(&mut app, app_event);
                    }
                    None => break,
                }
            }
            _ = tick_interval.tick() => {
                app.tick();
            }
        }
    }

    event_handle.abort();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn handle_event(app: &mut App, event: AppEvent) {
    if let Some(detail) = &app.detail {
        handle_detail_event(app, event, detail);
        return;
    }

    match event {
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            ..
        })
        | AppEvent::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }) => {
            app.set_status(String::from("Quitting..."));
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char('/'),
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.set_status(String::from("Focus: search input — type to search, Esc to clear"));
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.input.push(c);
            let query = app.input.clone();
            perform_search(app, &query);
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Backspace,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.input.pop();
            let query = app.input.clone();
            perform_search(app, &query);
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.input.clear();
            app.set_results(Vec::new());
            app.set_status(String::from("Search cleared"));
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.select_prev();
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.select_next();
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::PageUp,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.page_up();
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::PageDown,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.page_down();
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Home,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.selected = 0;
            app.scroll = 0;
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::End,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.selected = app.result_count().saturating_sub(1);
            app.ensure_visible();
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            if let Some(result) = app.selected_result() {
                let detail = DetailView {
                    attr: result.attr.clone(),
                    name: result.name.clone(),
                    description: result.description.clone(),
                    path: result.path.clone(),
                    size: result.size,
                    license: None,
                    homepage: None,
                    maintainers: Vec::new(),
                    main_program: None,
                };
                app.set_detail(detail);
            }
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Tab,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            let next_mode = match app.mode {
                SearchMode::Search => SearchMode::Locate,
                SearchMode::Locate => SearchMode::Which,
                SearchMode::Which => SearchMode::Search,
            };
            app.set_mode(next_mode);
            app.set_status(format!("Switched to {} mode", match next_mode {
                SearchMode::Search => "search",
                SearchMode::Locate => "locate",
                SearchMode::Which => "which",
            }));
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char('r'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }) => {
            app.set_status(String::from("Refreshing..."));
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char('n'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }) => {
            app.search_quiet = !app.search_quiet;
            app.set_status(format!(
                "Quiet mode {}",
                if app.search_quiet { "on" } else { "off" }
            ));
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::CONTROL,
            ..
        }) => {
            app.search_json = !app.search_json;
            app.set_status(format!(
                "JSON output {}",
                if app.search_json { "on" } else { "off" }
            ));
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char(':'),
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.set_status(String::from("Command palette — not yet implemented"));
        }
        _ => {}
    }
}

fn handle_detail_event(app: &mut App, event: AppEvent, _detail: &DetailView) {
    match event {
        AppEvent::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            ..
        })
        | AppEvent::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.close_detail();
        }
        AppEvent::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            ..
        }) => {
            app.close_detail();
        }
        _ => {}
    }
}

fn perform_search(app: &mut App, query: &str) {
    if query.is_empty() {
        app.set_results(Vec::new());
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
        app.set_status(String::from("No package metadata sidecar found. Run nix-index first."));
        return;
    }

    let db = match SearchDb::open(&sidecar) {
        Ok(db) => db,
        Err(err) => {
            app.set_status(format!("Failed to open package database: {}", err));
            return;
        }
    };

    let sort = if app.search_sort == SearchSort::Reverse {
        PkgSearchSort::Reverse
    } else {
        match app.search_sort {
            SearchSort::None => PkgSearchSort::None,
            SearchSort::AttrAsc => PkgSearchSort::Attr,
            SearchSort::SizeAsc => PkgSearchSort::Name,
            SearchSort::SizeDesc => PkgSearchSort::NameDesc,
            SearchSort::Reverse => PkgSearchSort::Reverse,
        }
    };

    let matches = if app.search_fuzzy {
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
                    attr: r.attr,
                    name: r.name,
                    description: r.description.unwrap_or_default(),
                    path: None,
                    size: None,
                })
                .collect();
            app.set_results(results);
            app.set_status(format!("Found {} result(s)", app.result_count()));
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
                    }
                })
                .collect();
            app.set_results(search_results);
            app.set_status(format!("Found {} result(s)", app.result_count()));
        }
        Err(err) => {
            app.set_status(format!("Locate error: {}", err));
        }
    }
}

fn perform_which_search(app: &mut App, query: &str) {
    let command = std::path::Path::new(query)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(query);

    match nixdex_core::command_index::CommandIndex::open(&app.database) {
        Ok(index) => {
            let providers = index
                .lookup_command(command.as_bytes())
                .unwrap_or_default();
            let results: Vec<crate::app::SearchResult> = providers
                .into_iter()
                .map(|p| crate::app::SearchResult {
                    attr: p.attr,
                    name: p.output,
                    description: String::new(),
                    path: None,
                    size: None,
                })
                .collect();
            app.set_results(results);
            app.set_status(format!("Found {} provider(s)", app.result_count()));
        }
        Err(_) => {
            app.set_status(String::from("Command index not available. Run nix-index first."));
        }
    }
}