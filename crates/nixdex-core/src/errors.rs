//! Workspace-wide error type for `nixdex-core`.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::store_path::StorePath;

/// Top-level error enum for library operations.
///
/// Large variants are boxed so the enum stays small enough for clippy's
/// `result_large_err` lint while still carrying full nested context.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Listing packages from a nixpkgs expression failed.
    #[error("querying available packages failed: {source}")]
    QueryPackages {
        /// Nested nixpkgs error.
        #[source]
        source: Box<crate::nixpkgs::Error>,
    },

    /// Fetching a store-path file listing from the binary cache failed.
    #[error("fetching the file listing for store path '{path}' failed: {source}")]
    FetchFiles {
        /// The store path that was requested.
        path: Box<StorePath>,
        /// Nested hydra/binary-cache error.
        #[source]
        source: Box<crate::hydra::Error>,
    },

    /// Fetching the references of a store path failed.
    #[error("fetching the references of store path '{path}' failed: {source}")]
    FetchReferences {
        /// The store path that was requested.
        path: Box<StorePath>,
        /// Nested hydra/binary-cache error.
        #[source]
        source: Box<crate::hydra::Error>,
    },

    /// Creating the directory that should hold the database failed.
    #[error("creating the directory for the database at '{path}' failed: {source}")]
    CreateDatabaseDir {
        /// Intended database directory.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// Creating a new database file failed.
    #[error("creating the database at '{path}' failed: {source}")]
    CreateDatabase {
        /// Intended database path.
        path: PathBuf,
        /// Nested database error.
        #[source]
        source: Box<crate::database::Error>,
    },

    /// Writing rows into an open database failed.
    #[error("writing to the database '{path}' failed: {source}")]
    WriteDatabase {
        /// Database path being written.
        path: PathBuf,
        /// Nested database error.
        #[source]
        source: Box<crate::database::Error>,
    },

    /// Reading from an existing database failed.
    #[error("reading the database '{path}' failed: {source}")]
    ReadDatabase {
        /// Database path being read.
        path: PathBuf,
        /// Nested database error.
        #[source]
        source: Box<crate::database::Error>,
    },

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Parsing a file listing or other structured input failed.
    #[error("parse error: {0}")]
    Parse(String),

    /// Requested functionality is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    /// Prebuilt index management failed.
    #[error("prebuilt index error: {0}")]
    Prebuilt(#[from] crate::prebuilt::Error),
}

/// Convenience alias used throughout the library.
pub type Result<T> = std::result::Result<T, Error>;
