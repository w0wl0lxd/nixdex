//! nixdex-core — library for building and searching a Nix package file index.

use std::io::{self, Read};

pub mod basename_index;
pub mod daemon;
pub mod database;
pub mod errors;
pub mod files;
pub mod frcode;
pub mod hydra;
pub mod index;
pub mod listings;
pub mod nixpkgs;
pub mod store_path;

pub use errors::{Error, Result};
pub use files::{ALL_FILE_TYPES, FileEntries, FileNode, FileTree, FileTreeEntry, FileType};
pub use hydra::Fetcher;
pub use nixpkgs::{
    EvalJob, EvalJobLine, EvalJobsOptions, PackageList, eval_expr_for_nixpkgs,
    list_packages_with_scopes,
};
pub use store_path::{Origin, StorePath};

/// Default binary-cache URL used when fetching file listings.
pub const CACHE_URL: &str = "https://cache.nixos.org";

/// Maximum uncompressed size accepted from a single zstd frame (defensive cap).
pub(crate) const MAX_ZSTD_FRAME_BYTES: u64 = 512 * 1024 * 1024;

/// Maximum back-reference window log for zstd decoders (defensive cap).
///
/// A window log of 27 corresponds to a 128 MiB window, which is the default
/// maximum selected by zstd at high compression levels. Capping the decoder
/// prevents a malicious frame from forcing a multi-gigabyte context allocation.
pub(crate) const ZSTD_WINDOW_LOG_MAX: u32 = 27;

/// Decompress `compressed` into a `Vec<u8>` while refusing to allocate more
/// than `max_bytes` for the output.
///
/// This bounds the memory impact of a zstd bomb: a tiny compressed payload
/// that expands to many gigabytes will hit the limit before OOMing the host.
pub(crate) fn bounded_zstd_decode(compressed: &[u8], max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
    decoder.window_log_max(ZSTD_WINDOW_LOG_MAX)?;
    let mut out = Vec::with_capacity(compressed.len().min(max_bytes));
    // Read one byte past the cap so we can tell whether the true size exceeds it.
    let limit = u64::try_from(max_bytes).map_or(u64::MAX, |m| m.saturating_add(1));
    let mut limited = decoder.take(limit);
    std::io::copy(&mut limited, &mut out)?;

    if out.len() > max_bytes {
        return Err(io::Error::other("zstd decompressed size exceeds limit"));
    }

    Ok(out)
}

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

#[cfg(test)]
mod tests {
    use super::bounded_zstd_decode;

    #[test]
    fn bounded_zstd_decode_honors_limit() {
        let original = vec![b'a'; 1024 * 1024];
        let compressed = zstd::encode_all(&original[..], 3).expect("compress");

        let err =
            bounded_zstd_decode(&compressed, original.len() - 1).expect_err("should exceed limit");
        assert!(err.to_string().contains("exceeds limit"));

        let decoded = bounded_zstd_decode(&compressed, original.len()).expect("decode");
        assert_eq!(decoded, original);
    }
}
