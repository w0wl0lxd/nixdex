//! Fetching file listings and references from the Nix binary cache.

use std::io::Read;

use thiserror::Error;

use crate::CACHE_URL;
use crate::files::FileTree;
use crate::store_path::{Origin, StorePath};

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
    client: reqwest::Client,
}

impl Fetcher {
    /// Create a fetcher targeting `base_url`.
    ///
    /// # Errors
    ///
    /// Returns an error when `base_url` is empty or the HTTP client fails to build.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(Error::Request("binary cache URL must not be empty".into()));
        }
        let client = reqwest::Client::builder()
            .user_agent(concat!("nixdex/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|err| Error::Request(err.to_string()))?;
        Ok(Self { base_url, client })
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

    /// Fetch the raw file listing (`.ls`) bytes, decompressed when needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Request`] on HTTP failure, [`Error::Parse`] on decompress failure.
    pub async fn fetch_files(&self, path: &StorePath) -> Result<Vec<u8>> {
        let url = self.listing_url(path);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|err| Error::Request(format!("{url}: {err}")))?;
        if !response.status().is_success() {
            return Err(Error::Request(format!("{url}: HTTP {}", response.status())));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|err| Error::Request(err.to_string()))?;
        decompress_listing(&bytes)
    }

    /// Fetch and parse a `.ls` listing into a [`FileTree`].
    ///
    /// # Errors
    ///
    /// Propagates fetch and parse errors.
    pub async fn fetch_file_tree(&self, path: &StorePath) -> Result<FileTree> {
        let bytes = self.fetch_files(path).await?;
        match FileTree::from_ls_json(&bytes) {
            Ok(tree) => Ok(tree),
            Err(crate::Error::Parse(msg)) => Err(Error::Parse(msg)),
            Err(other) => Err(Error::Parse(other.to_string())),
        }
    }

    /// Fetch the narinfo text for a store path.
    ///
    /// # Errors
    ///
    /// Returns request errors when the cache rejects the lookup.
    pub async fn fetch_narinfo(&self, path: &StorePath) -> Result<String> {
        let url = self.narinfo_url(path);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|err| Error::Request(format!("{url}: {err}")))?;
        if !response.status().is_success() {
            return Err(Error::Request(format!("{url}: HTTP {}", response.status())));
        }
        response
            .text()
            .await
            .map_err(|err| Error::Request(err.to_string()))
    }

    /// Fetch runtime references of a store path from its narinfo.
    ///
    /// # Errors
    ///
    /// Propagates fetch/parse errors.
    pub async fn fetch_references(&self, path: &StorePath) -> Result<Vec<StorePath>> {
        let text = self.fetch_narinfo(path).await?;
        parse_narinfo_references(&text, path.store_dir())
    }
}

/// Decompress a `.ls` payload by sniffing magic bytes.
///
/// Supports zstd (`28 b5 2f fd`), xz (`fd 37 7a 58 5a 00`), and plain JSON.
///
/// # Errors
///
/// Returns [`Error::Parse`] when decompression fails.
pub fn decompress_listing(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let mut decoder = zstd::stream::read::Decoder::new(bytes)
            .map_err(|err| Error::Parse(format!("zstd init: {err}")))?;
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|err| Error::Parse(format!("zstd decode: {err}")))?;
        return Ok(out);
    }
    if bytes.starts_with(&[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]) {
        return Err(Error::NotImplemented(
            "xz-compressed .ls listings are not supported yet",
        ));
    }
    // Plain JSON (starts with `{` or whitespace then `{`).
    Ok(bytes.to_vec())
}

/// Parse the `References:` field of a narinfo into store paths.
///
/// # Errors
///
/// Returns [`Error::Parse`] when a reference basename cannot be parsed.
pub fn parse_narinfo_references(narinfo: &str, store_dir: &str) -> Result<Vec<StorePath>> {
    let mut refs = Vec::new();
    for line in narinfo.lines() {
        let Some(rest) = line.strip_prefix("References:") else {
            continue;
        };
        for token in rest.split_whitespace() {
            let full = format!("{store_dir}/{token}");
            let origin = Origin {
                attr: String::new(),
                output: "out".to_string(),
                toplevel: false,
                system: None,
            };
            let Some(sp) = StorePath::parse(origin, &full) else {
                return Err(Error::Parse(format!(
                    "invalid reference store path: {token}"
                )));
            };
            refs.push(sp);
        }
        break;
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompress_plain_json() {
        let data = br#"{"root":{"type":"directory","entries":{}}}"#;
        let out = decompress_listing(data).expect("plain");
        assert_eq!(out, data);
    }

    #[test]
    fn parse_references_line() {
        let text = "\
StorePath: /nix/store/pg2zfrrbm58ynbjshhzkgg4q466spinf-hello-2.12.3
References: ias8xacs1h3jy7xgwi2awvim61k2ji6c-glibc-2.42-67 pg2zfrrbm58ynbjshhzkgg4q466spinf-hello-2.12.3
";
        let refs = parse_narinfo_references(text, "/nix/store").expect("refs");
        assert_eq!(refs.len(), 2);
        assert_eq!(
            refs.first().map(StorePath::hash),
            Some("ias8xacs1h3jy7xgwi2awvim61k2ji6c")
        );
        assert_eq!(refs.first().map(StorePath::name), Some("glibc-2.42-67"));
    }
}
