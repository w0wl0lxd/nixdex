//! nixdex-core — library for building and searching a Nix package file index.

pub mod daemon;
pub mod database;
pub mod errors;
pub mod files;
pub mod frcode;
pub mod hydra;
pub mod index;
pub mod nixpkgs;
pub mod store_path;

pub use errors::{Error, Result};
pub use files::{FileEntries, FileNode, FileTree, FileTreeEntry, FileType, ALL_FILE_TYPES};
pub use hydra::Fetcher;
pub use nixpkgs::{EvalJob, EvalJobLine, EvalJobsOptions, PackageList};
pub use store_path::{Origin, StorePath};

/// Default binary-cache URL used when fetching file listings.
pub const CACHE_URL: &str = "https://cache.nixos.org";

/// Build or update the nixdex index.
///
/// # Errors
///
/// Returns an error if the index build fails or is not yet implemented.
pub async fn update_index(options: &index::UpdateOptions) -> Result<()> {
    index::update(options).await
}

/// Search the nixdex database.
///
/// # Errors
///
/// Returns an error if the database cannot be read or the query is unsupported.
pub fn search_database(options: &database::SearchOptions<'_>) -> Result<()> {
    database::search(options)
}
