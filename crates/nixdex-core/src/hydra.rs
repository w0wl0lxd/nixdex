//! Fetching file listings and references from the Nix binary cache.

use thiserror::Error;

use crate::store_path::StorePath;
use crate::CACHE_URL;

/// Errors that can occur when talking to a binary cache.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// HTTP request to the binary cache failed.
    #[error("request failed: {0}")]
    Request(String),

    /// Response body could not be parsed.
    #[error("parse failed: {0}")]
    Parse(String),

    /// Local I/O failed while handling the response.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Requested functionality is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Convenience alias for this module.
pub type Result<T> = std::result::Result<T, Error>;

/// Client for a Nix binary cache (for example `https://cache.nixos.org`).
#[derive(Debug, Clone)]
pub struct Fetcher {
    base_url: String,
}

impl Fetcher {
    /// Create a fetcher targeting `base_url`.
    ///
    /// # Errors
    ///
    /// Returns an error when `base_url` is empty.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into();
        if base_url.is_empty() {
            return Err(Error::Request("binary cache URL must not be empty".into()));
        }
        Ok(Self { base_url })
    }

    /// Create a fetcher targeting the default binary cache.
    ///
    /// # Errors
    ///
    /// Propagates construction errors from [`Self::new`].
    pub fn default_cache() -> Result<Self> {
        Self::new(CACHE_URL)
    }

    /// Return the configured base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Build the `.ls` URL for a store path hash.
    #[must_use]
    pub fn listing_url(&self, path: &StorePath) -> String {
        format!("{}/{}.ls", self.base_url, path.hash())
    }

    /// Build the `.narinfo` URL for a store path hash.
    #[must_use]
    pub fn narinfo_url(&self, path: &StorePath) -> String {
        format!("{}/{}.narinfo", self.base_url, path.hash())
    }

    /// Fetch the file listing (`.ls`) for a store path.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until the HTTP pipeline is ready.
    #[allow(clippy::unused_async)] // will await HTTP once the client lands
    pub async fn fetch_files(&self, path: &StorePath) -> Result<Vec<u8>> {
        let _url = self.listing_url(path);
        Err(Error::NotImplemented(
            "Fetcher::fetch_files is not implemented yet",
        ))
    }

    /// Fetch the narinfo (references, compression, etc.) for a store path.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until the HTTP pipeline is ready.
    #[allow(clippy::unused_async)] // will await HTTP once the client lands
    pub async fn fetch_narinfo(&self, path: &StorePath) -> Result<String> {
        let _url = self.narinfo_url(path);
        Err(Error::NotImplemented(
            "Fetcher::fetch_narinfo is not implemented yet",
        ))
    }

    /// Fetch the references of a store path from its narinfo.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotImplemented`] until the HTTP pipeline is ready.
    #[allow(clippy::unused_async)] // will await HTTP once the client lands
    pub async fn fetch_references(&self, path: &StorePath) -> Result<Vec<StorePath>> {
        let _url = self.narinfo_url(path);
        Err(Error::NotImplemented(
            "Fetcher::fetch_references is not implemented yet",
        ))
    }
}
