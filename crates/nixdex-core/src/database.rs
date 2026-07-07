//! Creating and searching nixdex databases backed by redb + fst.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::files::{FileTree, FileType};
use crate::store_path::StorePath;

/// Errors that can occur when reading or writing a database.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Encountered an unsupported on-disk file type marker.
    #[error("unsupported file type: {found:?}")]
    UnsupportedFileType {
        /// Raw type marker found in the database.
        found: Vec<u8>,
    },

    /// Database format version is newer (or older) than supported.
    #[error("unsupported version: {found}")]
    UnsupportedVersion {
        /// Version number found in the header.
        found: u64,
    },

    /// Package entry required by a file listing was missing.
    #[error("database corrupt: missing package entry")]
    MissingPackageEntry,

    /// A file entry could not be parsed.
    #[error("database corrupt: could not parse entry: {entry:?}")]
    EntryParse {
        /// Raw entry bytes.
        entry: Vec<u8>,
    },

    /// A store-path JSON blob could not be parsed.
    #[error("database corrupt: could not parse store path: {path:?}")]
    StorePathParse {
        /// Raw store-path blob.
        path: Vec<u8>,
    },

    /// redb reported a storage failure.
    #[error("redb error: {0}")]
    Redb(String),

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Requested functionality is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Convenience alias for this module.
pub type Result<T> = std::result::Result<T, Error>;

/// Writer that creates a new file database (stub).
#[derive(Debug)]
pub struct Writer {
    path: PathBuf,
    level: i32,
}

impl Writer {
    /// Create a new database writer at `path` with compression `level`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until the redb schema is defined.
    pub fn create<P: AsRef<Path>>(path: P, level: i32) -> Result<Self> {
        let _writer = Self {
            path: path.as_ref().to_path_buf(),
            level,
        };
        Err(Error::NotImplemented(
            "database::Writer::create is not implemented yet",
        ))
    }

    /// Add a package and its file tree to the database.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until writing is implemented.
    pub fn add(
        &mut self,
        _path: &StorePath,
        _files: &FileTree,
        _filter_prefix: &[u8],
    ) -> Result<()> {
        let _ = (&self.path, self.level);
        Err(Error::NotImplemented(
            "database::Writer::add is not implemented yet",
        ))
    }

    /// Finish writing and return the compressed size in bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until writing is implemented.
    pub fn finish(self) -> Result<u64> {
        let _ = (self.path, self.level);
        Err(Error::NotImplemented(
            "database::Writer::finish is not implemented yet",
        ))
    }
}

/// Reader that opens an existing file database (stub).
#[derive(Debug)]
pub struct Reader {
    path: PathBuf,
}

impl Reader {
    /// Open a nixdex database at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until redb open/query is implemented.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let _reader = Self {
            path: path.as_ref().to_path_buf(),
        };
        Err(Error::NotImplemented(
            "database::Reader::open is not implemented yet",
        ))
    }

    /// Query the FST index for `pattern` (scaffold).
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until FST queries land.
    pub fn query_fst(&self, _pattern: &str) -> Result<Vec<String>> {
        let _ = &self.path;
        Err(Error::NotImplemented(
            "database::Reader::query_fst is not implemented yet",
        ))
    }

    /// Return the path this reader was opened against.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Output mode for a search request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Full output with package/path details.
    Full {
        /// Emit ANSI colors in output.
        color: bool,
        /// Group matches that share the same matching path component.
        group: bool,
        /// Only print matches from top-level packages.
        only_toplevel: bool,
    },
    /// Print only attribute names.
    Minimal,
}

/// Options for a database search.
#[derive(Debug, Clone)]
pub struct SearchOptions<'a> {
    /// Directory that holds the index database.
    pub database: PathBuf,
    /// Pattern to search for (regex-ready string from the CLI).
    pub pattern: String,
    /// Restrict results to a store-path hash.
    pub hash: Option<String>,
    /// Restrict results to package names matching this pattern.
    pub package_pattern: Option<String>,
    /// File-type filter (empty means "all types").
    pub file_type: &'a [FileType],
    /// Output formatting mode.
    pub mode: SearchMode,
}

/// Search the database for entries matching the supplied options.
///
/// # Errors
///
/// Returns an error if the database cannot be read or the query is unsupported.
pub fn search(options: &SearchOptions<'_>) -> crate::Result<()> {
    let _ = (
        &options.database,
        &options.pattern,
        &options.hash,
        &options.package_pattern,
        options.file_type,
        options.mode,
    );
    Err(crate::Error::NotImplemented(
        "database::search is not implemented yet",
    ))
}
