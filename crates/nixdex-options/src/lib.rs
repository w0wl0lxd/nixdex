//! NixOS module options sidecar for nixdex.
//!
//! The sidecar stores NixOS module option records (attribute path,
//! type, description, default value, example) enabling
//! `nixdex options <pattern>` and `nixdex search --field options`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::prelude::*;
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, WriteBytesExt};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum total size of the options sidecar (defensive cap).
///
/// Public so the downloader can reject an oversized body before buffering it,
/// instead of repeating the number and drifting from the value enforced here.
pub const MAX_OPTIONS_BYTES: usize = 1024 * 1024 * 1024;

/// Maximum number of option records.
const MAX_OPTION_COUNT: usize = 100_000;

/// Maximum length of a single option attribute path.
const MAX_OPTION_ATTR_BYTES: usize = 256;

/// Maximum length of a single option type string.
const MAX_OPTION_TYPE_BYTES: usize = 128;

/// Maximum length of a single option description.
const MAX_OPTION_DESC_BYTES: usize = 4096;

/// Magic for the options sidecar.
pub const OPTIONS_MAGIC: &[u8] = b"NXOP";
/// Sidecar format version.
const OPTIONS_VERSION: u32 = 1;

/// Sidecar filename relative to the database directory.
pub const OPTIONS_FILE: &str = "files.options";

/// Errors while building or querying the options sidecar.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Sidecar files are missing from the database directory.
    #[error("options sidecar missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("options sidecar corrupt: {0}")]
    Corrupt(String),

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failed.
    #[error("JSON error: {0}")]
    Json(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Strips the `| ` prefix that nixpkgs option descriptions carry over from
/// their Markdown table source.
///
/// Normalising here keeps every consumer consistent. The CLI used to strip the
/// prefix only when printing a human-readable record, so `--json`, the daemon's
/// `/options` response and [`OptionsDb::search`] all saw -- and matched
/// against -- the prefixed text. Both the build path and the sidecar decode
/// path go through this, because a downloaded sidecar never passes through
/// [`OptionsBuilder::record_option`].
fn normalize_description(description: String) -> String {
    match description.strip_prefix("| ") {
        Some(rest) => rest.to_string(),
        None => description,
    }
}

/// A single NixOS module option record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionRecord {
    /// Attribute path (e.g. "services.httpd.enable").
    pub attr: String,
    /// Option type (e.g. "boolean", "string", "package").
    pub r#type: String,
    /// Human-readable description.
    pub description: String,
    /// Default value (as a string representation).
    pub default: Option<String>,
    /// Example value (as a string representation).
    pub example: Option<String>,
}

/// Options database mapping attribute paths to option records.
#[derive(Debug, Default)]
pub struct OptionsDb {
    /// attr → option record.
    entries: BTreeMap<String, OptionRecord>,
}

/// Case-insensitive substring test.
///
/// `needle_lower` must already be lowercase. When both sides are ASCII the test
/// runs over the bytes in place, so a scan over the whole option database does
/// not allocate a lowercased copy of every attribute and description. Non-ASCII
/// input falls back to full Unicode lowercasing, which keeps the result
/// identical to `haystack.to_lowercase().contains(needle_lower)`.
fn contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    if !haystack.is_ascii() || !needle_lower.is_ascii() {
        return haystack.to_lowercase().contains(needle_lower);
    }
    if needle_lower.is_empty() {
        return true;
    }
    let hay = haystack.as_bytes();
    let needle = needle_lower.as_bytes();
    if needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

impl OptionsDb {
    /// Open the options sidecar from a database directory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Missing`] when the sidecar is absent, or
    /// [`Error::Corrupt`] when the file cannot be parsed.
    pub fn open(db_dir: &Path) -> Result<Self> {
        let path = db_dir.join(OPTIONS_FILE);
        if !path.is_file() {
            return Err(Error::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {OPTIONS_FILE}"),
            });
        }

        let data = mmap_guard::map_file(&path).map_err(Error::Io)?;
        if data.len() > MAX_OPTIONS_BYTES {
            return Err(Error::Corrupt("options file too large".into()));
        }

        // Validate magic and version.
        if data.len() < OPTIONS_MAGIC.len() + 4 {
            return Err(Error::Corrupt("options file too short for header".into()));
        }
        let magic = data
            .get(..OPTIONS_MAGIC.len())
            .ok_or_else(|| Error::Corrupt("options file too short for magic".into()))?;
        if magic != OPTIONS_MAGIC {
            return Err(Error::Corrupt(format!(
                "options magic {magic:?}, expected {:?}",
                OPTIONS_MAGIC
            )));
        }
        let ver_bytes = data
            .get(OPTIONS_MAGIC.len()..OPTIONS_MAGIC.len() + 4)
            .ok_or_else(|| Error::Corrupt("options file too short for version".into()))?;
        let ver = u32::from_le_bytes(
            ver_bytes
                .try_into()
                .map_err(|_| Error::Corrupt("options version slice too short".into()))?,
        );
        if ver != OPTIONS_VERSION {
            return Err(Error::Corrupt(format!(
                "options version {ver}, expected {OPTIONS_VERSION}"
            )));
        }

        // Parse NDJSON body.
        let body = data
            .get(OPTIONS_MAGIC.len() + 4..)
            .ok_or_else(|| Error::Corrupt("options file too short for body".into()))?;
        let mut entries = BTreeMap::new();
        for line in body.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let mut record: OptionRecord =
                sonic_rs::from_slice(line).map_err(|err| Error::Json(err.to_string()))?;
            record.description = normalize_description(std::mem::take(&mut record.description));
            if record.attr.len() > MAX_OPTION_ATTR_BYTES {
                return Err(Error::Corrupt(format!(
                    "option attr too long: {} (max {MAX_OPTION_ATTR_BYTES})",
                    record.attr.len()
                )));
            }
            entries.insert(record.attr.clone(), record);
        }

        if entries.len() > MAX_OPTION_COUNT {
            return Err(Error::Corrupt(format!(
                "too many options: {} (max {MAX_OPTION_COUNT})",
                entries.len()
            )));
        }

        Ok(Self { entries })
    }

    /// Look up an option record by attribute path.
    ///
    /// Returns `None` when the attribute is absent.
    pub fn lookup_attr(&self, attr: &str) -> Option<&OptionRecord> {
        self.entries.get(attr)
    }

    /// Search options by a pattern in the attribute path or description.
    ///
    /// Returns matching records sorted by attribute path.
    pub fn search(&self, pattern: &str, case_sensitive: bool) -> Vec<&OptionRecord> {
        let mut results: Vec<&OptionRecord> = if case_sensitive {
            self.entries
                .values()
                .filter(|record| {
                    record.attr.contains(pattern) || record.description.contains(pattern)
                })
                .collect()
        } else {
            let pattern_lower = pattern.to_lowercase();
            self.entries
                .values()
                .filter(|record| {
                    contains_ignore_case(&record.attr, &pattern_lower)
                        || contains_ignore_case(&record.description, &pattern_lower)
                })
                .collect()
        };
        results.sort_by(|a, b| a.attr.cmp(&b.attr));
        results
    }

    /// Number of option records in the database.
    #[must_use]
    pub fn option_count(&self) -> usize {
        self.entries.len()
    }
}

/// Accumulates option records while building the options index.
#[derive(Debug, Default)]
pub struct OptionsBuilder {
    /// attr → option record.
    entries: BTreeMap<String, OptionRecord>,
}

impl OptionsBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an option.
    ///
    /// Duplicate attrs are deduplicated (last write wins).
    pub fn record_option(
        &mut self,
        attr: String,
        r#type: String,
        description: String,
        default: Option<String>,
        example: Option<String>,
    ) -> Result<()> {
        if attr.len() > MAX_OPTION_ATTR_BYTES {
            return Err(Error::Corrupt(format!(
                "option attr too long: {} (max {MAX_OPTION_ATTR_BYTES})",
                attr.len()
            )));
        }
        if r#type.len() > MAX_OPTION_TYPE_BYTES {
            return Err(Error::Corrupt(format!(
                "option type too long: {} (max {MAX_OPTION_TYPE_BYTES})",
                r#type.len()
            )));
        }
        if description.len() > MAX_OPTION_DESC_BYTES {
            return Err(Error::Corrupt(format!(
                "option description too long: {} (max {MAX_OPTION_DESC_BYTES})",
                description.len()
            )));
        }

        let record = OptionRecord {
            attr,
            r#type,
            description: normalize_description(description),
            default,
            example,
        };
        self.entries.insert(record.attr.clone(), record);
        Ok(())
    }

    /// Number of options recorded.
    #[must_use]
    pub fn option_count(&self) -> usize {
        self.entries.len()
    }

    /// Write the options sidecar into `db_dir` (the directory that holds `files`).
    ///
    /// # Errors
    ///
    /// Returns an error if the sidecar cannot be written.
    pub fn write_sidecar(&self, db_dir: &Path) -> Result<()> {
        if self.entries.len() > MAX_OPTION_COUNT {
            return Err(Error::Corrupt(format!(
                "too many options: {} (max {MAX_OPTION_COUNT})",
                self.entries.len()
            )));
        }

        let path = db_dir.join(OPTIONS_FILE);
        let mut file = File::create(&path)?;

        // Write magic + version header.
        file.write_all(OPTIONS_MAGIC)?;
        file.write_u32::<LittleEndian>(OPTIONS_VERSION)?;

        // Write NDJSON lines.
        for record in self.entries.values() {
            let line = sonic_rs::to_string(record).map_err(|err| Error::Json(err.to_string()))?;
            writeln!(file, "{line}")?;
        }

        file.flush()?;

        // Validate size against defensive cap.
        let metadata = std::fs::metadata(&path)?;
        let max_bytes = MAX_OPTIONS_BYTES;
        let max_bytes_u64 = u64::try_from(max_bytes)
            .map_err(|_| Error::Corrupt("size conversion overflow".into()))?;
        if metadata.len() > max_bytes_u64 {
            return Err(Error::Corrupt(format!(
                "options sidecar too large: {} bytes (max {MAX_OPTIONS_BYTES})",
                metadata.len()
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_description;

    #[test]
    fn a_markdown_table_prefix_is_stripped() {
        assert_eq!(
            normalize_description("| Whether to enable the service.".to_string()),
            "Whether to enable the service."
        );
    }

    #[test]
    fn a_description_without_the_prefix_is_untouched() {
        assert_eq!(
            normalize_description("Whether to enable the service.".to_string()),
            "Whether to enable the service."
        );
    }

    #[test]
    fn a_bare_pipe_is_not_a_prefix() {
        // Only the `| ` pair is the table artefact. A pipe that starts real
        // prose, or one with no following space, must survive.
        assert_eq!(normalize_description("|pipe".to_string()), "|pipe");
    }
}

#[cfg(test)]
mod case_fold_tests {
    use super::contains_ignore_case;

    #[test]
    fn ascii_matching_ignores_case_without_allocating() {
        assert!(contains_ignore_case("services.NGINX.enable", "nginx"));
        assert!(!contains_ignore_case("services.nginx.enable", "apache"));
        assert!(contains_ignore_case("anything", ""));
        assert!(!contains_ignore_case("ab", "abc"));
    }

    #[test]
    fn non_ascii_matching_agrees_with_full_lowercasing() {
        for (haystack, needle) in [
            ("Straße Wärme", "wärme"),
            ("ÉCOLE normale", "école"),
            ("plain ascii", "ÄÖÜ"),
        ] {
            let needle_lower = needle.to_lowercase();
            assert_eq!(
                contains_ignore_case(haystack, &needle_lower),
                haystack.to_lowercase().contains(&needle_lower),
                "mismatch for {haystack:?} / {needle:?}"
            );
        }
    }
}
