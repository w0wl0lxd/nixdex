//! Fetching file listings and references from the Nix binary cache.

use std::io::Read;
use std::time::Duration;

use reqwest::header::{self, HeaderValue};
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

impl Error {
    /// Returns `true` if this error represents an HTTP 404 response.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        match self {
            Self::Request(msg) => msg.contains("HTTP 404"),
            _ => false,
        }
    }
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
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
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

    /// Perform a GET with retries for transient failures.
    ///
    /// Retries on timeouts, connection errors, and HTTP 5xx responses.
    /// HTTP 404 and other client errors are not retried.
    async fn get_with_retry(&self, url: &str) -> Result<reqwest::Response> {
        const MAX_ATTEMPTS: u32 = 5;
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(16);
        let accept = HeaderValue::from_static("br, gzip, deflate");

        for attempt in 1..=MAX_ATTEMPTS {
            let response = self
                .client
                .get(url)
                .header(header::ACCEPT_ENCODING, accept.clone())
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status == reqwest::StatusCode::NOT_FOUND {
                        return Err(Error::Request(format!("{url}: HTTP {status}")));
                    }
                    if status.is_server_error() && attempt < MAX_ATTEMPTS {
                        tracing::warn!(url, attempt, status = %status, "server error, retrying");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(max_backoff);
                        continue;
                    }
                    if !status.is_success() {
                        return Err(Error::Request(format!("{url}: HTTP {status}")));
                    }
                    return Ok(resp);
                }
                Err(err) => {
                    let is_transient = err.is_timeout() || err.is_connect();
                    if is_transient && attempt < MAX_ATTEMPTS {
                        tracing::warn!(url, attempt, error = %err, "transient request error, retrying");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(max_backoff);
                        continue;
                    }
                    return Err(Error::Request(format!("{url}: {err}")));
                }
            }
        }

        Err(Error::Request(format!(
            "{url}: failed after {MAX_ATTEMPTS} attempts"
        )))
    }

    /// Fetch the raw file listing (`.ls`) bytes, decompressed when needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Request`] on HTTP failure, [`Error::Parse`] on decompress failure.
    pub async fn fetch_files(&self, path: &StorePath) -> Result<Vec<u8>> {
        let url = self.listing_url(path);
        let response = self.get_with_retry(&url).await?;
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
        let response = self.get_with_retry(&url).await?;
        response
            .text()
            .await
            .map_err(|err| Error::Request(err.to_string()))
    }

    /// Fetch the narinfo for a store path and parse its references and `.nar` URL.
    ///
    /// # Errors
    ///
    /// Propagates fetch/parse errors.
    pub async fn fetch_narinfo_details(
        &self,
        path: &StorePath,
    ) -> Result<(Vec<StorePath>, Option<String>)> {
        let text = self.fetch_narinfo(path).await?;
        let refs = parse_narinfo_references(&text, path.store_dir())?;
        let nar_url = parse_narinfo_url(&text);
        Ok((refs, nar_url))
    }

    /// Fetch runtime references of a store path from its narinfo.
    ///
    /// # Errors
    ///
    /// Propagates fetch/parse errors.
    pub async fn fetch_references(&self, path: &StorePath) -> Result<Vec<StorePath>> {
        let (refs, _url) = self.fetch_narinfo_details(path).await?;
        Ok(refs)
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
        let out = crate::bounded_zstd_decode(bytes, crate::files::MAX_LS_BYTES)
            .map_err(|err| Error::Parse(format!("zstd decode: {err}")))?;
        return Ok(out);
    }
    if bytes.starts_with(&[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]) {
        let out = bounded_xz_decode(bytes, crate::files::MAX_LS_BYTES)
            .map_err(|err| Error::Parse(format!("xz decode: {err}")))?;
        return Ok(out);
    }
    // Plain JSON (starts with `{` or whitespace then `{`).
    Ok(bytes.to_vec())
}

/// Decompress `compressed` into a `Vec<u8>` while refusing to allocate more
/// than `max_bytes` for the output.
fn bounded_xz_decode(compressed: &[u8], max_bytes: usize) -> std::io::Result<Vec<u8>> {
    let decoder = xz2::read::XzDecoder::new(compressed);
    let mut out = Vec::with_capacity(compressed.len().min(max_bytes));
    let limit = u64::try_from(max_bytes).map_or(u64::MAX, |m| m.saturating_add(1));
    let mut limited = decoder.take(limit);
    std::io::copy(&mut limited, &mut out)?;

    if out.len() > max_bytes {
        return Err(std::io::Error::other("xz decompressed size exceeds limit"));
    }

    Ok(out)
}

/// Parse the `URL:` field of a narinfo into the relative `.nar` path.
pub fn parse_narinfo_url(narinfo: &str) -> Option<String> {
    for line in narinfo.lines() {
        let Some(rest) = line.strip_prefix("URL:") else {
            continue;
        };
        return Some(rest.trim().to_string());
    }
    None
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
    fn decompress_zstd_payload() {
        let data = br#"{"root":{"type":"directory","entries":{}}}"#;
        let compressed = zstd::encode_all(&data[..], 3).expect("compress");
        let out = decompress_listing(&compressed).expect("zstd");
        assert_eq!(out, data);
    }

    #[test]
    fn decompress_xz_payload() {
        let data = br#"{"root":{"type":"directory","entries":{}}}"#;
        let mut compressed = Vec::new();
        {
            let mut enc = xz2::write::XzEncoder::new(&mut compressed, 6);
            std::io::Write::write_all(&mut enc, data).expect("write");
            std::io::Write::flush(&mut enc).expect("flush");
        }
        let out = decompress_listing(&compressed).expect("xz");
        assert_eq!(out, data);
    }

    #[test]
    fn parse_narinfo_url_line() {
        let text = "StorePath: /nix/store/abc-foo\nURL: nar/abc-foo.nar.xz\n";
        assert_eq!(
            parse_narinfo_url(text),
            Some("nar/abc-foo.nar.xz".to_string())
        );
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
