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
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum total size of the history sidecar (defensive cap).
///
/// Public so the downloader can reject an oversized body before buffering it,
/// instead of repeating the number and drifting from the value enforced here.
pub const MAX_HISTORY_BYTES: usize = 512 * 1024 * 1024;

/// Maximum number of version entries per attribute.
const MAX_VERSIONS_PER_ATTR: usize = 1_000;

/// Maximum length of a version string.
const MAX_VERSION_BYTES: usize = 128;

/// Maximum length of a commit hash.
const MAX_COMMIT_BYTES: usize = 64;

/// Magic for the history sidecar.
pub const HISTORY_MAGIC: &[u8] = b"NXHS";
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
    /// Versions are stored newest-first and duplicate versions for the same
    /// attr are deduplicated.
    ///
    /// The order is established here rather than asked of the caller. The
    /// entry is placed by `date`, so recording in any order still yields a
    /// newest-first list. `date` must therefore be an ISO-8601 date
    /// (`YYYY-MM-DD`, git's `%cs`), which orders correctly as a string; two
    /// entries sharing a date keep the order they were recorded in.
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
        // Deduplicate by version string. Recording a version that is already
        // present is a no-op, so the cap is only checked when a new version is
        // actually pushed -- otherwise a full attr would reject its own
        // idempotent re-record.
        if versions.iter().any(|v| v.version == entry.version) {
            return Ok(());
        }
        if versions.len() >= MAX_VERSIONS_PER_ATTR {
            return Err(Error::Corrupt(format!(
                "too many versions for attr (max {MAX_VERSIONS_PER_ATTR})"
            )));
        }
        // Insert ahead of the first older entry. Appending made "newest-first"
        // an obligation on the caller that nothing checked and no caller
        // established.
        let position = match versions
            .iter()
            .position(|existing| entry.date > existing.date)
        {
            Some(index) => index,
            // Older than everything recorded so far, so it goes last.
            None => versions.len(),
        };
        versions.insert(position, entry);
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
        // Write to a temporary file and rename it into place, so a reader never
        // observes a half-written sidecar and a failed write leaves the previous
        // sidecar intact.
        let tmp_path = db_dir.join(format!("{HISTORY_FILE}.tmp"));
        let mut file = File::create(&tmp_path)?;

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
        file.sync_all()?;
        drop(file);

        // Validate size against defensive cap before the rename, so an oversized
        // sidecar never replaces a good one.
        let metadata = std::fs::metadata(&tmp_path)?;
        let max_bytes = MAX_HISTORY_BYTES;
        let max_bytes_u64 = u64::try_from(max_bytes)
            .map_err(|_| Error::Corrupt("size conversion overflow".into()))?;
        if metadata.len() > max_bytes_u64 {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(Error::Corrupt(format!(
                "history sidecar too large: {} bytes (max {MAX_HISTORY_BYTES})",
                metadata.len()
            )));
        }

        std::fs::rename(&tmp_path, &path)?;

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

#[cfg(test)]
mod tests {
    use super::{HistoryBuilder, HistoryDb, MAX_VERSIONS_PER_ATTR};

    fn fill_to_capacity(builder: &mut HistoryBuilder) {
        for i in 0..MAX_VERSIONS_PER_ATTR {
            builder
                .record_version(
                    "pkgs.hello".to_string(),
                    format!("1.{i}"),
                    "cafe".to_string(),
                    "2026-01-01".to_string(),
                )
                .expect("recording a fresh version below the cap succeeds");
        }
    }

    /// The newest-first promise must hold whatever order the caller records in.
    ///
    /// `record_version` appended, so the order was whatever the caller happened
    /// to use. Nothing checked it and no production caller established it.
    #[test]
    fn versions_come_back_newest_first_whatever_order_they_were_recorded_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = HistoryBuilder::new();

        // Deliberately out of order: oldest, newest, middle.
        for (version, date) in [
            ("1.0", "2024-01-01"),
            ("3.0", "2026-01-01"),
            ("2.0", "2025-01-01"),
        ] {
            builder
                .record_version(
                    "pkgs.hello".to_string(),
                    version.to_string(),
                    "cafe".to_string(),
                    date.to_string(),
                )
                .expect("recording a fresh version succeeds");
        }

        builder.write_sidecar(dir.path()).expect("write sidecar");
        let db = HistoryDb::open(dir.path()).expect("open sidecar");
        let versions: Vec<String> = db
            .lookup_attr("pkgs.hello")
            .into_iter()
            .map(|entry| entry.version)
            .collect();

        assert_eq!(versions, vec!["3.0", "2.0", "1.0"]);
    }

    #[test]
    fn re_recording_a_known_version_at_capacity_is_a_no_op() {
        let mut builder = HistoryBuilder::new();
        fill_to_capacity(&mut builder);

        builder
            .record_version(
                "pkgs.hello".to_string(),
                "1.0".to_string(),
                "cafe".to_string(),
                "2026-01-01".to_string(),
            )
            .expect("re-recording an existing version must not hit the cap");
    }

    #[test]
    fn a_new_version_at_capacity_is_rejected() {
        let mut builder = HistoryBuilder::new();
        fill_to_capacity(&mut builder);

        let err = builder.record_version(
            "pkgs.hello".to_string(),
            "9.9".to_string(),
            "cafe".to_string(),
            "2026-01-01".to_string(),
        );
        assert!(err.is_err(), "a genuinely new version must still be capped");
    }

    #[test]
    fn writing_the_sidecar_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut builder = HistoryBuilder::new();
        builder
            .record_version(
                "pkgs.hello".to_string(),
                "1.0".to_string(),
                "cafe".to_string(),
                "2026-01-01".to_string(),
            )
            .expect("record");
        builder.write_sidecar(dir.path()).expect("write sidecar");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");

        let db = HistoryDb::open(dir.path()).expect("the renamed sidecar is readable");
        assert_eq!(db.attr_count(), 1);
    }
}
