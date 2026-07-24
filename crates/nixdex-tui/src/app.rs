use std::path::PathBuf;

use nixdex_core::database::SearchSort;
use nixdex_core::package_search::SearchField;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Search,
    Locate,
    Which,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub attr: String,
    pub name: String,
    pub description: String,
    pub path: Option<String>,
    pub size: Option<u64>,
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
    pub search_limit: Option<usize>,
    pub search_count: bool,
    pub search_json: bool,
    pub search_name_only: bool,
    pub search_color: bool,
    pub search_quiet: bool,
    pub search_details: bool,
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
            search_limit: Some(50),
            search_count: false,
            search_json: false,
            search_name_only: false,
            search_color: false,
            search_quiet: false,
            search_details: false,
        }
    }

    pub fn set_mode(&mut self, mode: SearchMode) {
        self.mode = mode;
        self.input.clear();
        self.results.clear();
        self.selected = 0;
        self.scroll = 0;
        self.detail = None;
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
    }

    pub fn set_results(&mut self, results: Vec<SearchResult>) {
        self.results = results;
        self.selected = 0;
        self.scroll = 0;
        self.detail = None;
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
        if (self.selected as u16) >= self.scroll + screen_height - 3 {
            self.scroll = self.selected as u16 - screen_height + 4;
        }
        if (self.selected as u16) < self.scroll {
            self.scroll = self.selected as u16;
        }
    }

    pub fn set_detail(&mut self, detail: DetailView) {
        self.detail = Some(detail);
    }

    pub fn close_detail(&mut self) {
        self.detail = None;
    }

    pub fn set_status(&mut self, message: String) {
        self.status_message = message;
    }

    pub fn tick(&mut self) {
        self.status_tick += 1;
    }

    pub fn selected_result(&self) -> Option<&SearchResult> {
        self.results.get(self.selected)
    }

    pub fn result_count(&self) -> usize {
        self.results.len()
    }
}