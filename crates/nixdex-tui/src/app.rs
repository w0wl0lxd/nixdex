use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nixdex_core::database::SearchSort;
use nixdex_core::package_search::{SearchDb, SearchField};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Search,
    Locate,
    Which,
}

pub struct SearchDbCache {
    db: Option<SearchDb>,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for SearchDbCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchDbCache")
            .field("db", &self.db.is_some())
            .field("path", &self.path)
            .finish()
    }
}

impl Default for SearchDbCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchDbCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            db: None,
            path: None,
        }
    }

    pub fn get_or_open(&mut self, sidecar: &Path) -> Result<&SearchDb, String> {
        let needs_open = self.db.is_none() || self.path.as_deref() != Some(sidecar);

        if needs_open {
            match SearchDb::open(sidecar) {
                Ok(db) => {
                    self.db = Some(db);
                    self.path = Some(sidecar.to_path_buf());
                }
                Err(err) => return Err(err.to_string()),
            }
        }
        match &self.db {
            Some(db) => Ok(db),
            None => Err("database was not opened".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    CatppuccinMocha,
    Nord,
    Dracula,
    TokyoNight,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub attr: String,
    pub name: String,
    pub description: String,
    pub path: Option<String>,
    pub size: Option<u64>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub maintainers: Vec<String>,
    pub main_program: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DetailView {
    pub attr: String,
    pub name: String,
    pub description: String,
    pub path: Option<String>,
    pub size: Option<u64>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub maintainers: Vec<String>,
    pub main_program: Option<String>,
    pub pinned: bool,
}

#[derive(Debug)]
pub struct App {
    pub mode: SearchMode,
    pub input: String,
    pub results: Vec<SearchResult>,
    pub selected: usize,
    pub scroll: u16,
    pub detail: Option<DetailView>,
    pub status_message: String,
    pub status_tick: u64,
    pub database: PathBuf,
    pub search_sort: SearchSort,
    pub search_field: SearchField,
    pub search_case_sensitive: bool,
    pub search_exact: bool,
    pub search_regex: bool,
    pub search_fuzzy: bool,
    pub search_tiered_fuzzy: bool,
    pub search_limit: Option<usize>,
    pub search_count: bool,
    pub search_json: bool,
    pub search_name_only: bool,
    pub search_color: bool,
    pub search_quiet: bool,
    pub search_details: bool,
    pub theme: Theme,
    pub detail_pinned: bool,
    pub expand_all: bool,
    pub search_cache: BTreeMap<String, Vec<SearchResult>>,
    pub cache_timestamps: BTreeMap<String, Instant>,
    pub cache_ttl: Duration,
    pub toasts: Vec<Toast>,
    pub is_searching: bool,
    pub show_help: bool,
    /// Set by Ctrl+R. The event loop clears it and re-runs the current query.
    pub reload_requested: bool,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub timestamp: Instant,
    pub duration: Duration,
}

impl Toast {
    pub fn new(message: String, duration: Duration) -> Self {
        Self {
            message,
            timestamp: Instant::now(),
            duration,
        }
    }

    pub fn is_expired(&self) -> bool {
        self.timestamp.elapsed() >= self.duration
    }
}

impl App {
    pub fn new(database: PathBuf) -> Self {
        Self {
            mode: SearchMode::Search,
            input: String::new(),
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            detail: None,
            status_message: String::from("Press / to search, Tab to switch mode, q to quit"),
            status_tick: 0,
            database,
            search_sort: SearchSort::None,
            search_field: SearchField::Both,
            search_case_sensitive: false,
            search_exact: false,
            search_regex: false,
            search_fuzzy: false,
            search_tiered_fuzzy: false,
            search_limit: Some(50),
            search_count: false,
            search_json: false,
            search_name_only: false,
            search_color: false,
            search_quiet: false,
            search_details: false,
            theme: Theme::default(),
            detail_pinned: false,
            expand_all: false,
            search_cache: BTreeMap::new(),
            cache_timestamps: BTreeMap::new(),
            cache_ttl: Duration::from_secs(30),
            toasts: Vec::new(),
            is_searching: false,
            show_help: false,
            reload_requested: false,
        }
    }

    pub fn set_mode(&mut self, mode: SearchMode) {
        self.mode = mode;
        self.input.clear();
        self.results.clear();
        self.selected = 0;
        self.scroll = 0;
        self.detail = None;
        self.detail_pinned = false;
        self.status_message = match mode {
            SearchMode::Search => String::from("Search mode — type to search packages"),
            SearchMode::Locate => String::from("Locate mode — type to search files"),
            SearchMode::Which => String::from("Which mode — type a command to find its package"),
        };
    }

    pub fn set_input(&mut self, input: String) {
        self.input = input;
        self.selected = 0;
        self.scroll = 0;
        self.detail = None;
        self.detail_pinned = false;
    }

    pub fn set_results(&mut self, results: Vec<SearchResult>) {
        self.results = results;
        self.selected = 0;
        self.scroll = 0;
        self.detail = None;
        self.detail_pinned = false;
    }

    pub fn select_next(&mut self) {
        if self.selected < self.results.len().saturating_sub(1) {
            self.selected += 1;
            self.ensure_visible();
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.ensure_visible();
        }
    }

    pub fn page_down(&mut self) {
        let page_size = 10;
        for _ in 0..page_size {
            self.select_next();
        }
    }

    pub fn page_up(&mut self) {
        let page_size = 10;
        for _ in 0..page_size {
            self.select_prev();
        }
    }

    pub fn ensure_visible(&mut self) {
        let screen_height = 20u16;
        #[allow(clippy::unnecessary_lazy_evaluations)]
        let selected_u16 = u16::try_from(self.selected).unwrap_or_else(|_| u16::MAX);
        if selected_u16 >= self.scroll + screen_height - 3 {
            self.scroll = selected_u16.saturating_sub(screen_height - 4);
        }
        if selected_u16 < self.scroll {
            self.scroll = selected_u16;
        }
    }

    pub fn set_detail(&mut self, detail: DetailView) {
        self.detail = Some(detail);
    }

    pub fn close_detail(&mut self) {
        self.detail = None;
        self.detail_pinned = false;
    }

    pub fn toggle_detail_pin(&mut self) {
        if let Some(ref mut detail) = self.detail {
            detail.pinned = !detail.pinned;
            self.detail_pinned = detail.pinned;
        }
    }

    pub fn toggle_expand_all(&mut self) {
        self.expand_all = !self.expand_all;
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub fn cycle_theme(&mut self) {
        self.theme = match self.theme {
            Theme::CatppuccinMocha => Theme::Nord,
            Theme::Nord => Theme::Dracula,
            Theme::Dracula => Theme::TokyoNight,
            Theme::TokyoNight => Theme::CatppuccinMocha,
        };
    }

    pub fn set_status(&mut self, message: String) {
        self.status_message = message;
    }

    pub fn add_toast(&mut self, message: String) {
        self.toasts
            .push(Toast::new(message, Duration::from_secs(3)));
    }

    pub fn tick(&mut self) {
        self.status_tick += 1;
        self.toasts.retain(|t| !t.is_expired());
        // Drop cache entries that have aged out. Without this the cache only
        // ever grows: `is_cache_valid` skips an expired entry but never removes
        // it, so a long session holds every result set it ever fetched.
        self.clear_expired_cache();
    }

    pub fn selected_result(&self) -> Option<&SearchResult> {
        self.results.get(self.selected)
    }

    pub fn result_count(&self) -> usize {
        self.results.len()
    }

    /// Snapshot the settings one search needs, for the blocking worker.
    pub fn search_request(&self, query: &str) -> SearchRequest {
        SearchRequest {
            query: query.to_string(),
            mode: self.mode,
            database: self.database.clone(),
            limit: self.search_limit,
            sort: self.search_sort,
            field: self.search_field,
            case_sensitive: self.search_case_sensitive,
            tiered_fuzzy: self.search_tiered_fuzzy,
            fuzzy: self.search_fuzzy,
            regex: self.search_regex,
            exact: self.search_exact,
            quiet: self.search_quiet,
            details: self.search_details,
            reload: false,
        }
    }

    pub fn cache_key(&self, query: &str) -> String {
        format!(
            "{:?}|{}|{:?}|{}|{}|{}|{}|{}|{:?}|{}|{}",
            self.mode,
            query,
            self.search_field,
            self.search_case_sensitive,
            self.search_exact,
            self.search_regex,
            self.search_fuzzy,
            self.search_tiered_fuzzy,
            self.search_sort,
            match self.search_limit {
                Some(v) => v,
                None => 0,
            },
            self.search_name_only,
        )
    }

    pub fn get_cached_results(&self, query: &str) -> Option<&Vec<SearchResult>> {
        self.search_cache.get(&self.cache_key(query))
    }

    pub fn cache_results(&mut self, query: String, results: Vec<SearchResult>) {
        let key = self.cache_key(&query);
        self.search_cache.insert(key.clone(), results);
        self.cache_timestamps.insert(key, Instant::now());
    }

    pub fn is_cache_valid(&self, query: &str) -> bool {
        let key = self.cache_key(query);
        if let Some(ts) = self.cache_timestamps.get(&key) {
            ts.elapsed() < self.cache_ttl
        } else {
            false
        }
    }

    /// Drop every cached result, so the next search goes back to the database.
    pub fn clear_search_cache(&mut self) {
        self.search_cache.clear();
        self.cache_timestamps.clear();
    }

    pub fn clear_expired_cache(&mut self) {
        let now = Instant::now();
        self.cache_timestamps
            .retain(|_, ts| now.duration_since(*ts) < self.cache_ttl);
        self.search_cache
            .retain(|k, _| self.cache_timestamps.contains_key(k));
    }
}

pub fn fuzzy_score(query: &str, target: &str) -> u32 {
    if query.is_empty() {
        return 0;
    }

    let query_lower = query.to_lowercase();
    let target_lower = target.to_lowercase();

    if target_lower == query_lower {
        return 1000;
    }

    if target_lower.starts_with(&query_lower) {
        return 500;
    }

    let query_chars: Vec<char> = query_lower.chars().collect();
    let target_chars: Vec<char> = target_lower.chars().collect();

    let mut score = 0;
    let mut query_idx = 0;
    let mut last_match_idx = None;

    for (ti, tc) in target_chars.iter().enumerate() {
        let Some(&qc) = query_chars.get(query_idx) else {
            continue;
        };
        if *tc == qc {
            if let Some(last_idx) = last_match_idx {
                if ti == last_idx + 1 {
                    score += 50;
                } else if is_word_boundary(&target_chars, last_idx, ti) {
                    score += 100;
                } else {
                    score += 10;
                }
            } else {
                score += 30;
            }
            last_match_idx = Some(ti);
            query_idx += 1;
        }
    }

    if query_idx < query_chars.len() {
        return 0;
    }

    score
}

fn is_word_boundary(chars: &[char], from: usize, to: usize) -> bool {
    if from + 1 == to {
        return false;
    }
    if from == 0 {
        return true;
    }
    let prev = match chars.get(from) {
        Some(c) => *c,
        None => return false,
    };
    let curr = match chars.get(to) {
        Some(c) => *c,
        None => return false,
    };
    prev.is_ascii_whitespace()
        || prev == '_'
        || prev == '-'
        || prev == '.'
        || prev == '/'
        || prev == ':'
        || curr.is_ascii_uppercase() && prev.is_ascii_lowercase()
}

/// Everything one search needs, snapshotted off `App`.
///
/// The search runs on a blocking worker task so the event loop can keep
/// drawing and reading keys while a database read is in flight. The worker
/// cannot borrow `App`, so it gets this copy of the settings instead.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub mode: SearchMode,
    pub database: PathBuf,
    pub limit: Option<usize>,
    pub sort: SearchSort,
    pub field: SearchField,
    pub case_sensitive: bool,
    pub tiered_fuzzy: bool,
    pub fuzzy: bool,
    pub regex: bool,
    pub exact: bool,
    pub quiet: bool,
    pub details: bool,
    /// Drop the worker's cached database handle before searching, so a sidecar
    /// rewritten since the last query is picked up.
    pub reload: bool,
}

/// What one search produced, sent back to the event loop.
///
/// `results` is `None` when the search failed or found no usable database. The
/// loop then keeps the results it already has and shows `status` instead, so a
/// transient error does not wipe the list under the user.
#[derive(Debug)]
pub struct SearchOutcome {
    pub query: String,
    pub mode: SearchMode,
    pub results: Option<Vec<SearchResult>>,
    pub status: Option<String>,
}

#[cfg(test)]
mod cache_expiry_tests {
    use super::App;
    use std::time::Duration;

    #[test]
    fn a_tick_drops_cache_entries_that_have_aged_out() {
        let mut app = App::new(std::path::PathBuf::from("/nonexistent"));
        app.cache_ttl = Duration::ZERO;
        app.cache_results("stale".to_string(), Vec::new());
        assert_eq!(app.search_cache.len(), 1, "the entry was cached");

        app.tick();

        assert!(
            app.search_cache.is_empty(),
            "an expired entry must be evicted, not merely skipped"
        );
        assert!(app.cache_timestamps.is_empty());
    }
}
