//! Package version history sidecar for nixdex.
//!
//! The sidecar stores a mapping from attribute paths to lists of
//! `(version, commit, date)` entries, enabling `nixdex history <attr>`
//! and `nixdex search --history` to show when package versions existed.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::prelude::*;
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, WriteBytesExt};
use mmap_guard;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum total size of the history sidecar (defensive cap).
const MAX_HISTORY_BYTES: usize = 512 * 1024 * 1024;

/// Maximum number of version entries per attribute.
const MAX_VERSIONS_PER_ATTR: usize = 1_000;

/// Maximum length of a version string.
const MAX_VERSION_BYTES: usize = 128;

/// Maximum length of a commit hash.
const MAX_COMMIT_BYTES: usize = 64;

/// Magic for the history sidecar.
const HISTORY_MAGIC: &[u8] = b"NXHS";
/// Sidecar format version.
const HISTORY_VERSION: u32 = 1;

/// Sidecar filename relative to the database directory.
pub const HISTORY_FILE: &str = "files.history";

/// Errors while building or querying the version history sidecar.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Sidecar files are missing from the database directory.
    #[error("history sidecar missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("history sidecar corrupt: {0}")]
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

/// A single version record for a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    /// Version string (e.g. "2.41.0").
    pub version: String,
    /// Nixpkgs commit hash.
    pub commit: String,
    /// Date of the nixpkgs commit (ISO 8601).
    pub date: String,
}

/// Version history for a single package attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionHistory {
    /// Attribute path (e.g. "hello").
    pub attr: String,
    /// Version entries, newest first.
    pub versions: Vec<VersionEntry>,
}

/// Accumulates version history entries while a NIXI database is written.
#[derive(Debug, Default)]
pub struct HistoryBuilder {
    /// attr → version entries.
    entries: BTreeMap<String, Vec<VersionEntry>>,
}

impl HistoryBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a version for a package attribute.
    ///
    /// Versions are stored newest-first; duplicate versions for the same
    /// attr are deduplicated.
    pub fn record_version(
        &mut self,
        attr: String,
        version: String,
        commit: String,
        date: String,
    ) -> Result<()> {
        if version.len() > MAX_VERSION_BYTES {
            return Err(Error::Corrupt(format!(
                "version string too long: {} (max {MAX_VERSION_BYTES})",
                version.len()
            )));
        }
        if commit.len() > MAX_COMMIT_BYTES {
            return Err(Error::Corrupt(format!(
                "commit hash too long: {} (max {MAX_COMMIT_BYTES})",
                commit.len()
            )));
        }
        let entry = VersionEntry {
            version,
            commit,
            date,
        };
        let versions = self.entries.entry(attr).or_default();
        if versions.len() >= MAX_VERSIONS_PER_ATTR {
            return Err(Error::Corrupt(format!(
                "too many versions for attr (max {MAX_VERSIONS_PER_ATTR})"
            )));
        }
        // Deduplicate by version string.
        if !versions.iter().any(|v| v.version == entry.version) {
            versions.push(entry);
        }
        Ok(())
    }

    /// Number of attributes tracked.
    #[must_use]
    pub fn attr_count(&self) -> usize {
        self.entries.len()
    }

    /// Write the history sidecar into `db_dir` (the directory that holds `files`).
    ///
    /// The sidecar is written as a binary blob with magic, version, and
    /// NDJSON lines for each attribute's version history.
    ///
    /// # Errors
    ///
    /// Returns an error if the sidecar cannot be written.
    pub fn write_sidecar(&self, db_dir: &Path) -> Result<()> {
        let path = db_dir.join(HISTORY_FILE);
        let mut file = File::create(&path)?;

        // Write magic + version header.
        file.write_all(HISTORY_MAGIC)?;
        file.write_u32::<LittleEndian>(HISTORY_VERSION)?;

        // Write NDJSON lines.
        for (attr, versions) in &self.entries {
            let history = VersionHistory {
                attr: attr.clone(),
                versions: versions.clone(),
            };
            let line = sonic_rs::to_string(&history).map_err(|err| Error::Json(err.to_string()))?;
            writeln!(file, "{line}")?;
        }

        file.flush()?;

        // Validate size against defensive cap.
        let metadata = std::fs::metadata(&path)?;
        let max_bytes = usize::try_from(MAX_HISTORY_BYTES)
            .map_err(|_| Error::Corrupt("size conversion overflow".into()))?;
        let max_bytes_u64 = u64::try_from(max_bytes)
            .map_err(|_| Error::Corrupt("size conversion overflow".into()))?;
        if metadata.len() > max_bytes_u64 {
            return Err(Error::Corrupt(format!(
                "history sidecar too large: {} bytes (max {MAX_HISTORY_BYTES})",
                metadata.len()
            )));
        }

        Ok(())
    }
}

/// Opened version history sidecar for querying.
#[derive(Debug)]
pub struct HistoryDb {
    /// attr → version entries, loaded from the sidecar.
    entries: BTreeMap<String, Vec<VersionEntry>>,
}

impl HistoryDb {
    /// Open the history sidecar from a database directory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Missing`] when the sidecar is absent, or
    /// [`Error::Corrupt`] when the file cannot be parsed.
    pub fn open(db_dir: &Path) -> Result<Self> {
        let path = db_dir.join(HISTORY_FILE);
        if !path.is_file() {
            return Err(Error::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {HISTORY_FILE}"),
            });
        }

        let data = mmap_guard::map_file(&path).map_err(Error::Io)?;
        if data.len() > MAX_HISTORY_BYTES {
            return Err(Error::Corrupt("history file too large".into()));
        }

        // Validate magic and version.
        if data.len() < HISTORY_MAGIC.len() + 4 {
            return Err(Error::Corrupt("history file too short for header".into()));
        }
        let magic = data
            .get(..HISTORY_MAGIC.len())
            .ok_or_else(|| Error::Corrupt("history file too short for magic".into()))?;
        if magic != HISTORY_MAGIC {
            return Err(Error::Corrupt(format!(
                "history magic {magic:?}, expected {:?}",
                HISTORY_MAGIC
            )));
        }
        let ver_bytes = data
            .get(HISTORY_MAGIC.len()..HISTORY_MAGIC.len() + 4)
            .ok_or_else(|| Error::Corrupt("history file too short for version".into()))?;
        let ver = u32::from_le_bytes(
            ver_bytes
                .try_into()
                .map_err(|_| Error::Corrupt("history version slice too short".into()))?,
        );
        if ver != HISTORY_VERSION {
            return Err(Error::Corrupt(format!(
                "history version {ver}, expected {HISTORY_VERSION}"
            )));
        }

        // Parse NDJSON body.
        let body = data
            .get(HISTORY_MAGIC.len() + 4..)
            .ok_or_else(|| Error::Corrupt("history file too short for body".into()))?;
        let mut entries = BTreeMap::new();
        for line in body.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let history: VersionHistory =
                sonic_rs::from_slice(line).map_err(|err| Error::Json(err.to_string()))?;
            entries.insert(history.attr, history.versions);
        }

        Ok(Self { entries })
    }

    /// Look up the version history for a given attribute.
    ///
    /// Returns an empty list when the attribute is absent.
    pub fn lookup_attr(&self, attr: &str) -> Vec<VersionEntry> {
        self.entries.get(attr).cloned().unwrap_or_else(Vec::new)
    }

    /// Number of attributes in the history database.
    #[must_use]
    pub fn attr_count(&self) -> usize {
        self.entries.len()
    }
}
