//! Prebuilt index management: polling, downloading, and validating nix-index-database releases.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tracing::warn;

use crate::basename_index::BasenameIndex;
use crate::database::{FILE_MAGIC, generate_sidecars};

/// Decode an HTTP header value to a `String`, logging invalid UTF-8 instead of
/// silently discarding the header.
fn header_to_string(value: &reqwest::header::HeaderValue, name: &str) -> Option<String> {
    match value.to_str() {
        Ok(s) => Some(s.to_string()),
        Err(err) => {
            warn!(header = %name, error = %err, "ignoring header with invalid UTF-8");
            None
        }
    }
}

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

    /// Database sidecar generation failed.
    #[error("database error: {0}")]
    Database(#[from] crate::database::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Configuration for prebuilt index polling.
#[derive(Debug, Clone)]
pub struct PrebuiltConfig {
    /// Release URL pattern (e.g., <https://github.com/nix-community/nix-index-database/releases/download>).
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
            release_url:
                "https://github.com/nix-community/nix-index-database/releases/latest/download"
                    .to_string(),
            architecture: default_architecture(),
            small: false,
            cache_dir: Self::default_cache_dir(),
            refresh_interval: Duration::from_secs(3600),
        }
    }
}

impl PrebuiltConfig {
    #[cfg(feature = "prebuilt")]
    fn default_cache_dir() -> PathBuf {
        crate::nixdex_dir().join("prebuilt")
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
        .and_then(|v| header_to_string(v, "ETag"));

    let last_modified = response
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|v| header_to_string(v, "Last-Modified"));

    Ok(etag.or(last_modified))
}

/// Return the host architecture string used by `nix-index-database` assets.
///
/// Maps Rust `cfg` constants (`x86_64` + `linux` → `x86_64-linux`,
/// `aarch64` + `macos` → `aarch64-darwin`, etc.).
pub fn default_architecture() -> String {
    let arch = match std::env::consts::ARCH {
        "x86" => "i686",
        other => other,
    };
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{arch}-{os}")
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

/// Download the prebuilt index to `dest` with a `.tmp` + atomic rename.
///
/// The parent directory is created if it does not exist. The file is
/// validated as a NIXI database before the final rename.
///
/// # Errors
///
/// Returns an error if download, validation, or filesystem operations fail.
pub async fn download_to(config: &PrebuiltConfig, dest: &Path) -> Result<()> {
    let url = build_asset_url(config);
    let client = reqwest::Client::builder()
        .user_agent(concat!("nixdex/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|err| Error::Request(err.to_string()))?;

    let mut response = client
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

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let temp_path = dest.with_extension("tmp");
    let mut file = tokio::fs::File::create(&temp_path).await?;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| Error::Request(err.to_string()))?
    {
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
    }
    file.flush().await?;

    validate_nixi(&temp_path)?;
    tokio::fs::rename(&temp_path, dest).await?;

    // Generate nixdex sidecars for fast basename and package search lookups.
    // This is CPU-bound, so we use spawn_blocking to avoid blocking the async runtime.
    let dest_clone = dest.to_path_buf();
    tokio::task::spawn_blocking(move || generate_sidecars(&dest_clone))
        .await
        .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))??;

    Ok(())
}

/// Download the prebuilt index to a per-etag subdirectory under `cache_dir`,
/// then validate and return the directory containing `files`.
///
/// # Errors
///
/// Returns an error if download, validation, or filesystem operations fail.
pub async fn download_and_validate(config: &PrebuiltConfig) -> Result<PathBuf> {
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

    let headers = response.headers();
    let etag = headers
        .get(header::ETAG)
        .and_then(|v| header_to_string(v, "ETag"));
    let last_modified = headers
        .get(header::LAST_MODIFIED)
        .and_then(|v| header_to_string(v, "Last-Modified"));
    let cache_key = match etag.or(last_modified) {
        Some(e) => {
            // Derive a fixed-length safe digest from the header value.
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(e.as_bytes());
            format!("{:x}", hasher.finalize())
        }
        None => "unknown".to_string(),
    };

    let target_dir = config.cache_dir.join(cache_key);
    download_to(config, &target_dir.join("files")).await?;
    Ok(target_dir)
}

/// Validate that a file is a valid NIXI database by checking magic and version.
///
/// # Errors
///
/// Returns an error if the file is not a valid NIXI database.
fn validate_nixi(path: &Path) -> Result<()> {
    let mut file = fs::File::open(path)?;
    let mut header = [0u8; 12];
    if let Err(err) = std::io::Read::read_exact(&mut file, &mut header) {
        return if err.kind() == std::io::ErrorKind::UnexpectedEof {
            Err(Error::Validation("file too short for NIXI header".into()))
        } else {
            Err(Error::Io(err))
        };
    }

    let (magic, version_bytes) = header.split_at(4);
    if magic != FILE_MAGIC {
        return Err(Error::Validation(format!(
            "bad magic: expected {:?}, found {:?}",
            FILE_MAGIC, magic
        )));
    }

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
