//! Prebuilt index management: polling, downloading, and validating nix-index-database releases.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header;
use thiserror::Error;

use crate::basename_index::BasenameIndex;
use crate::database::FILE_MAGIC;

/// Errors that can occur during prebuilt index management.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// HTTP request to check or download the prebuilt index failed.
    #[error("HTTP request failed: {0}")]
    Request(String),

    /// Downloaded file validation failed (bad magic or version).
    #[error("validation failed: {0}")]
    Validation(String),

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Basename secondary index error.
    #[error("basename index error: {0}")]
    BasenameIndex(#[from] crate::basename_index::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Configuration for prebuilt index polling.
#[derive(Debug, Clone)]
pub struct PrebuiltConfig {
    /// Release URL pattern (e.g., "https://github.com/nix-community/nix-index-database/releases/download").
    pub release_url: String,
    /// Architecture identifier (e.g., "x86_64-linux").
    pub architecture: String,
    /// Whether to use the `-small` variant.
    pub small: bool,
    /// Cache directory for prebuilt indexes.
    pub cache_dir: PathBuf,
    /// Refresh interval.
    pub refresh_interval: Duration,
}

impl Default for PrebuiltConfig {
    fn default() -> Self {
        Self {
            release_url: "https://github.com/nix-community/nix-index-database/releases/download"
                .to_string(),
            architecture: "x86_64-linux".to_string(),
            small: false,
            cache_dir: Self::default_cache_dir(),
            refresh_interval: Duration::from_secs(3600),
        }
    }
}

impl PrebuiltConfig {
    #[cfg(feature = "prebuilt")]
    fn default_cache_dir() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("nixdex")
            .join("prebuilt")
    }

    #[cfg(not(feature = "prebuilt"))]
    fn default_cache_dir() -> PathBuf {
        PathBuf::from(".cache/nixdex/prebuilt")
    }
}

/// Check if a new prebuilt index is available by comparing ETag or Last-Modified.
///
/// Returns `Some(etag_or_modified)` if the remote has changed, `None` if unchanged.
///
/// # Errors
///
/// Returns an error if the HTTP request fails.
pub async fn check_update(config: &PrebuiltConfig) -> Result<Option<String>> {
    let url = build_asset_url(config);
    let client = reqwest::Client::builder()
        .user_agent(concat!("nixdex/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|err| Error::Request(err.to_string()))?;

    let response = client
        .head(&url)
        .send()
        .await
        .map_err(|err| Error::Request(format!("HEAD {url}: {err}")))?;

    if !response.status().is_success() {
        return Err(Error::Request(format!(
            "HEAD {url}: HTTP {}",
            response.status()
        )));
    }

    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let last_modified = response
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    Ok(etag.or(last_modified))
}

/// Build the asset URL for the configured architecture and variant.
fn build_asset_url(config: &PrebuiltConfig) -> String {
    let filename = if config.small {
        format!("index-{}-small", config.architecture)
    } else {
        format!("index-{}", config.architecture)
    };
    format!("{}/{}", config.release_url, filename)
}

/// Download the prebuilt index to a temporary file, then validate and move to cache.
///
/// # Errors
///
/// Returns an error if download, validation, or filesystem operations fail.
pub async fn download_and_validate(config: &PrebuiltConfig) -> Result<PathBuf> {
    let url = build_asset_url(config);
    let client = reqwest::Client::builder()
        .user_agent(concat!("nixdex/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|err| Error::Request(err.to_string()))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|err| Error::Request(format!("GET {url}: {err}")))?;

    if !response.status().is_success() {
        return Err(Error::Request(format!(
            "GET {url}: HTTP {}",
            response.status()
        )));
    }

    let etag = match response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
    {
        Some(e) => e,
        None => "unknown",
    };

    let target_dir = config.cache_dir.join(etag);
    fs::create_dir_all(&target_dir)?;

    let temp_path = target_dir.join("files.tmp");
    let mut file = fs::File::create(&temp_path)?;

    let bytes = response.bytes().await.map_err(|err| Error::Request(err.to_string()))?;
    std::io::Write::write_all(&mut file, &bytes)?;

    validate_nixi(&temp_path)?;

    let final_path = target_dir.join("files");
    fs::rename(&temp_path, &final_path)?;

    Ok(target_dir)
}

/// Validate that a file is a valid NIXI database by checking magic and version.
///
/// # Errors
///
/// Returns an error if the file is not a valid NIXI database.
fn validate_nixi(path: &Path) -> Result<()> {
    let data = fs::read(path)?;

    if data.len() < 12 {
        return Err(Error::Validation("file too short for NIXI header".into()));
    }

    let magic = data.get(0..4).ok_or_else(|| Error::Validation("file too short for magic".into()))?;
    if magic != FILE_MAGIC {
        return Err(Error::Validation(format!(
            "bad magic: expected {:?}, found {:?}",
            FILE_MAGIC, magic
        )));
    }

    let version_bytes = data.get(4..12).ok_or_else(|| Error::Validation("file too short for version".into()))?;
    let version = u64::from_le_bytes(
        version_bytes
            .try_into()
            .map_err(|_| Error::Validation("version slice too short".into()))?,
    );
    if version != 1 && version != 2 {
        return Err(Error::Validation(format!("unsupported version: {version}")));
    }

    Ok(())
}

/// Update the `current` symlink to point to the given directory.
///
/// Creates the symlink atomically.
///
/// # Errors
///
/// Returns an error if symlink creation fails.
pub fn update_current_symlink(cache_dir: &Path, target_dir: &Path) -> Result<()> {
    let current_link = cache_dir.join("current");
    let temp_link = cache_dir.join("current.tmp");

    if temp_link.exists() {
        fs::remove_file(&temp_link)?;
    }

    std::os::unix::fs::symlink(target_dir, &temp_link)?;
    fs::rename(&temp_link, &current_link)?;

    Ok(())
}

/// Open the basename index from the current prebuilt directory.
///
/// # Errors
///
/// Returns an error if the current symlink or index files are missing/invalid.
pub fn open_current_basename_index(cache_dir: &Path) -> Result<BasenameIndex> {
    let current = cache_dir.join("current");
    if !current.exists() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "current symlink does not exist",
        )));
    }

    BasenameIndex::open(&current).map_err(Error::from)
}

/// Get the path to the current prebuilt directory.
///
/// Returns `None` if the symlink does not exist.
///
/// # Errors
///
/// Returns an error if the symlink cannot be read.
pub fn current_dir(cache_dir: &Path) -> Result<Option<PathBuf>> {
    let current = cache_dir.join("current");
    if !current.exists() {
        return Ok(None);
    }

    let target = fs::read_link(&current)?;
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_nixi_rejects_short_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("files");
        fs::write(&path, b"short").expect("write");
        let err = validate_nixi(&path).expect_err("should fail");
        assert!(matches!(err, Error::Validation(_)));
    }

    #[test]
    fn validate_nixi_rejects_bad_magic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("files");
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(b"BAD!");
        data[4..12].copy_from_slice(&1u64.to_le_bytes());
        fs::write(&path, &data).expect("write");
        let err = validate_nixi(&path).expect_err("should fail");
        assert!(matches!(err, Error::Validation(_)));
    }

    #[test]
    fn validate_nixi_accepts_v1() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("files");
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(FILE_MAGIC);
        data[4..12].copy_from_slice(&1u64.to_le_bytes());
        fs::write(&path, &data).expect("write");
        validate_nixi(&path).expect("should accept v1");
    }

    #[test]
    fn validate_nixi_accepts_v2() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("files");
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(FILE_MAGIC);
        data[4..12].copy_from_slice(&2u64.to_le_bytes());
        fs::write(&path, &data).expect("write");
        validate_nixi(&path).expect("should accept v2");
    }
}
