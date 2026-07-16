//! Search package metadata (attr/description) sidecar built by `nix-index`.

use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;

use regex::RegexBuilder;

use crate::errors::{Error, Result};
use crate::nixpkgs::PackageMeta;

/// Which fields of a package record to match against.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum SearchField {
    /// Match only the attribute path.
    Attr,
    /// Match only the description.
    Description,
    /// Match either the attribute path or the description.
    #[default]
    Both,
}

impl fmt::Display for SearchField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attr => write!(f, "attr"),
            Self::Description => write!(f, "description"),
            Self::Both => write!(f, "both"),
        }
    }
}

impl FromStr for SearchField {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "attr" => Ok(Self::Attr),
            "description" | "desc" => Ok(Self::Description),
            "both" => Ok(Self::Both),
            _ => Err(Error::Parse(format!("unknown search field: {s}"))),
        }
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
    /// `regex` controls whether `pattern` is treated as a `regex` pattern or a
    /// case-insensitive literal substring.
    pub fn search(
        &self,
        pattern: &str,
        regex: bool,
        field: SearchField,
        limit: Option<usize>,
    ) -> Result<Vec<&PackageMeta>> {
        let mut matches: Vec<&PackageMeta> = if regex {
            let re = RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .map_err(|source| Error::Parse(format!("invalid regex: {source}")))?;
            self.records
                .iter()
                .filter(|record| record_matches_regex(record, &re, field))
                .collect()
        } else {
            let needle = pattern.to_lowercase();
            self.records
                .iter()
                .filter(|record| record_matches_literal(record, &needle, field))
                .collect()
        };

        if let Some(limit) = limit {
            matches.truncate(limit);
        }

        Ok(matches)
    }
}

fn record_matches_regex(record: &PackageMeta, re: &regex::Regex, field: SearchField) -> bool {
    match field {
        SearchField::Attr => re.is_match(&record.attr),
        SearchField::Description => record
            .description
            .as_ref()
            .is_some_and(|desc| re.is_match(desc)),
        SearchField::Both => {
            re.is_match(&record.attr)
                || record
                    .description
                    .as_ref()
                    .is_some_and(|desc| re.is_match(desc))
        }
    }
}

fn record_matches_literal(record: &PackageMeta, needle: &str, field: SearchField) -> bool {
    match field {
        SearchField::Attr => record.attr.to_lowercase().contains(needle),
        SearchField::Description => record
            .description
            .as_ref()
            .is_some_and(|desc| desc.to_lowercase().contains(needle)),
        SearchField::Both => {
            record.attr.to_lowercase().contains(needle)
                || record
                    .description
                    .as_ref()
                    .is_some_and(|desc| desc.to_lowercase().contains(needle))
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
            .search("nix", false, SearchField::Attr, None)
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
            .search("GREETING", false, SearchField::Description, None)
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
            .search("^g", true, SearchField::Both, None)
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
            .search("a|b|c", true, SearchField::Attr, Some(2))
            .expect("search");
        assert_eq!(hits.len(), 2);
    }
}
