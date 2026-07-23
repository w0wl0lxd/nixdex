//! Content-defined chunking (CDC) for the prebuilt index — client foundation for
//! Tier 3 delta sync.
//!
//! This module splits an index blob into content-defined chunks using FastCDC and
//! fingerprints each chunk with BLAKE3. The resulting [`ChunkManifest`] is what an
//! upstream `nix-index-database` release would publish so that a client can fetch
//! only the chunks whose BLAKE3 hash it does not already have (served by `Range`
//! over a chunk-addressed store). Server-side manifest publishing and chunk
//! fetching are out of scope here; this is the client-side, server-independent half.
//!
//! Determinism: FastCDC is content-defined, so identical bytes always yield
//! identical chunk boundaries regardless of where the bytes appear in a file.

use std::io::Read;
use std::path::Path;

use fastcdc::v2020;
use thiserror::Error;

/// Default minimum chunk size (bytes). Below this, no cut point is considered.
pub const DEFAULT_MIN_CHUNK: u32 = 4 * 1024;

/// Default average chunk size (bytes). The target cut-point probability.
pub const DEFAULT_AVG_CHUNK: u32 = 64 * 1024;

/// Default maximum chunk size (bytes). A chunk is cut here even without a match.
pub const DEFAULT_MAX_CHUNK: u32 = 256 * 1024;

/// Errors produced while chunking or (de)serializing a [`ChunkManifest`].
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// The source data could not be read.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The chunk manifest could not be (de)serialized.
    #[error("manifest (de)serialization failed: {0}")]
    Manifest(String),
}

/// Convenience alias for this module's results.
pub type Result<T> = std::result::Result<T, Error>;

/// FastCDC tuning parameters.
#[derive(Debug, Clone, Copy)]
pub struct CdcConfig {
    /// Minimum chunk size in bytes.
    pub min_size: u32,
    /// Average chunk size in bytes (controls cut-point probability).
    pub avg_size: u32,
    /// Maximum chunk size in bytes.
    pub max_size: u32,
}

impl Default for CdcConfig {
    fn default() -> Self {
        Self {
            min_size: DEFAULT_MIN_CHUNK,
            avg_size: DEFAULT_AVG_CHUNK,
            max_size: DEFAULT_MAX_CHUNK,
        }
    }
}

/// A single content-defined chunk of the source data.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Chunk {
    /// Byte offset of the chunk within the source.
    pub offset: u64,
    /// Length of the chunk in bytes.
    pub length: u64,
    /// BLAKE3 hash of the chunk's bytes (`[u8; 32]`).
    pub hash: [u8; 32],
}

/// A serialized manifest of content-defined chunks for an index blob.
///
/// The manifest records each chunk's offset, length, and BLAKE3 hash. It is
/// sufficient for a client to (a) ask an index store for chunks by hash and
/// (b) verify each received chunk against its recorded hash.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChunkManifest {
    /// Total length of the original blob the chunks were cut from.
    pub total_length: u64,
    /// The chunks in source order.
    pub chunks: Vec<Chunk>,
}

impl ChunkManifest {
    /// Number of chunks in the manifest.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether the manifest contains no chunks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Total length of the original blob, in bytes.
    #[must_use]
    pub fn total_length(&self) -> u64 {
        self.total_length
    }

    /// Serialize the manifest to bytes (stable, postcard-encoded).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Manifest`] if serialization fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        postcard::to_stdvec(self).map_err(|err| Error::Manifest(err.to_string()))
    }

    /// Deserialize a manifest previously produced by [`ChunkManifest::to_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Manifest`] if deserialization fails.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        postcard::from_bytes(bytes).map_err(|err| Error::Manifest(err.to_string()))
    }
}

/// Hash a byte slice with BLAKE3, returning the raw `[u8; 32]` digest.
fn blake3_hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// Convert a `u64` to `usize`, returning `None` when it cannot be represented.
fn to_usize(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}

/// Convert a `usize` length to `u64`, saturating on the impossible overflow path.
fn len_to_u64(value: usize) -> u64 {
    match u64::try_from(value) {
        Ok(v) => v,
        Err(_) => u64::MAX,
    }
}

/// `(start, end)` byte range of `chunk` within a source buffer, if representable.
fn chunk_range(chunk: &Chunk) -> Option<(usize, usize)> {
    let start = to_usize(chunk.offset)?;
    let end = to_usize(chunk.offset + chunk.length)?;
    Some((start, end))
}

/// Cut `data` into content-defined chunks and fingerprint each with BLAKE3.
///
/// The returned chunks are contiguous and cover `data` exactly (when non-empty).
#[must_use]
pub fn chunk_bytes(data: &[u8], config: &CdcConfig) -> Vec<Chunk> {
    if data.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let chunker = v2020::FastCDC::new(data, config.min_size, config.avg_size, config.max_size);
    for entry in chunker {
        let offset = entry.offset;
        let end = offset + entry.length;
        let Some(slice) = data.get(offset..end) else {
            break;
        };
        chunks.push(Chunk {
            offset: len_to_u64(offset),
            length: len_to_u64(entry.length),
            hash: blake3_hash(slice),
        });
    }
    chunks
}

/// Chunk a file on disk and return its [`ChunkManifest`].
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read.
pub fn chunk_file(path: &Path, config: &CdcConfig) -> Result<ChunkManifest> {
    let mut file = std::fs::File::open(path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    Ok(build_manifest(&data, config))
}

/// Build a [`ChunkManifest`] for `data` with the given CDC configuration.
#[must_use]
pub fn build_manifest(data: &[u8], config: &CdcConfig) -> ChunkManifest {
    let chunks = chunk_bytes(data, config);
    let total_length = len_to_u64(data.len());
    ChunkManifest {
        total_length,
        chunks,
    }
}

/// Verify that every chunk's recorded BLAKE3 hash matches the bytes of `data` at
/// the recorded offset/length. Returns `true` only if the manifest fully matches.
#[must_use]
pub fn verify_manifest(data: &[u8], manifest: &ChunkManifest) -> bool {
    if manifest.total_length != len_to_u64(data.len()) {
        return false;
    }
    for chunk in &manifest.chunks {
        let Some((start, end)) = chunk_range(chunk) else {
            return false;
        };
        let Some(slice) = data.get(start..end) else {
            return false;
        };
        if blake3_hash(slice) != chunk.hash {
            return false;
        }
    }
    true
}

/// Reassemble a blob from chunk payloads, in manifest order.
///
/// `chunk_data` must contain one payload per manifest chunk, in order. This is
/// the client side of a delta sync: the caller supplies only the chunks it was
/// missing, fetched by hash from an index store.
///
/// # Errors
///
/// Returns [`Error::Manifest`] if the supplied chunks do not match the manifest
/// (wrong count, wrong hash, or wrong total length after reassembly).
pub fn reconstruct(chunk_data: &[&[u8]], manifest: &ChunkManifest) -> Result<Vec<u8>> {
    if chunk_data.len() != manifest.chunks.len() {
        return Err(Error::Manifest(format!(
            "expected {} chunks, got {}",
            manifest.chunks.len(),
            chunk_data.len()
        )));
    }

    let capacity = match to_usize(manifest.total_length) {
        Some(n) => n,
        None => 0,
    };
    let mut out = Vec::with_capacity(capacity);
    for (chunk, payload) in manifest.chunks.iter().zip(chunk_data.iter()) {
        if blake3_hash(payload) != chunk.hash {
            return Err(Error::Manifest("chunk payload hash mismatch".into()));
        }
        match len_to_u64(payload.len()) {
            len if len == chunk.length => {}
            _ => return Err(Error::Manifest("chunk payload length mismatch".into())),
        }
        out.extend_from_slice(payload);
    }

    match len_to_u64(out.len()) {
        len if len == manifest.total_length => Ok(out),
        _ => Err(Error::Manifest("reassembled length mismatch".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_data(len: usize, seed_byte: u8) -> Vec<u8> {
        let mut data = vec![0u8; len];
        for (i, b) in data.iter_mut().enumerate() {
            *b = seed_byte.wrapping_add((i % 251) as u8);
        }
        data
    }

    #[test]
    fn chunks_are_contiguous_and_cover_file() {
        let data = sample_data(200_000, 7);
        let config = CdcConfig::default();
        let chunks = chunk_bytes(&data, &config);

        assert!(!chunks.is_empty(), "non-empty input should yield chunks");
        let mut pos = 0usize;
        for chunk in &chunks {
            let (start, end) = chunk_range(chunk).expect("representable range");
            assert_eq!(start, pos);
            let slice = data.get(start..end).expect("in-bounds slice");
            assert_eq!(blake3_hash(slice), chunk.hash);
            pos = end;
        }
        assert_eq!(pos, data.len());
    }

    #[test]
    fn chunking_is_deterministic() {
        let data = sample_data(500_000, 42);
        let config = CdcConfig::default();
        let a = chunk_bytes(&data, &config);
        let b = chunk_bytes(&data, &config);
        assert_eq!(a, b);
    }

    #[test]
    fn chunk_lengths_respect_bounds() {
        let data = sample_data(700_000, 21);
        let config = CdcConfig::default();
        let chunks = chunk_bytes(&data, &config);
        assert!(!chunks.is_empty());
        let last = chunks.len() - 1;
        let max = u64::from(config.max_size);
        let min = u64::from(config.min_size);
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(chunk.length <= max, "chunk {i} exceeds max size");
            if i != last {
                assert!(chunk.length >= min, "interior chunk {i} below min size");
            }
        }
    }

    #[test]
    fn manifest_round_trips_through_bytes() {
        let data = sample_data(150_000, 3);
        let manifest = build_manifest(&data, &CdcConfig::default());
        let bytes = manifest.to_bytes().expect("serialize");
        let restored = ChunkManifest::from_bytes(&bytes).expect("deserialize");
        assert_eq!(manifest, restored);
    }

    #[test]
    fn verify_manifest_matches_and_detects_corruption() {
        let data = sample_data(150_000, 11);
        let manifest = build_manifest(&data, &CdcConfig::default());
        assert!(verify_manifest(&data, &manifest));

        let mut corrupted = data.clone();
        if let Some(b) = corrupted.last_mut() {
            *b ^= 0xff;
        }
        assert!(!verify_manifest(&corrupted, &manifest));
    }

    #[test]
    fn reconstruct_reassembles_original() {
        let data = sample_data(150_000, 5);
        let manifest = build_manifest(&data, &CdcConfig::default());

        let payloads: Vec<&[u8]> = manifest
            .chunks
            .iter()
            .filter_map(|c| {
                let (start, end) = chunk_range(c)?;
                data.get(start..end)
            })
            .collect();

        let rebuilt = reconstruct(&payloads, &manifest).expect("reconstruct");
        assert_eq!(rebuilt, data);
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(chunk_bytes(&[], &CdcConfig::default()).is_empty());
        let manifest = build_manifest(&[], &CdcConfig::default());
        assert!(manifest.is_empty());
        assert_eq!(manifest.total_length, 0);
    }
}
