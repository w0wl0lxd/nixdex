//! Search package metadata (attr/description) sidecar built by `nix-index`.

const MAX_PATTERN_BYTES: usize = 1024;
const REGEX_SIZE_LIMIT: usize = 1_000_000;

use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use regex::RegexBuilder;

use crate::errors::{Error, Result};
use crate::nixpkgs::PackageMeta;

/// How to order package search results.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum SearchSort {
    /// Preserve the natural order returned by the search strategy.
    #[default]
    None,
    /// Sort by attribute path ascending.
    Attr,
    /// Sort by attribute path descending.
    AttrDesc,
    /// Sort by package name ascending.
    Name,
    /// Sort by package name descending.
    NameDesc,
    /// Sort by `meta.mainProgram` ascending.
    MainProgram,
    /// Sort by `meta.mainProgram` descending.
    MainProgramDesc,
}

impl fmt::Display for SearchSort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Attr => write!(f, "attr"),
            Self::AttrDesc => write!(f, "attr-desc"),
            Self::Name => write!(f, "name"),
            Self::NameDesc => write!(f, "name-desc"),
            Self::MainProgram => write!(f, "main-program"),
            Self::MainProgramDesc => write!(f, "main-program-desc"),
        }
    }
}

impl FromStr for SearchSort {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "" | "none" | "relevance" => Ok(Self::None),
            "attr" => Ok(Self::Attr),
            "attr-desc" | "attr:desc" | "attr-descending" => Ok(Self::AttrDesc),
            "name" => Ok(Self::Name),
            "name-desc" | "name:desc" | "name-descending" => Ok(Self::NameDesc),
            "main-program" | "mainprogram" | "main_program" => Ok(Self::MainProgram),
            "main-program-desc" | "main-program:desc" | "mainprogram-desc" => {
                Ok(Self::MainProgramDesc)
            }
            _ => Err(Error::Parse(format!("unknown search sort order: {s}"))),
        }
    }
}

#[cfg(feature = "cli")]
impl clap::ValueEnum for SearchSort {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self::None,
            Self::Attr,
            Self::AttrDesc,
            Self::Name,
            Self::NameDesc,
            Self::MainProgram,
            Self::MainProgramDesc,
        ]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            Self::None => clap::builder::PossibleValue::new("none"),
            Self::Attr => clap::builder::PossibleValue::new("attr"),
            Self::AttrDesc => clap::builder::PossibleValue::new("attr-desc"),
            Self::Name => clap::builder::PossibleValue::new("name"),
            Self::NameDesc => clap::builder::PossibleValue::new("name-desc"),
            Self::MainProgram => clap::builder::PossibleValue::new("main-program"),
            Self::MainProgramDesc => clap::builder::PossibleValue::new("main-program-desc"),
        })
    }
}

/// Which fields of a package record to match against.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum SearchField {
    /// Match only the attribute path.
    Attr,
    /// Match only the description.
    Description,
    /// Match only `meta.mainProgram`.
    MainProgram,
    /// Match any human-readable field (attr, description, main program).
    #[default]
    Both,
}

impl fmt::Display for SearchField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attr => write!(f, "attr"),
            Self::Description => write!(f, "description"),
            Self::MainProgram => write!(f, "main-program"),
            Self::Both => write!(f, "both"),
        }
    }
}

impl FromStr for SearchField {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "" | "both" => Ok(Self::Both),
            "attr" => Ok(Self::Attr),
            "description" | "desc" => Ok(Self::Description),
            "main-program" | "mainprogram" | "main_program" => Ok(Self::MainProgram),
            _ => Err(Error::Parse(format!("unknown search field: {s}"))),
        }
    }
}

#[cfg(feature = "cli")]
impl clap::ValueEnum for SearchField {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Attr, Self::Description, Self::MainProgram, Self::Both]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            Self::Attr => clap::builder::PossibleValue::new("attr"),
            Self::Description => clap::builder::PossibleValue::new("description"),
            Self::MainProgram => clap::builder::PossibleValue::new("main-program"),
            Self::Both => clap::builder::PossibleValue::new("both"),
        })
    }
}

/// In-memory package metadata search index.
///
/// Backed by the `packages.json` NDJSON sidecar produced during `nix-index`.
pub struct SearchDb {
    records: Vec<PackageMeta>,
}

impl SearchDb {
    /// Load package metadata from an NDJSON `packages.json` file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or any line is not valid JSON.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(Error::Io)?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();

        for line in reader.lines() {
            let line = line.map_err(Error::Io)?;
            if line.trim().is_empty() {
                continue;
            }
            let record: PackageMeta = sonic_rs::from_str(&line)
                .map_err(|source| Error::Parse(format!("invalid packages.json line: {source}")))?;
            records.push(record);
        }

        Ok(Self { records })
    }

    /// Search loaded records by attribute and/or description.
    ///
    /// - `regex`: treat `pattern` as a regex instead of a literal substring.
    /// - `case_sensitive`: disable case folding for literal searches and regex
    ///   `i` flag.
    /// - `exact`: require the pattern to match the whole field (literal equality
    ///   or regex anchored with `^(?:...)$`).
    pub fn search(
        &self,
        pattern: &str,
        regex: bool,
        field: SearchField,
        case_sensitive: bool,
        exact: bool,
        sort: SearchSort,
        limit: Option<usize>,
    ) -> Result<Vec<&PackageMeta>> {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(Error::Parse(format!(
                "pattern exceeds maximum length of {MAX_PATTERN_BYTES} bytes"
            )));
        }
        let mut matches: Vec<&PackageMeta> = if regex {
            let anchored = if exact {
                format!("^(?:{pattern})$")
            } else {
                pattern.to_string()
            };
            let re = RegexBuilder::new(&anchored)
                .case_insensitive(!case_sensitive)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map_err(|source| Error::Parse(format!("invalid regex: {source}")))?;
            self.records
                .iter()
                .filter(|record| record_matches_regex(record, &re, field))
                .collect()
        } else if exact {
            self.records
                .iter()
                .filter(|record| record_matches_exact(record, pattern, field, case_sensitive))
                .collect()
        } else {
            let needle = if case_sensitive {
                pattern.to_string()
            } else {
                pattern.to_lowercase()
            };
            self.records
                .iter()
                .filter(|record| record_matches_literal(record, &needle, field, case_sensitive))
                .collect()
        };

        Self::sort_records(&mut matches, sort);

        if let Some(limit) = limit {
            matches.truncate(limit);
        }

        Ok(matches)
    }

    /// Fuzzy-search package records using the skim v2 scoring algorithm.
    ///
    /// Records are ranked by the highest fuzzy match score across the selected
    /// field(s). Results are returned in descending score order, optionally
    /// truncated to `limit`.
    pub fn search_fuzzy(
        &self,
        pattern: &str,
        field: SearchField,
        case_sensitive: bool,
        sort: SearchSort,
        limit: Option<usize>,
    ) -> Result<Vec<&PackageMeta>> {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(Error::Parse(format!(
                "pattern exceeds maximum length of {MAX_PATTERN_BYTES} bytes"
            )));
        }
        let matcher = if case_sensitive {
            SkimMatcherV2::default().respect_case()
        } else {
            SkimMatcherV2::default().smart_case()
        };

        let mut scored: Vec<(i64, &PackageMeta)> = self
            .records
            .iter()
            .filter_map(|record| {
                fuzzy_score(record, pattern, field, &matcher).map(|score| (score, record))
            })
            .collect();

        if sort == SearchSort::None {
            scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
        } else {
            scored.sort_by(|(score_a, a), (score_b, b)| {
                let ord = Self::compare_records(a, b, sort);
                if ord == std::cmp::Ordering::Equal {
                    score_b.cmp(score_a)
                } else {
                    ord
                }
            });
        }

        if let Some(limit) = limit {
            scored.truncate(limit);
        }

        Ok(scored.into_iter().map(|(_, record)| record).collect())
    }

    /// Sort a slice of package records in-place according to `sort`.
    fn sort_records(matches: &mut Vec<&PackageMeta>, sort: SearchSort) {
        if sort == SearchSort::None {
            return;
        }
        matches.sort_by(|a, b| Self::compare_records(a, b, sort));
    }

    /// Compare two records according to `sort`.
    fn compare_records(a: &PackageMeta, b: &PackageMeta, sort: SearchSort) -> std::cmp::Ordering {
        let ord = match sort {
            SearchSort::None => std::cmp::Ordering::Equal,
            SearchSort::Attr | SearchSort::AttrDesc => a.attr.cmp(&b.attr),
            SearchSort::Name | SearchSort::NameDesc => a.name.cmp(&b.name),
            SearchSort::MainProgram | SearchSort::MainProgramDesc => {
                let a_main = match a.main_program.as_deref() {
                    Some(v) => v,
                    None => "",
                };
                let b_main = match b.main_program.as_deref() {
                    Some(v) => v,
                    None => "",
                };
                a_main.cmp(b_main)
            }
        };
        match sort {
            SearchSort::AttrDesc | SearchSort::NameDesc | SearchSort::MainProgramDesc => {
                ord.reverse()
            }
            _ => ord,
        }
    }
}

fn value_contains(value: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        value.contains(needle)
    } else {
        value.to_lowercase().contains(needle)
    }
}

fn value_equals(value: &str, pattern: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        value == pattern
    } else {
        value.to_lowercase() == pattern.to_lowercase()
    }
}

fn record_matches_regex(record: &PackageMeta, re: &regex::Regex, field: SearchField) -> bool {
    match field {
        SearchField::Attr => re.is_match(&record.attr),
        SearchField::Description => record
            .description
            .as_ref()
            .is_some_and(|desc| re.is_match(desc)),
        SearchField::MainProgram => record
            .main_program
            .as_ref()
            .is_some_and(|main| re.is_match(main)),
        SearchField::Both => {
            re.is_match(&record.attr)
                || record
                    .description
                    .as_ref()
                    .is_some_and(|desc| re.is_match(desc))
                || record
                    .main_program
                    .as_ref()
                    .is_some_and(|main| re.is_match(main))
        }
    }
}

fn record_matches_literal(
    record: &PackageMeta,
    needle: &str,
    field: SearchField,
    case_sensitive: bool,
) -> bool {
    match field {
        SearchField::Attr => value_contains(&record.attr, needle, case_sensitive),
        SearchField::Description => record
            .description
            .as_ref()
            .is_some_and(|desc| value_contains(desc, needle, case_sensitive)),
        SearchField::MainProgram => record
            .main_program
            .as_ref()
            .is_some_and(|main| value_contains(main, needle, case_sensitive)),
        SearchField::Both => {
            value_contains(&record.attr, needle, case_sensitive)
                || record
                    .description
                    .as_ref()
                    .is_some_and(|desc| value_contains(desc, needle, case_sensitive))
                || record
                    .main_program
                    .as_ref()
                    .is_some_and(|main| value_contains(main, needle, case_sensitive))
        }
    }
}

fn record_matches_exact(
    record: &PackageMeta,
    pattern: &str,
    field: SearchField,
    case_sensitive: bool,
) -> bool {
    match field {
        SearchField::Attr => value_equals(&record.attr, pattern, case_sensitive),
        SearchField::Description => record
            .description
            .as_ref()
            .is_some_and(|desc| value_equals(desc, pattern, case_sensitive)),
        SearchField::MainProgram => record
            .main_program
            .as_ref()
            .is_some_and(|main| value_equals(main, pattern, case_sensitive)),
        SearchField::Both => {
            value_equals(&record.attr, pattern, case_sensitive)
                || record
                    .description
                    .as_ref()
                    .is_some_and(|desc| value_equals(desc, pattern, case_sensitive))
                || record
                    .main_program
                    .as_ref()
                    .is_some_and(|main| value_equals(main, pattern, case_sensitive))
        }
    }
}

fn fuzzy_score(
    record: &PackageMeta,
    pattern: &str,
    field: SearchField,
    matcher: &SkimMatcherV2,
) -> Option<i64> {
    match field {
        SearchField::Attr => matcher.fuzzy_match(&record.attr, pattern),
        SearchField::Description => record
            .description
            .as_ref()
            .and_then(|desc| matcher.fuzzy_match(desc, pattern)),
        SearchField::MainProgram => record
            .main_program
            .as_ref()
            .and_then(|main| matcher.fuzzy_match(main, pattern)),
        SearchField::Both => {
            let mut best: Option<i64> = None;
            for value in [
                Some(record.attr.as_str()),
                record.description.as_deref(),
                record.main_program.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                if let Some(score) = matcher.fuzzy_match(value, pattern) {
                    best = Some(best.map_or(score, |current| current.max(score)));
                }
            }
            best
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn test_record(attr: &str, description: &str) -> PackageMeta {
        PackageMeta {
            attr: attr.to_string(),
            name: attr.to_string(),
            description: Some(description.to_string()),
            main_program: None,
        }
    }

    fn write_fixture(path: &Path, records: &[PackageMeta]) {
        let mut file = File::create(path).expect("tempfile");
        for record in records {
            let line = sonic_rs::to_string(record).expect("serialize");
            writeln!(file, "{line}").expect("write");
        }
    }

    #[test]
    fn literal_attr_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        write_fixture(
            &path,
            &[
                test_record("hello", "A friendly greeting"),
                test_record("nix", "The Nix package manager"),
            ],
        );

        let db = SearchDb::open(&path).expect("open");
        let hits = db
            .search(
                "nix",
                false,
                SearchField::Attr,
                false,
                false,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].attr, "nix");
    }

    #[test]
    fn literal_description_match_is_case_insensitive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        write_fixture(&path, &[test_record("hello", "A friendly greeting")]);

        let db = SearchDb::open(&path).expect("open");
        let hits = db
            .search(
                "GREETING",
                false,
                SearchField::Description,
                false,
                false,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].attr, "hello");
    }

    #[test]
    fn regex_both_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        write_fixture(
            &path,
            &[
                test_record("git", "Distributed version control"),
                test_record("hello", "A friendly greeting"),
            ],
        );

        let db = SearchDb::open(&path).expect("open");
        let hits = db
            .search(
                "^g",
                true,
                SearchField::Both,
                false,
                false,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].attr, "git");
    }

    #[test]
    fn limit_truncates_results() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        write_fixture(
            &path,
            &[
                test_record("a", "first"),
                test_record("b", "second"),
                test_record("c", "third"),
            ],
        );

        let db = SearchDb::open(&path).expect("open");
        let hits = db
            .search(
                "a|b|c",
                true,
                SearchField::Attr,
                false,
                false,
                SearchSort::None,
                Some(2),
            )
            .expect("search");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn main_program_field_is_searchable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        let mut git = test_record("git", "Distributed version control");
        git.main_program = Some("git".to_string());
        let mut lazygit = test_record("lazygit", "A simple terminal UI for git");
        lazygit.main_program = Some("lazygit".to_string());
        write_fixture(&path, &[git, lazygit]);

        let db = SearchDb::open(&path).expect("open");
        let hits = db
            .search(
                "git",
                false,
                SearchField::MainProgram,
                false,
                false,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(hits.len(), 2);

        let exact = db
            .search(
                "git",
                false,
                SearchField::MainProgram,
                false,
                true,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].attr, "git");
    }

    #[test]
    fn case_sensitive_literal_skips_wrong_case() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        write_fixture(&path, &[test_record("Hello", "A friendly greeting")]);

        let db = SearchDb::open(&path).expect("open");
        let hits = db
            .search(
                "hello",
                false,
                SearchField::Attr,
                false,
                false,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(hits.len(), 1);

        let case_sensitive = db
            .search(
                "hello",
                false,
                SearchField::Attr,
                true,
                false,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(case_sensitive.len(), 0);
    }

    #[test]
    fn search_rejects_oversized_pattern() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        write_fixture(&path, &[test_record("hello", "A greeting")]);

        let db = SearchDb::open(&path).expect("open");
        let long = "a".repeat(MAX_PATTERN_BYTES + 1);
        let result = db.search(
            &long,
            false,
            SearchField::Attr,
            false,
            false,
            SearchSort::None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn exact_literal_requires_full_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        write_fixture(
            &path,
            &[test_record("hello", "A friendly greeting program")],
        );

        let db = SearchDb::open(&path).expect("open");
        let hits = db
            .search(
                "greet",
                false,
                SearchField::Description,
                false,
                false,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(hits.len(), 1);

        let exact = db
            .search(
                "greet",
                false,
                SearchField::Description,
                false,
                true,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(exact.len(), 0);

        let full = db
            .search(
                "A friendly greeting program",
                false,
                SearchField::Description,
                false,
                true,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(full.len(), 1);
    }

    #[test]
    fn fuzzy_search_ranks_by_relevance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        write_fixture(
            &path,
            &[
                test_record("neovim-unwrapped", "Vim-fork focused on extensibility"),
                test_record("neovim-remote", "Remote control for NeoVim"),
                test_record("vim", "The ubiquitous text editor"),
            ],
        );

        let db = SearchDb::open(&path).expect("open");
        let hits = db
            .search_fuzzy("nvim", SearchField::Both, false, SearchSort::None, None)
            .expect("search");

        assert!(hits.len() >= 2, "expected at least neovim matches");
        let attrs: Vec<_> = hits.iter().map(|r| r.attr.as_str()).collect();
        assert!(
            attrs
                .iter()
                .any(|&a| a == "neovim-unwrapped" || a == "neovim-remote"),
            "expected neovim results, got {attrs:?}"
        );

        let limited = db
            .search_fuzzy("nvim", SearchField::Both, false, SearchSort::None, Some(2))
            .expect("search");
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn sort_orders_results() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("packages.json");
        let mut alpha = test_record("alpha", "First");
        alpha.name = "z-name".into();
        alpha.main_program = Some("alpha".into());
        let mut beta = test_record("beta", "Second");
        beta.name = "a-name".into();
        beta.main_program = Some("beta".into());
        let mut gamma = test_record("gamma", "Third");
        gamma.main_program = None;
        write_fixture(&path, &[alpha, beta, gamma]);

        let db = SearchDb::open(&path).expect("open");
        let all = db
            .search(
                ".*",
                true,
                SearchField::Attr,
                false,
                false,
                SearchSort::None,
                None,
            )
            .expect("search");
        assert_eq!(all.len(), 3);

        let by_attr = db
            .search(
                ".*",
                true,
                SearchField::Attr,
                false,
                false,
                SearchSort::Attr,
                None,
            )
            .expect("search");
        assert_eq!(
            by_attr.iter().map(|r| r.attr.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"]
        );

        let by_name = db
            .search(
                ".*",
                true,
                SearchField::Attr,
                false,
                false,
                SearchSort::Name,
                None,
            )
            .expect("search");
        assert_eq!(
            by_name.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["a-name", "gamma", "z-name"]
        );

        let by_main = db
            .search(
                ".*",
                true,
                SearchField::Attr,
                false,
                false,
                SearchSort::MainProgram,
                None,
            )
            .expect("search");
        let mains: Vec<_> = by_main
            .iter()
            .map(|r| r.main_program.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(mains, vec!["", "alpha", "beta"]);
    }
}
