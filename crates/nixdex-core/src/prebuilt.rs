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
    /// Maximum number of concurrent HTTP connections used to download a prebuilt
    /// index via segmented `Range` requests. Servers without `Range` support fall
    /// back to a single serial stream, so this only affects throughput, never
    /// correctness.
    pub max_connections: usize,
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
            max_connections: DEFAULT_MAX_CONNECTIONS,
        }
    }
}

/// Default number of concurrent connections for a segmented prebuilt download.
pub const DEFAULT_MAX_CONNECTIONS: usize = 8;

/// Files smaller than this are downloaded with the serial path instead of being
/// split into `Range` segments.
const MIN_SEGMENT_BYTES: usize = 1 << 20;

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
/// Build an HTTP client tuned for bulk prebuilt-index transfers.
///
/// Enables HTTP/2 flow-control adaptation, disables Nagle's algorithm, and
/// raises the per-host connection pool so many parallel `.ls`/`.narinfo` or
/// segmented `Range` requests are not throttled by the default idle limit.
fn build_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("nixdex/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .http2_adaptive_window(true)
        .tcp_nodelay(true)
        .pool_max_idle_per_host(32)
        .build()
        .map_err(|err| Error::Request(err.to_string()))
}

pub async fn check_update(config: &PrebuiltConfig) -> Result<Option<String>> {
    let url = build_asset_url(config);
    let client = build_client(Duration::from_secs(30))?;

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
/// The download uses a segmented parallel `Range` strategy when the server
/// advertises a `Content-Length` and honors `Range` requests (HTTP `206`);
/// otherwise it transparently falls back to the serial single-stream path.
/// The parent directory is created if it does not exist and the file is
/// validated as a NIXI database before the final rename.
///
/// # Errors
///
/// Returns an error if download, validation, or filesystem operations fail.
pub async fn download_to(config: &PrebuiltConfig, dest: &Path) -> Result<()> {
    download_index_file(config, dest).await?;

    // Generate nixdex sidecars for fast basename and package search lookups.
    // This is CPU-bound, so we use spawn_blocking to avoid blocking the async runtime.
    let dest_clone = dest.to_path_buf();
    tokio::task::spawn_blocking(move || generate_sidecars(&dest_clone))
        .await
        .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))??;

    Ok(())
}

/// Download and validate the prebuilt index into `dest`, without generating
/// sidecars. `download_to` layers sidecar generation on top of this.
///
/// # Errors
///
/// Returns an error if the download or validation fails.
async fn download_index_file(config: &PrebuiltConfig, dest: &Path) -> Result<()> {
    let url = build_asset_url(config);
    let client = build_client(Duration::from_secs(300))?;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let temp_path = dest.with_extension("tmp");

    // Prefer the parallel segmented path; degrade to the serial stream on any
    // indication that the server cannot serve `Range` requests.
    if let Err(err) =
        try_segmented_download(&client, &url, config.max_connections, &temp_path).await
    {
        warn!(error = %err, "segmented prebuilt download unavailable; using serial path");
        download_serial(&client, &url, &temp_path).await?;
    }

    validate_nixi(&temp_path)?;
    tokio::fs::rename(&temp_path, dest).await?;

    Ok(())
}

/// Split `total` bytes into at most `max_conn` contiguous, inclusive
/// `(start, end)` byte ranges. Each segment is at least `MIN_SEGMENT_BYTES`
/// unless the whole file fits in a single segment.
///
/// # Panics
///
/// Never panics; `max_conn` is clamped to at least `1` and `total` is assumed
/// to fit in `usize` (true for any file that can be downloaded).
fn plan_segments(total: usize, max_conn: usize) -> Vec<(usize, usize)> {
    if total == 0 {
        return Vec::new();
    }
    let by_size = total.div_ceil(MIN_SEGMENT_BYTES).max(1);
    let n = max_conn.max(1).min(by_size);

    let chunk = total / n;
    let remainder = total % n;

    let mut segments = Vec::with_capacity(n);
    let mut start = 0usize;
    for i in 0..n {
        let extra = usize::from(u8::from(i < remainder));
        let end_exclusive = start + chunk + extra;
        segments.push((start, end_exclusive - 1));
        start = end_exclusive;
    }
    segments
}

/// Build an `HTTP Range` header value for the inclusive `[start, end]` range.
fn range_header_value(start: usize, end: usize) -> reqwest::header::HeaderValue {
    reqwest::header::HeaderValue::from_str(&format!("bytes={start}-{end}"))
        .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("bytes=0-"))
}

/// Path of the temporary part file for segment `index` alongside `tmp`.
fn part_path(tmp: &Path, index: usize) -> PathBuf {
    tmp.with_extension(format!("tmp.part-{index}"))
}

/// Stream a response body into `path` chunk by chunk.
async fn stream_to_file(response: reqwest::Response, path: PathBuf) -> Result<()> {
    let mut file = tokio::fs::File::create(&path).await?;
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| Error::Request(err.to_string()))?
    {
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
    }
    file.flush().await?;
    Ok(())
}

/// Concatenate the `count` part files (named by [`part_path`]) into `tmp`.
async fn concat_parts(tmp: &Path, count: usize) -> Result<()> {
    let mut out = tokio::fs::File::create(tmp).await?;
    for i in 0..count {
        let part = part_path(tmp, i);
        let mut part_file = tokio::fs::File::open(&part).await?;
        tokio::io::copy(&mut part_file, &mut out)
            .await
            .map_err(Error::Io)?;
    }
    out.flush().await?;
    // The segments have been merged into `tmp`; remove them so they do not
    // linger in the cache directory after the download completes.
    for i in 0..count {
        let _ = tokio::fs::remove_file(part_path(tmp, i)).await;
    }
    Ok(())
}

/// Remove all `count` segment part files alongside `tmp`, ignoring errors.
async fn remove_parts(tmp: &Path, count: usize) {
    for i in 0..count {
        let _ = tokio::fs::remove_file(part_path(tmp, i)).await;
    }
}

/// Maximum number of times a missing segment is re-requested before the
/// segmented download is abandoned in favour of the serial path.
const MAX_SEGMENT_ATTEMPTS: usize = 3;

/// Per-segment outcome during a resumable segmented download.
enum SegmentError {
    /// The resource changed or the server stopped honoring `Range`; the resume
    /// must be abandoned and the caller should start a fresh download.
    Changed,
    /// A transient transport or HTTP error worth retrying.
    Transient,
}

/// Add an `If-Range` header when an ETag is known, so a changed resource makes
/// the server reply `200` (full body) instead of a stale `206` partial.
fn with_if_range(
    builder: reqwest::RequestBuilder,
    etag: Option<&String>,
) -> reqwest::RequestBuilder {
    match etag {
        Some(etag) => match reqwest::header::HeaderValue::from_str(etag) {
            Ok(value) => builder.header(reqwest::header::IF_RANGE, value),
            Err(_) => builder,
        },
        None => builder,
    }
}

/// Segments that still need downloading: those whose part file is missing or
/// not the exact expected length. Each entry carries the segment index together
/// with its `(start, end)` range so callers avoid indexing.
async fn missing_segments(tmp: &Path, segments: &[(usize, usize)]) -> Vec<(usize, (usize, usize))> {
    let mut missing = Vec::new();
    for (i, &(start, end)) in segments.iter().enumerate().skip(1) {
        let path = part_path(tmp, i);
        let complete = match tokio::fs::metadata(&path).await {
            Ok(meta) => {
                let expected = match u64::try_from(end - start + 1) {
                    Ok(value) => value,
                    Err(_) => u64::MAX,
                };
                meta.len() == expected
            }
            Err(_) => false,
        };
        if !complete {
            missing.push((i, (start, end)));
        }
    }
    missing
}

/// Fetch a single `Range` segment into `part`.
///
/// Returns `Err(Changed)` when the server stops honoring `Range` (e.g. replies
/// `200` because an `If-Range` precondition failed, or `416`) so the caller can
/// abort the resume cleanly, and `Err(Transient)` for retryable transport
/// failures, `5xx` responses, or `429` rate-limiting.
async fn fetch_segment(
    client: &reqwest::Client,
    url: &str,
    etag: Option<&String>,
    start: usize,
    end: usize,
    part: PathBuf,
) -> std::result::Result<(), SegmentError> {
    let response = with_if_range(
        client
            .get(url)
            .header(reqwest::header::RANGE, range_header_value(start, end)),
        etag,
    )
    .send()
    .await
    .map_err(|_| SegmentError::Transient)?;
    match response.status() {
        reqwest::StatusCode::PARTIAL_CONTENT => stream_to_file(response, part)
            .await
            .map_err(|_| SegmentError::Transient),
        // `200 OK` here means the `If-Range` precondition failed and the server
        // returned the full (changed) body: stop rather than merge mismatched
        // ranges into the file.
        reqwest::StatusCode::OK | reqwest::StatusCode::RANGE_NOT_SATISFIABLE => {
            Err(SegmentError::Changed)
        }
        status if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS => {
            Err(SegmentError::Transient)
        }
        _ => Err(SegmentError::Changed),
    }
}

/// Attempt a resumable parallel segmented `Range` download into `tmp`.
///
/// The first segment is probed to confirm the server honors `Range` (HTTP `206`)
/// and, when an ETag is known, that the resource is unchanged (`If-Range`). The
/// remaining segments are downloaded concurrently; on transient failure only the
/// missing parts are re-requested (up to [`MAX_SEGMENT_ATTEMPTS`]) rather than
/// restarting the whole file. Returns `Err` when the server lacks `Range`
/// support, the resource changed mid-download, or retries are exhausted; callers
/// should fall back to [`download_serial`].
async fn try_segmented_download(
    client: &reqwest::Client,
    url: &str,
    max_conn: usize,
    tmp: &Path,
) -> Result<()> {
    let head = client
        .head(url)
        .send()
        .await
        .map_err(|err| Error::Request(format!("HEAD {url}: {err}")))?;

    let total = head
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| Error::Request("missing Content-Length; cannot segment".into()))?;

    let etag = head
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| header_to_string(v, "ETag"));

    let Ok(total_usize) = usize::try_from(total) else {
        return Err(Error::Request(
            "file size exceeds addressable memory".into(),
        ));
    };
    if total_usize < MIN_SEGMENT_BYTES {
        return Err(Error::Request(
            "file too small for segmented download".into(),
        ));
    }

    let segments = plan_segments(total_usize, max_conn);
    let n = segments.len();

    // Probe the first segment with If-Range to confirm 206 and an unchanged resource.
    let Some(&(start0, end0)) = segments.first() else {
        return Err(Error::Request("no segments planned".into()));
    };
    let probe = with_if_range(
        client
            .get(url)
            .header(reqwest::header::RANGE, range_header_value(start0, end0)),
        etag.as_ref(),
    )
    .send()
    .await
    .map_err(|err| Error::Request(format!("GET {url}: {err}")))?;

    if probe.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(Error::Request(
            "server does not support HTTP Range or resource changed".into(),
        ));
    }
    stream_to_file(probe, part_path(tmp, 0)).await?;

    // Retry loop: redownload only the still-missing segments, sending If-Range so
    // a changed resource aborts the resume cleanly.
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        if attempt > MAX_SEGMENT_ATTEMPTS {
            remove_parts(tmp, n).await;
            return Err(Error::Request(
                "segmented download incomplete after retries".into(),
            ));
        }

        let missing = missing_segments(tmp, &segments).await;
        if missing.is_empty() {
            break;
        }

        let mut handles = Vec::with_capacity(missing.len());
        for (i, (start, end)) in missing {
            let client = client.clone();
            let url = url.to_string();
            let part = part_path(tmp, i);
            let etag = etag.clone();
            handles.push(tokio::spawn(async move {
                fetch_segment(&client, &url, etag.as_ref(), start, end, part).await
            }));
        }

        let mut changed = false;
        for handle in handles {
            if matches!(handle.await, Ok(Err(SegmentError::Changed))) {
                changed = true;
                break;
            }
        }

        if changed {
            remove_parts(tmp, n).await;
            return Err(Error::Request(
                "resource changed during segmented download".into(),
            ));
        }
    }

    concat_parts(tmp, n).await
}

/// Download the full file as a single serial stream into `tmp`.
async fn download_serial(client: &reqwest::Client, url: &str, tmp: &Path) -> Result<()> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|err| Error::Request(format!("GET {url}: {err}")))?;

    if !response.status().is_success() {
        return Err(Error::Request(format!(
            "GET {url}: HTTP {}",
            response.status()
        )));
    }

    let mut file = tokio::fs::File::create(tmp).await?;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| Error::Request(err.to_string()))?
    {
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
    }
    file.flush().await?;

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
    let client = build_client(Duration::from_secs(30))?;

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

    #[test]
    fn plan_segments_even() {
        let total = 8 * (1usize << 20);
        let segments = plan_segments(total, 8);
        assert_eq!(segments.len(), 8);
        assert_eq!(
            segments.first().copied().unwrap_or((0, 0)),
            (0, (1usize << 20) - 1)
        );
        assert_eq!(segments.last().copied().unwrap_or((0, 0)).1, total - 1);
        for (i, &(start, end)) in segments.iter().enumerate() {
            if i > 0 {
                assert_eq!(start, segments[i - 1].1 + 1);
            }
            assert!(end >= start);
        }
    }

    #[test]
    fn plan_segments_small_is_single_range() {
        // Files below MIN_SEGMENT_BYTES collapse to a single range.
        assert_eq!(plan_segments(100, 8), vec![(0, 99)]);
        assert_eq!(
            plan_segments((1usize << 20) - 1, 8),
            vec![(0, (1usize << 20) - 2)]
        );
    }

    #[test]
    fn plan_segments_multi_boundary_and_last_open() {
        let total = 3 * (1usize << 20) + 500;
        let segments = plan_segments(total, 2);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments.first().copied().unwrap_or((0, 0)).0, 0);
        // The last segment is open-ended to the final byte.
        assert_eq!(segments.last().copied().unwrap_or((0, 0)).1, total - 1);
        let mut expected = 0usize;
        for &(start, end) in &segments {
            assert_eq!(start, expected);
            expected = end + 1;
        }
        assert_eq!(expected, total);
    }

    #[test]
    fn plan_segments_caps_at_max_connections() {
        let total = 100 * (1usize << 20);
        assert_eq!(plan_segments(total, 8).len(), 8);
        // A single connection yields a single range spanning the whole file.
        assert_eq!(plan_segments(total, 1), vec![(0, total - 1)]);
    }

    #[test]
    fn plan_segments_empty() {
        assert!(plan_segments(0, 8).is_empty());
    }

    // --- Mock HTTP server tests for segmented / serial download ---------------

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::Router;
    use axum::body::Body;
    use axum::extract::{Request, State};
    use axum::http::{StatusCode, header};
    use axum::response::Response;

    struct MockState {
        data: Arc<Vec<u8>>,
        honor_range: bool,
        /// When set, fail the first `Range` request whose start offset is non-zero
        /// (i.e. any segment other than the probe) with `503`, to exercise resume.
        fail_first_range: Arc<AtomicBool>,
        /// Set by the handler the first time it serves a failing request.
        failed: Arc<AtomicBool>,
    }

    async fn mock_handler(State(state): State<Arc<MockState>>, req: Request) -> Response {
        let total = state.data.len();
        let (status, body, content_range) = if state.honor_range {
            match req
                .headers()
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
            {
                Some(s) if s.starts_with("bytes=") => {
                    let rest = &s["bytes=".len()..];
                    match rest.split_once('-') {
                        Some((a, b)) => {
                            let start: usize = a.parse().unwrap_or(0);
                            let end: usize = b.parse().unwrap_or(total.saturating_sub(1));
                            let end = end.min(total.saturating_sub(1));
                            if start <= end && end < total {
                                if state.fail_first_range.load(Ordering::Relaxed)
                                    && start > 0
                                    && !state.failed.swap(true, Ordering::Relaxed)
                                {
                                    (StatusCode::SERVICE_UNAVAILABLE, Vec::new(), None)
                                } else {
                                    let slice = state.data[start..=end].to_vec();
                                    (
                                        StatusCode::PARTIAL_CONTENT,
                                        slice,
                                        Some(format!("bytes {start}-{end}/{total}")),
                                    )
                                }
                            } else {
                                (StatusCode::RANGE_NOT_SATISFIABLE, Vec::new(), None)
                            }
                        }
                        None => (StatusCode::OK, state.data.to_vec(), None),
                    }
                }
                _ => (StatusCode::OK, state.data.to_vec(), None),
            }
        } else {
            (StatusCode::OK, state.data.to_vec(), None)
        };

        let content_length = body.len();
        let mut resp = Response::new(Body::from(body));
        *resp.status_mut() = status;
        if let Some(cr) = content_range {
            if let Ok(v) = header::HeaderValue::from_str(&cr) {
                resp.headers_mut().insert(header::CONTENT_RANGE, v);
            }
        }
        if let Ok(v) = header::HeaderValue::from_str(&content_length.to_string()) {
            resp.headers_mut().insert(header::CONTENT_LENGTH, v);
        }
        resp
    }

    /// Build a synthetic-but-valid NIXI database: a 12-byte header followed by
    /// `payload` random bytes, so `validate_nixi` and the reconstruction assert
    /// both pass.
    fn make_fake_db(payload: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(12 + payload);
        data.extend_from_slice(FILE_MAGIC);
        data.extend_from_slice(&1u64.to_le_bytes());
        let mut tail = vec![0u8; payload];
        fastrand::fill(&mut tail);
        data.extend_from_slice(&tail);
        data
    }

    async fn spawn_mock_server(state: Arc<MockState>) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let app = Router::new().fallback(mock_handler).with_state(state);
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        addr
    }

    #[tokio::test]
    async fn parallel_range_download_reconstructs() {
        let data = Arc::new(make_fake_db(2 * (1 << 20) + 1234));
        let addr = spawn_mock_server(Arc::new(MockState {
            data: data.clone(),
            honor_range: true,
            fail_first_range: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
        }))
        .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("files");
        let config = PrebuiltConfig {
            release_url: format!("http://{addr}"),
            architecture: "x86_64-linux".into(),
            small: false,
            cache_dir: dir.path().join("cache"),
            refresh_interval: std::time::Duration::ZERO,
            max_connections: 8,
        };

        download_index_file(&config, &dest)
            .await
            .expect("download should succeed");
        let got = fs::read(&dest).expect("read downloaded file");
        assert_eq!(got, *data);
    }

    #[tokio::test]
    async fn range_ignoring_server_falls_back_to_serial() {
        let data = Arc::new(make_fake_db(2 * (1 << 20) + 1234));
        let addr = spawn_mock_server(Arc::new(MockState {
            data: data.clone(),
            honor_range: false,
            fail_first_range: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
        }))
        .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("files");
        let config = PrebuiltConfig {
            release_url: format!("http://{addr}"),
            architecture: "x86_64-linux".into(),
            small: false,
            cache_dir: dir.path().join("cache"),
            refresh_interval: std::time::Duration::ZERO,
            max_connections: 8,
        };

        download_index_file(&config, &dest)
            .await
            .expect("serial fallback should succeed");
        let got = fs::read(&dest).expect("read downloaded file");
        assert_eq!(got, *data);
    }

    #[tokio::test]
    async fn segmented_download_resumes_after_transient_failure() {
        let data = Arc::new(make_fake_db(2 * (1 << 20) + 1234));
        // Keep the state handle so we can confirm the injected transient
        // failure actually occurred; otherwise this test could pass via the
        // serial fallback and prove nothing about segment resume.
        let state = Arc::new(MockState {
            data: data.clone(),
            honor_range: true,
            // Force the first non-probe segment request to fail once with 503;
            // the download must retry just that segment and still succeed.
            fail_first_range: Arc::new(AtomicBool::new(true)),
            failed: Arc::new(AtomicBool::new(false)),
        });
        let addr = spawn_mock_server(state.clone()).await;

        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("files");
        let config = PrebuiltConfig {
            release_url: format!("http://{addr}"),
            architecture: "x86_64-linux".into(),
            small: false,
            cache_dir: dir.path().join("cache"),
            refresh_interval: std::time::Duration::ZERO,
            max_connections: 8,
        };

        download_index_file(&config, &dest)
            .await
            .expect("resumable download should succeed after transient failure");
        let got = fs::read(&dest).expect("read downloaded file");
        assert_eq!(got, *data);
        assert!(
            state.failed.load(Ordering::Relaxed),
            "injected transient failure never occurred; segmented path not exercised"
        );
        // No part files should remain after a successful resumable download.
        assert!(!dir.path().join("files.tmp.part-0").exists());
        assert!(!dir.path().join("files.tmp.part-1").exists());
    }
}
