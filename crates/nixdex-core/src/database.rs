//! Creating and searching NIXI-compatible file databases.
//!
//! Format (compatible with upstream `nix-index`):
//! - magic `NIXI` + `u64` LE version `1` or `2`
//! - v1: single zstd frame of concatenated package frcode data
//! - v2: independent zstd frames cut only at package boundaries, followed by a
//!   trailing zstd skippable frame with a seek table
//! - per package: file entries, then footer entry with metadata `p` and JSON `StorePath`
//!
//! Optional secondary index (nixdex): basename FST sidecars next to `files`
//! (see [`crate::basename_index`]).

use std::fs::{self, File};
use std::io;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, WriteBytesExt};
use grep::matcher::{LineMatchKind, Matcher, NoError};
use grep::regex::RegexMatcherBuilder;
use memchr;
use mmap_guard;
use rayon::prelude::*;
use regex::bytes::{Regex, RegexBuilder};
use regex_syntax::ast::{AssertionKind, Ast};
use serde::Serialize;
use sonic_rs;
use thiserror::Error;

use indexmap::IndexSet;
use roaring::RoaringBitmap;

use crate::basename_index::{
    BasenameIndex, BasenameIndexBuilder, FST_FILE, NAMES_FILE, POSTINGS_FILE,
};
use crate::files::{FileNode, FileTree, FileTreeEntry, FileType};
use crate::frcode;
use crate::ngram_index::NgramIndex;
use crate::nixpkgs::PackageMeta;
use crate::path_index::{PathIndex, PathIndexBuilder};
use crate::redb_index;
use crate::store_path::StorePath;

/// Database format versions supported by this build.
const SUPPORTED_VERSIONS: &[u64] = &[1, 2];

/// Default on-disk format version written by [`Writer::create`].
const DEFAULT_WRITE_VERSION: u64 = 2;

/// Magic bytes identifying a nix-index / nixdex database file.
pub const FILE_MAGIC: &[u8] = b"NIXI";

/// Maximum length (in bytes) of a user-supplied search regex.
const MAX_PATTERN_BYTES: usize = 1024;

/// Maximum memory (in bytes) allowed for regex compilation (NFA/DFA).
const REGEX_SIZE_LIMIT: usize = 1_000_000;

/// Magic of the trailing zstd skippable frame used by version 2 seek tables.
const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;

/// Byte offset right after the file magic and version header.
const DATA_START: usize = 12;

/// Frame map sidecar filename for selective decompression.
const FRAME_MAP_FILE: &str = "files.frame_map";

/// Magic for the frame map sidecar.
const FRAME_MAP_MAGIC: &[u8] = b"NFRM";

/// Frame map sidecar version.
const FRAME_MAP_VERSION: u32 = 1;

/// Attrs sidecar filename for incremental builds.
const ATTRS_FILE: &str = "files.attrs";

/// Magic for the attrs sidecar.
const ATTRS_MAGIC: &[u8] = b"NATR";

/// Attrs sidecar version.
const ATTRS_VERSION: u32 = 1;

/// Maximum size of the attrs sidecar (defensive cap).
const MAX_ATTRS_BYTES: usize = 1024 * 1024 * 1024;

/// Defensive cap on the number of v2 frames (seek table entries).
const MAX_FRAME_COUNT: usize = 1024 * 1024;

/// Defensive cap on the on-disk database file size.
const MAX_DATABASE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Errors that can occur when reading or writing a database.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Encountered an unsupported on-disk file type marker (bad magic).
    #[error(
        "expected file to start with nix-index file magic 'NIXI', but found '{found:?}' (is this a valid nix-index database file?)"
    )]
    UnsupportedFileType {
        /// Raw type marker found in the database.
        found: Vec<u8>,
    },

    /// Database format version is newer (or older) than supported.
    #[error(
        "this executable only supports the nix-index database versions 1 and 2, but found a database with version {found}"
    )]
    UnsupportedVersion {
        /// Version number found in the header.
        found: u64,
    },

    /// Database payload is internally inconsistent or truncated.
    #[error("database corrupt: {0}")]
    Corrupt(&'static str),

    /// Package entry required by a file listing was missing.
    #[error("database corrupt: missing package entry")]
    MissingPackageEntry,

    /// frcode codec reported a corrupt stream.
    #[error("database corrupt, frcode error: {0}")]
    Frcode(#[from] frcode::Error),

    /// A file entry could not be parsed.
    #[error("database corrupt: could not parse entry: {entry:?}")]
    EntryParse {
        /// Raw entry bytes.
        entry: Vec<u8>,
    },

    /// A store-path JSON blob could not be parsed.
    #[error("database corrupt: could not parse store path: {path:?}")]
    StorePathParse {
        /// Raw store-path blob.
        path: Vec<u8>,
    },

    /// Regular expression compilation failed.
    #[error("invalid search pattern: {0}")]
    Regex(#[from] regex::Error),

    /// ripgrep `grep` matcher compilation failed.
    #[error("grep matcher error: {0}")]
    Grep(String),

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failed.
    #[error("JSON error: {0}")]
    Json(String),

    /// Requested functionality is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    /// Basename secondary index is missing or unreadable.
    #[error("secondary index: {0}")]
    SecondaryIndex(#[from] crate::basename_index::Error),

    /// Path secondary index is missing or unreadable.
    #[error("path index: {0}")]
    PathIndex(#[from] crate::path_index::Error),

    /// Entry-level secondary index is missing or unreadable.
    #[error("entry index: {0}")]
    EntryIndex(#[from] crate::entry_index::IndexError),

    /// Trigram (n-gram) secondary index is missing or unreadable.
    #[error("ngram index: {0}")]
    NgramIndex(#[from] crate::ngram_index::Error),

    /// Path-level per-entry cache is missing or unreadable.
    #[error("path entry index: {0}")]
    PathEntryIndex(#[from] crate::path_entry_index::IndexError),

    /// Path-level trigram index is missing or unreadable.
    #[error("path trigram index: {0}")]
    PathTrigramIndex(#[from] crate::path_trigram_index::Error),
}

/// Convenience alias for this module.
pub type Result<T> = std::result::Result<T, Error>;

/// Writer that creates a new NIXI database file.
pub struct Writer {
    /// Path of the NIXI `files` blob (used to place sidecars beside it).
    path: PathBuf,
    /// Zstd compression level used during `finish`.
    level: i32,
    /// On-disk format version to write (1 or 2).
    version: u64,
    /// Accumulated raw frcode data for all packages added so far.
    raw: Vec<u8>,
    /// End offsets in `raw` for each complete package frame.
    boundaries: Vec<usize>,
    /// Optional basename index accumulated during `add`.
    basename_index: BasenameIndexBuilder,
    /// Optional path index accumulated during `add`.
    path_index: PathIndexBuilder,
    /// Open file handle for streaming frame writes.
    file: Option<File>,
    /// Compressed lengths of frames already written to `file`.
    frame_lengths: Vec<u32>,
    /// Global ordinal-to-frame map accumulated across flushed chunks.
    frame_map: Vec<u32>,
    /// Whether `finish` has already materialized the on-disk file.
    finished: bool,
    /// Accumulated (attr, output, hash) triples for the attrs sidecar.
    attrs: Vec<(String, String, String)>,
    /// Optional redb index written alongside the NIXI database.
    redb: Option<redb_index::Writer>,
}

impl Drop for Writer {
    fn drop(&mut self) {
        if !self.finished {
            // Best-effort finish; callers should prefer `finish()` for error reporting.
            let _ = self.do_finish();
        }
    }
}

impl Writer {
    /// Creates a new database at the given path with the specified zstd compression level.
    ///
    /// Writes version 2 by default and does not build the optional `redb` exact-path
    /// sidecar. Use [`create_with_version`](Self::create_with_version) to request a
    /// different version or enable the `redb` sidecar.
    ///
    /// # Errors
    ///
    /// Returns an error if the version is unsupported.
    pub fn create<P: AsRef<Path>>(path: P, level: i32) -> Result<Self> {
        Self::create_with_version(path, level, DEFAULT_WRITE_VERSION, false)
    }

    /// Creates a new database at the given path with the specified zstd compression level,
    /// on-disk format version, and optional `redb` exact-path sidecar.
    ///
    /// `version` must be `1` or `2`.
    ///
    /// # Errors
    ///
    /// Returns an error if the version is unsupported.
    pub fn create_with_version<P: AsRef<Path>>(
        path: P,
        level: i32,
        version: u64,
        enable_redb: bool,
    ) -> Result<Self> {
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(Error::UnsupportedVersion { found: version });
        }
        let path = path.as_ref().to_path_buf();
        let mut file = File::create(&path)?;
        file.write_all(FILE_MAGIC)?;
        file.write_u64::<LittleEndian>(version)?;

        // If the redb sidecar is disabled, remove any stale sidecars left over
        // from a previous build so the reader cannot open outdated exact-path
        // indexes.
        if !enable_redb && let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            let _ = fs::remove_file(dir.join(redb_index::DEFAULT_FILE));
            let _ = fs::remove_file(dir.join(redb_index::PATH_CACHE_FILE));
        }

        let redb = if enable_redb {
            path.parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .map(redb_index::Writer::create)
                .transpose()
                .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))?
        } else {
            None
        };

        Ok(Self {
            path,
            level,
            version,
            raw: Vec::new(),
            boundaries: Vec::new(),
            basename_index: BasenameIndexBuilder::new(),
            path_index: PathIndexBuilder::new(),
            file: Some(file),
            frame_lengths: Vec::new(),
            frame_map: Vec::new(),
            finished: false,
            attrs: Vec::new(),
            redb,
        })
    }

    /// Add a package and its file tree to the database.
    ///
    /// Entries are only added if their path starts with `filter_prefix` and does
    /// not start with any of `exclude_prefixes`.
    /// Packages with no matching entries are skipped.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding or writing fails.
    pub fn add(&mut self, path: &StorePath, files: &FileTree, filter_prefix: &[u8]) -> Result<()> {
        self.add_excluding(path, files, filter_prefix, &[])
    }

    /// Add a package and its file tree to the database, excluding paths.
    ///
    /// Entries are only added if their path starts with `filter_prefix` and does
    /// not start with any of `exclude_prefixes`.
    /// Packages with no matching entries are skipped.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding or writing fails.
    pub fn add_excluding(
        &mut self,
        path: &StorePath,
        files: &FileTree,
        filter_prefix: &[u8],
        exclude_prefixes: &[&[u8]],
    ) -> Result<()> {
        let entries: Vec<FileTreeEntry> = files
            .to_list(filter_prefix)
            .into_iter()
            .filter(|entry| {
                !exclude_prefixes
                    .iter()
                    .any(|prefix| entry.path.starts_with(prefix))
                    && entry.is_encodable()
            })
            .collect();
        if entries.is_empty() {
            return Ok(());
        }

        let label = format!("{}.{}", path.origin().attr, path.origin().output);
        let paths: Vec<Vec<u8>> = entries.iter().map(|e| e.path.clone()).collect();
        let ordinal = self.basename_index.record_package(label, paths.clone())?;

        // Record full paths in the path index using the same ordinal
        self.path_index.record_package(ordinal, paths)?;

        // Add to redb index if enabled, using the same filtered entries as the NIXI output.
        if let Some(redb) = &mut self.redb {
            redb.add(path, &entries)
                .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))?;
        }

        let mut package = Vec::new();
        let json = sonic_rs::to_vec(path).map_err(|err| Error::Json(err.to_string()))?;
        let mut fr = frcode::Encoder::new(&mut package, b"p".to_vec(), json)?;
        for entry in entries {
            entry.encode(&mut fr)?;
        }
        fr.finish()?;

        self.raw.extend_from_slice(&package);
        self.boundaries.push(self.raw.len());

        // Record (attr, output, hash) for the attrs sidecar.
        self.attrs.push((
            path.origin().attr.clone(),
            path.origin().output.clone(),
            path.hash().to_string(),
        ));

        Ok(())
    }

    /// Return the current estimated uncompressed size of the database in bytes.
    #[must_use]
    pub fn estimated_size(&self) -> u64 {
        match u64::try_from(self.raw.len()) {
            Ok(len) => len,
            Err(_) => u64::MAX,
        }
    }

    /// Compress and write the current v2 raw chunk as one or more frames.
    ///
    /// For v1 this is a no-op because the whole stream must be a single frame.
    /// After flushing, `self.raw` and `self.boundaries` are cleared so the next
    /// chunk starts fresh.
    ///
    /// # Errors
    ///
    /// Returns an error if compression or I/O fails.
    pub fn flush_chunk(&mut self) -> Result<()> {
        if self.raw.is_empty() || self.version != 2 {
            return Ok(());
        }

        let raw = std::mem::take(&mut self.raw);
        let boundaries = std::mem::take(&mut self.boundaries);

        let parallelism = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .max(1);

        let frames = frame_ranges(&raw, &boundaries, parallelism)?;
        if frames.len() > MAX_FRAME_COUNT {
            return Err(Error::Corrupt("frame count exceeds maximum"));
        }

        let base_frame_idx = self.frame_lengths.len();
        let mut boundary_idx = 0usize;
        for (frame_idx, (_frame_start, frame_end)) in frames.iter().enumerate() {
            while boundary_idx < boundaries.len() {
                let boundary = *boundaries
                    .get(boundary_idx)
                    .ok_or(Error::Corrupt("package boundary index out of range"))?;
                if boundary <= *frame_end {
                    self.frame_map.push(
                        u32::try_from(frame_idx + base_frame_idx)
                            .map_err(|_| Error::Corrupt("frame index overflow"))?,
                    );
                    boundary_idx += 1;
                } else {
                    break;
                }
            }
        }

        for (ord, &frame_idx) in self
            .frame_map
            .iter()
            .enumerate()
            .skip(self.frame_map.len() - boundaries.len())
        {
            let local_idx =
                usize::try_from(frame_idx).map_err(|_| Error::Corrupt("frame index overflow"))?;
            let local_idx = local_idx - base_frame_idx;
            let (frame_start, frame_end) = frames
                .get(local_idx)
                .ok_or(Error::Corrupt("frame index out of range"))?;
            let boundary = *boundaries
                .get(ord)
                .ok_or(Error::Corrupt("package boundary index out of range"))?;
            if boundary < *frame_start || boundary > *frame_end {
                return Err(Error::Corrupt("package boundary outside assigned frame"));
            }
        }

        let slices: Vec<&[u8]> = frames
            .iter()
            .map(|(start, end)| {
                raw.get(*start..*end)
                    .ok_or(Error::Corrupt("package boundary out of range"))
            })
            .collect::<Result<Vec<_>>>()?;

        let file = self
            .file
            .as_mut()
            .ok_or_else(|| Error::Io(std::io::Error::other("database file not open")))?;

        // Compress the slices in parallel using Rayon, with a single-threaded
        // `zstd::bulk::Compressor` instantiated inside each task. This keeps the
        // compression fast while avoiding the per-frame `Encoder` allocations and
        // the multi-worker `CCtx` memory explosion that makes high compression
        // levels allocate ~1 GiB per core.
        let level = self.level;
        let compressed: Vec<io::Result<Vec<u8>>> = slices
            .par_iter()
            .map(|&slice| {
                let mut compressor = zstd::bulk::Compressor::new(level)?;
                compressor.compress(slice)
            })
            .collect();

        for frame_result in compressed {
            let frame = frame_result.map_err(Error::Io)?;
            let len_u32 = u32::try_from(frame.len())
                .map_err(|_| Error::Corrupt("compressed frame length overflow"))?;
            file.write_all(&frame)?;
            self.frame_lengths.push(len_u32);
        }

        Ok(())
    }

    fn do_finish(&mut self) -> Result<u64> {
        if self.finished {
            return Ok(0);
        }
        self.finished = true;

        match self.version {
            1 => {
                let mut file = self
                    .file
                    .take()
                    .ok_or_else(|| Error::Io(std::io::Error::other("database file not open")))?;
                let raw = std::mem::take(&mut self.raw);
                // Single-threaded bulk compression with the exact source size
                // known up front. See `flush_chunk` for the rationale.
                let mut compressor = zstd::bulk::Compressor::new(self.level)?;
                let compressed = compressor.compress(&raw)?;
                file.write_all(&compressed)?;

                file.flush()?;
                let size = file.stream_position()?;

                if let Some(dir) = self.path.parent()
                    && !dir.as_os_str().is_empty()
                {
                    self.basename_index.write_sidecars(dir)?;
                    write_attrs_sidecar(dir, &self.attrs)?;
                    self.path_index.write_sidecars(dir)?;
                    if let Some(redb) = self.redb.take() {
                        redb.finish()
                            .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))?;
                    }
                }

                return Ok(size);
            }
            2 => {
                self.flush_chunk()?;
            }
            _ => return Err(Error::Corrupt("unsupported database version")),
        }

        let mut file = self
            .file
            .take()
            .ok_or_else(|| Error::Io(std::io::Error::other("database file not open")))?;

        let frame_count = u32::try_from(self.frame_lengths.len())
            .map_err(|_| Error::Corrupt("frame count overflow"))?;
        let payload_len = 4usize
            .checked_add(
                self.frame_lengths
                    .len()
                    .checked_mul(4)
                    .ok_or(Error::Corrupt("seek table length overflow"))?,
            )
            .and_then(|len| len.checked_add(4))
            .ok_or(Error::Corrupt("seek table length overflow"))?;
        let payload_len_u32 =
            u32::try_from(payload_len).map_err(|_| Error::Corrupt("seek table length overflow"))?;

        file.write_all(&SKIPPABLE_MAGIC.to_le_bytes())?;
        file.write_all(&payload_len_u32.to_le_bytes())?;
        file.write_all(&frame_count.to_le_bytes())?;
        for len in &self.frame_lengths {
            file.write_all(&len.to_le_bytes())?;
        }
        file.write_all(&payload_len_u32.to_le_bytes())?;

        file.flush()?;
        let size = file.stream_position()?;

        if let Some(dir) = self.path.parent()
            && !dir.as_os_str().is_empty()
        {
            write_frame_map(dir, &self.frame_map, self.frame_lengths.len())?;
            self.basename_index.write_sidecars(dir)?;
            write_attrs_sidecar(dir, &self.attrs)?;
            self.path_index.write_sidecars(dir)?;
            if let Some(redb) = self.redb.take() {
                redb.finish()
                    .map_err(|err| Error::Io(std::io::Error::other(err.to_string())))?;
            }
        }

        Ok(size)
    }

    /// Finish writing the NIXI stream and basename sidecars; return compressed size.
    ///
    /// Sidecars are written next to the `files` path when a parent directory exists.
    ///
    /// # Errors
    ///
    /// Returns an I/O or secondary-index error if finalization fails.
    pub fn finish(mut self) -> Result<u64> {
        self.do_finish()
    }
}

/// Split `raw` frcode data into contiguous frame ranges, grouped at package
/// boundaries and targeting one frame per available CPU.
fn frame_ranges(
    raw: &[u8],
    boundaries: &[usize],
    parallelism: usize,
) -> Result<Vec<(usize, usize)>> {
    if raw.is_empty() || boundaries.is_empty() {
        return Ok(Vec::new());
    }

    let target = (raw.len() / parallelism).max(1);
    let mut ranges = Vec::new();
    let mut start = 0usize;

    for &end in boundaries {
        // Each range must end at a package boundary. Once we reach the target
        // size, cut the frame and start the next one.
        let len = end
            .checked_sub(start)
            .ok_or(Error::Corrupt("package boundary out of order"))?;
        if len >= target {
            ranges.push((start, end));
            start = end;
        }
    }

    if start < raw.len() {
        ranges.push((start, raw.len()));
    }

    Ok(ranges)
}

/// Reader that opens an existing NIXI database file.
#[derive(Debug)]
pub struct Reader {
    path: PathBuf,
    version: u64,
    /// Raw file bytes, including the header (mmapped).
    data: mmap_guard::FileData,
    /// Each frame is a `(offset, length)` slice into `data` that holds one
    /// compressed zstd frame. Frames are decompressed lazily during search.
    frames: Vec<(usize, usize)>,
    /// Optional frame map: package ordinal → frame index.
    frame_map: Option<Vec<u32>>,
    /// For each frame, the first package ordinal in that frame.
    frame_starts: Option<Vec<u32>>,
    /// Optional redb reader for fast exact-path lookups.
    redb: once_cell::sync::OnceCell<redb_index::Reader>,
    /// Optional path-level trigram index for fast literal substring queries.
    path_trigram: once_cell::sync::OnceCell<crate::path_trigram_index::PathTrigramIndex>,
    /// Optional per-path entry cache for fast result materialisation.
    path_entry: once_cell::sync::OnceCell<crate::path_entry_index::PathEntryIndex>,
    /// Optional basename-based entry cache for fast `nixdex locate -w` queries.
    entry_index: once_cell::sync::OnceCell<crate::entry_index::EntryIndex>,
    /// Optional trigram (n-gram) inverted index for fast literal substring candidate pruning.
    ngram: once_cell::sync::OnceCell<crate::ngram_index::NgramIndex>,
    /// Optional basename index for exact-basename candidate resolution.
    basename: once_cell::sync::OnceCell<crate::basename_index::BasenameIndex>,
    /// Optional path index for exact/prefix path candidate resolution.
    path_index: once_cell::sync::OnceCell<crate::path_index::PathIndex>,
    /// Cached decompressed frcode data for version 1 databases. Version 1 uses
    /// a single large zstd frame; caching the decoded bytes lets repeated
    /// searches (in particular the daemon) avoid decompressing it every query.
    v1_decompressed: once_cell::sync::OnceCell<Vec<u8>>,
}

impl Reader {
    /// Maximum number of path-level trigram candidates we will materialise
    /// before falling back to a full frcode scan. Keeps very common trigrams
    /// (e.g. "bin") from decoding thousands of paths.
    const PATH_TRIGRAM_CANDIDATE_LIMIT: u64 = 1000;

    /// Opens a nix-index / nixdex database located at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the path does not exist or is not a valid database.
    #[allow(clippy::cognitive_complexity)]
    #[allow(clippy::too_many_lines)]
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();

        let metadata = fs::metadata(&path_buf)?;
        if metadata.len() > MAX_DATABASE_BYTES {
            return Err(Error::Corrupt("database file exceeds maximum size"));
        }

        let data = mmap_guard::map_file(&path_buf)?;

        if data.len() < DATA_START {
            return Err(Error::Corrupt("database file too short for header"));
        }

        let magic = data
            .get(..FILE_MAGIC.len())
            .ok_or(Error::Corrupt("database header magic missing"))?;
        if magic != FILE_MAGIC {
            return Err(Error::UnsupportedFileType {
                found: magic.to_vec(),
            });
        }

        let version = u64::from_le_bytes(
            data.get(4..DATA_START)
                .ok_or(Error::Corrupt("database header truncated"))?
                .try_into()
                .map_err(|_| Error::Corrupt("database header length"))?,
        );
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(Error::UnsupportedVersion { found: version });
        }

        let frames = match version {
            1 => {
                let len = data.len() - DATA_START;
                if len == 0 {
                    Vec::new()
                } else {
                    vec![(DATA_START, len)]
                }
            }
            2 => parse_seek_table(&data, DATA_START)?,
            _ => return Err(Error::Corrupt("unsupported database version")),
        };

        // Try to read frame_map sidecar for selective decompression.
        let (frame_map, frame_starts) = if let Some(dir) = path_buf.parent()
            && !dir.as_os_str().is_empty()
        {
            match read_frame_map(dir) {
                Ok(fm) => {
                    let fs = compute_frame_starts(&fm, frames.len());
                    (Some(fm), Some(fs))
                }
                Err(err) => {
                    let is_missing = matches!(
                        &err,
                        Error::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound
                    );
                    if !is_missing {
                        tracing::warn!(%err, "frame_map sidecar unreadable; falling back to full scan");
                    }
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        // Try to open redb index for fast exact-path lookups.
        let mut redb_cell = once_cell::sync::OnceCell::new();
        if let Some(dir) = path_buf.parent()
            && !dir.as_os_str().is_empty()
        {
            match redb_index::Reader::open(dir) {
                Ok(r) => _ = redb_cell.set(r),
                Err(err) => {
                    tracing::debug!(%err, "redb index unavailable; falling back to scan");
                }
            }
        }

        Ok(Self {
            path: path_buf,
            version,
            data,
            frames,
            frame_map,
            frame_starts,
            redb: redb_cell,
            path_trigram: once_cell::sync::OnceCell::new(),
            path_entry: once_cell::sync::OnceCell::new(),
            entry_index: once_cell::sync::OnceCell::new(),
            ngram: once_cell::sync::OnceCell::new(),
            basename: once_cell::sync::OnceCell::new(),
            path_index: once_cell::sync::OnceCell::new(),
            v1_decompressed: once_cell::sync::OnceCell::new(),
        })
    }
    /// Return the path this reader was opened against.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the on-disk format version of the opened database.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Return the number of packages in the database, if known from the on-disk
    /// frame map. Returns `None` for databases without a frame map.
    #[must_use]
    pub fn package_count(&self) -> Option<usize> {
        self.frame_map.as_ref().map(std::vec::Vec::len)
    }

    /// Lazily decompress the version 1 frcode stream once and cache the result.
    ///
    /// This is shared across repeated searches so long-running processes (the
    /// daemon, or multiple CLI queries against the same `Reader`) do not pay
    /// the zstd decompression cost on every query.
    fn v1_decompressed(&self) -> Result<&[u8]> {
        self.v1_decompressed
            .get_or_try_init(|| {
                let compressed = self
                    .data
                    .get(DATA_START..)
                    .ok_or(Error::Corrupt("v1 frame slice out of bounds"))?;
                decompress_frame_threaded(compressed)
            })
            .map(std::vec::Vec::as_slice)
    }

    /// Pre-decompress and cache the version 1 frcode stream.
    ///
    /// Has no effect on version 2 databases. Returns immediately if the cache is
    /// already populated.
    pub fn prefetch_v1(&self) -> Result<()> {
        if self.version == 1 {
            self.v1_decompressed()?;
        }
        Ok(())
    }

    fn open_sidecar<'a, T, F>(
        cell: &'a once_cell::sync::OnceCell<T>,
        dir: Option<&'a Path>,
        open: F,
    ) -> Option<&'a T>
    where
        F: FnOnce(&Path) -> Result<T>,
    {
        let dir = match dir {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => return None,
        };
        match cell.get_or_try_init(|| open(dir)) {
            Ok(index) => Some(index),
            Err(err) => {
                tracing::debug!(%err, "sidecar unavailable; falling back to scan");
                None
            }
        }
    }

    /// Returns the redb reader if available, loading it lazily on first access.
    #[must_use]
    pub fn redb(&self) -> Option<&redb_index::Reader> {
        self.redb.get()
    }

    /// Returns the path-level trigram index if available, loading it lazily.
    #[must_use]
    pub fn path_trigram(&self) -> Option<&crate::path_trigram_index::PathTrigramIndex> {
        Self::open_sidecar(&self.path_trigram, self.path.parent(), |dir| {
            crate::path_trigram_index::PathTrigramIndex::open(dir)
                .map_err(Error::PathTrigramIndex)
        })
    }

    /// Returns the per-path entry cache if available, loading it lazily.
    #[must_use]
    pub fn path_entry(&self) -> Option<&crate::path_entry_index::PathEntryIndex> {
        Self::open_sidecar(&self.path_entry, self.path.parent(), |dir| {
            crate::path_entry_index::PathEntryIndex::open(dir).map_err(Error::PathEntryIndex)
        })
    }

    /// Returns the basename-based entry index if available, loading it lazily.
    #[must_use]
    pub fn entry_index(&self) -> Option<&crate::entry_index::EntryIndex> {
        Self::open_sidecar(&self.entry_index, self.path.parent(), |dir| {
            crate::entry_index::EntryIndex::open(dir).map_err(Error::EntryIndex)
        })
    }

    /// Returns the ngram inverted index if available, loading it lazily.
    #[must_use]
    pub fn ngram(&self) -> Option<&crate::ngram_index::NgramIndex> {
        Self::open_sidecar(&self.ngram, self.path.parent(), |dir| {
            crate::ngram_index::NgramIndex::open(dir).map_err(Error::NgramIndex)
        })
    }

    /// Returns the basename index if available, loading it lazily.
    #[must_use]
    pub fn basename(&self) -> Option<&crate::basename_index::BasenameIndex> {
        Self::open_sidecar(&self.basename, self.path.parent(), |dir| {
            crate::basename_index::BasenameIndex::open(dir).map_err(Error::SecondaryIndex)
        })
    }

    /// Returns the path index if available, loading it lazily.
    #[must_use]
    pub fn path_index(&self) -> Option<&crate::path_index::PathIndex> {
        Self::open_sidecar(&self.path_index, self.path.parent(), |dir| {
            crate::path_index::PathIndex::open(dir).map_err(Error::PathIndex)
        })
    }

    /// Scan every frame in parallel, yielding `(StorePath, FileTreeEntry)` matches.
    ///
    /// Each frame is a complete frcode package stream, so decompression and
    /// decoding can be parallelized.
    ///
    /// # Errors
    ///
    /// Returns an error if a frame is corrupt or I/O fails.
    #[allow(clippy::too_many_lines)]
    pub fn search_entries(
        &self,
        path_pattern: &Regex,
        package_pattern: Option<&Regex>,
        hash: Option<&str>,
        package_labels: Option<&IndexSet<String>>,
        package_ordinals: Option<&RoaringBitmap>,
    ) -> Result<Vec<(StorePath, FileTreeEntry)>> {
        // Selective frame decompression: if we have a frame_map and candidate ordinals,
        // only decompress frames that contain at least one candidate ordinal.
        // Ordinals are only meaningful when we also know the ordinal at the start
        // of each frame, so disable the filter if the frame map is missing.
        let filter_ordinals: Option<&RoaringBitmap> = if self.frame_starts.is_some() {
            package_ordinals
        } else {
            None
        };

        let frames_to_scan: Vec<(usize, usize, Option<u32>)> =
            if let (Some(frame_map), Some(ordinals), Some(frame_starts)) =
                (&self.frame_map, filter_ordinals, &self.frame_starts)
            {
                // Collect the set of frame indices that contain any candidate ordinal.
                let mut needed_frames = RoaringBitmap::new();
                for ord in ordinals {
                    let idx = usize::try_from(ord)
                        .map_err(|_| Error::Corrupt("package ordinal overflow"))?;
                    if let Some(&frame_idx) = frame_map.get(idx) {
                        needed_frames.insert(frame_idx);
                    }
                }

                // Map frame indices to (offset, len, frame_start_ordinal).
                self.frames
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (offset, len))| {
                        let i_u32 = u32::try_from(i).ok()?;
                        if needed_frames.contains(i_u32) {
                            let start_ord = frame_starts.get(i).copied();
                            Some((*offset, *len, start_ord))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                // No frame_map or no ordinals: scan all frames.
                self.frames
                    .iter()
                    .map(|(offset, len)| (*offset, *len, None))
                    .collect()
            };

        // NIXI v1 databases are a single large zstd frame. Long-running holders
        // of the `Reader` (the daemon) can pre-cache the decoded frcode bytes; if
        // the cache is warm, scan it directly. Otherwise stream-decode the frame.
        if self.version == 1 {
            let matchers = FrameMatchers::new(path_pattern.as_str())?;
            if let Some(raw) = self.v1_decompressed.get() {
                let frame_start_ordinal =
                    if let Some(v) = self.frame_starts.as_ref().and_then(|v| v.first().copied()) {
                        v
                    } else {
                        0
                    };
                return search_frame_raw(
                    raw.as_slice(),
                    &matchers,
                    path_pattern,
                    package_pattern,
                    hash,
                    package_labels,
                    Some(frame_start_ordinal),
                    filter_ordinals,
                );
            }
            if let Some((offset, len, frame_start_ordinal)) = frames_to_scan.first() {
                let start = *offset;
                let end = start + *len;
                let compressed = self
                    .data
                    .get(start..end)
                    .ok_or(Error::Corrupt("frame slice out of range"))?;
                return search_frame_stream(
                    compressed,
                    &matchers,
                    path_pattern,
                    package_pattern,
                    hash,
                    package_labels,
                    *frame_start_ordinal,
                    filter_ordinals,
                );
            }
            return Ok(Vec::new());
        }

        let matchers = FrameMatchers::new(path_pattern.as_str())?;
        let per_frame: Vec<Vec<(StorePath, FileTreeEntry)>> = frames_to_scan
            .par_iter()
            .map(|(offset, len, frame_start_ordinal)| {
                let start = *offset;
                let end = start + *len;
                let compressed = self
                    .data
                    .get(start..end)
                    .ok_or(Error::Corrupt("frame slice out of range"))?;
                search_frame(
                    compressed,
                    &matchers,
                    path_pattern,
                    package_pattern,
                    hash,
                    package_labels,
                    *frame_start_ordinal,
                    filter_ordinals,
                )
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut matches = Vec::new();
        for frame_matches in per_frame {
            matches.extend(frame_matches);
        }
        Ok(matches)
    }

    /// Fast literal-substring search using the path-level trigram + entry sidecars.
    ///
    /// Returns `Ok(None)` if the sidecars are missing, the pattern is not a
    /// literal substring, or the trigram candidate set is empty. Otherwise returns
    /// `Ok(Some(results))` with all matching `(StorePath, FileTreeEntry)` pairs,
    /// pre-filtered by `package_pattern`, `hash`, and `should_include_match`.
    #[allow(clippy::cognitive_complexity)]
    pub fn search_path_trigram(
        &self,
        pattern: &str,
        path_pattern: &Regex,
        package_pattern: Option<&Regex>,
        hash: Option<&str>,
        options: &SearchOptions<'_>,
    ) -> Result<Option<Vec<(StorePath, FileTreeEntry)>>> {
        let Some(trigram) = self.path_trigram.get() else {
            return Ok(None);
        };
        let Some(entry) = self.path_entry.get() else {
            return Ok(None);
        };

        if pattern.len() < 3 {
            return self.search_short_literal(
                pattern,
                path_pattern,
                package_pattern,
                hash,
                options,
            );
        }

        let candidates = match trigram.candidate_path_ids(pattern) {
            Ok(Some(ids)) => ids,
            Ok(None) => return Ok(None),
            Err(err) => {
                tracing::debug!(%err, "path trigram candidate lookup failed; falling back");
                return Ok(None);
            }
        };


        if candidates.is_empty() {
            return Ok(Some(Vec::new()));
        }

        if candidates.len() > Self::PATH_TRIGRAM_CANDIDATE_LIMIT {
            tracing::debug!(
                count = candidates.len(),
                "too many path trigram candidates; falling back to scan"
            );
            return Ok(None);
        }

        let needle = pattern.as_bytes();
        let mut results = Vec::new();
        for path_id in candidates {
            let path = entry.path_bytes(path_id).map_err(Error::PathEntryIndex)?;
            if memchr::memmem::find(path, needle).is_none() {
                continue;
            }

            let hits = entry
                .lookup_entries_by_id(path_id)
                .map_err(Error::PathEntryIndex)?;
            for (store_path, entry) in hits {
                if package_pattern.is_some_and(|re| !re.is_match(store_path.name().as_bytes())) {
                    continue;
                }
                if hash.is_some_and(|h| h != store_path.hash()) {
                    continue;
                }
                if should_include_match(options, path_pattern, &store_path, &entry) {
                    results.push((store_path, entry));
                }
            }
        }

        Ok(Some(results))
    }

    /// Fast literal-substring search for short patterns (< 3 chars) by scanning
    /// all paths in the entry index.
    ///
    /// Trigram indexes require at least 3 characters, so short patterns fall
    /// through to a full frcode scan. This method uses the resident path-entry
    /// sidecar to avoid decoding the entire database.
    ///
    /// For basename-only queries (no `/`), the basename FST is checked first
    /// so non-existent commands return near-instantly.
    ///
    /// Returns `Ok(None)` if the sidecar is missing. Otherwise returns
    /// `Ok(Some(results))` with all matching `(StorePath, FileTreeEntry)` pairs,
    /// pre-filtered by `package_pattern`, `hash`, and `should_include_match`.
    pub fn search_short_literal(
        &self,
        pattern: &str,
        path_pattern: &Regex,
        package_pattern: Option<&Regex>,
        hash: Option<&str>,
        options: &SearchOptions<'_>,
    ) -> Result<Option<Vec<(StorePath, FileTreeEntry)>>> {
        let Some(entry) = self.path_entry.get() else {
            return Ok(None);
        };

        let needle = pattern.as_bytes();

        // For basename-only queries, check the basename FST first so
        // non-existent commands return near-instantly.
        if !needle.contains(&b'/') {
            if let Some(basename_index) = self.basename.get() {
                if let Ok(ords) = basename_index.lookup_basename_ordinals(needle) {
                    if !ords.is_empty() {
                        // TODO: when entry index supports id-limited scan, use ords
                        // to restrict the scan to matching basenames only.
                    }
                }
            }
        }

        let mut results = Vec::new();
        let path_count = entry.path_count();

        let limit = options.limit.unwrap_or(usize::MAX);
        for path_id in 0..path_count {
            if results.len() >= limit {
                break;
            }

            let path = match entry.path_bytes(path_id as u32) {
                Ok(p) => p,
                Err(_) => continue,
            };

            if memchr::memmem::find(path, needle).is_none() {
                continue;
            }

            let hits = match entry.lookup_entries_by_id(path_id as u32) {
                Ok(h) => h,
                Err(_) => continue,
            };

            for (store_path, entry) in hits {
                if results.len() >= limit {
                    break;
                }
                if package_pattern.is_some_and(|re| !re.is_match(store_path.name().as_bytes())) {
                    continue;
                }
                if hash.is_some_and(|h| h != store_path.hash()) {
                    continue;
                }
                if should_include_match(options, path_pattern, &store_path, &entry) {
                    results.push((store_path, entry));
                }
            }
        }

        Ok(Some(results))
    }

    /// Fast exact-basename search using the basename-based entry sidecar.
    ///
    /// Returns `Ok(None)` if the sidecar is missing. Otherwise returns
    /// `Some(results)` filtered by package/hash/options.
    pub fn search_entry_index(
        &self,
        basename: &[u8],
        path_pattern: &Regex,
        package_pattern: Option<&Regex>,
        hash: Option<&str>,
        options: &SearchOptions<'_>,
    ) -> Result<Option<Vec<(StorePath, FileTreeEntry)>>> {
        let Some(index) = self.entry_index.get() else {
            return Ok(None);
        };

        let mut results = Vec::new();
        for (store_path, entry) in index.lookup_entries(basename).map_err(Error::EntryIndex)? {
            if package_pattern.is_some_and(|re| !re.is_match(store_path.name().as_bytes())) {
                continue;
            }
            if hash.is_some_and(|h| h != store_path.hash()) {
                continue;
            }
            if should_include_match(options, path_pattern, &store_path, &entry) {
                results.push((store_path, entry));
            }
        }

        Ok(Some(results))
    }

    /// Exact-basename lookup via the optional FST secondary index.
    ///
    /// `pattern` is treated as a **basename** (final path component), not a full
    /// path or regex. Returns package labels (`attr.output`) that contain a file
    /// with that basename.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SecondaryIndex`] when sidecars are missing or corrupt.
    pub fn query_fst(&self, pattern: &str) -> Result<Vec<String>> {
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| {
                Error::SecondaryIndex(crate::basename_index::Error::Missing {
                    dir: self.path.clone(),
                    detail: "database path has no parent directory for sidecars".into(),
                })
            })?;
        let index = BasenameIndex::open(dir)?;
        // Convert borrowed strings to owned for the public API.
        Ok(index
            .lookup_basename(pattern.as_bytes())?
            .into_iter()
            .map(std::string::ToString::to_string)
            .collect())
    }

    /// Full-path lookup via the optional path secondary index.
    ///
    /// `path` is treated as a **full path** (e.g., `/bin/ls`). Returns package
    /// ordinals that contain a file with that exact full path.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PathIndex`] when sidecars are missing or corrupt.
    pub fn query_path_ordinals(&self, path: &[u8]) -> Result<Vec<u32>> {
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| {
                Error::PathIndex(crate::path_index::Error::Missing {
                    dir: self.path.clone(),
                    detail: "database path has no parent directory for sidecars".into(),
                })
            })?;
        let index = PathIndex::open(dir)?;
        Ok(index.lookup_path_ordinals(path)?)
    }

    /// Prefix lookup via the optional path secondary index.
    ///
    /// `prefix` is treated as a path prefix (e.g., `/bin/`). Returns package
    /// ordinals that contain any file whose path starts with this prefix.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PathIndex`] when sidecars are missing or corrupt.
    pub fn query_prefix_ordinals(&self, prefix: &[u8]) -> Result<Vec<u32>> {
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| {
                Error::PathIndex(crate::path_index::Error::Missing {
                    dir: self.path.clone(),
                    detail: "database path has no parent directory for sidecars".into(),
                })
            })?;
        let index = PathIndex::open(dir)?;
        Ok(index.lookup_prefix_ordinals(prefix)?)
    }
}

fn parse_seek_table(data: &[u8], data_start: usize) -> Result<Vec<(usize, usize)>> {
    if data.len() < data_start + 8 {
        return Err(Error::Corrupt("v2 database too short for seek table"));
    }

    let payload_len = usize::try_from(read_u32_le(data, data.len() - 4)?)
        .map_err(|_| Error::Corrupt("v2 seek table payload length overflow"))?;
    let skippable_size = 8usize
        .checked_add(payload_len)
        .ok_or(Error::Corrupt("v2 seek table size overflow"))?;
    let min_file_size = DATA_START
        .checked_add(skippable_size)
        .ok_or(Error::Corrupt("v2 seek table size overflow"))?;
    if data.len() < min_file_size {
        return Err(Error::Corrupt("v2 seek table truncated"));
    }

    let skippable_end = data.len();
    let skippable_start = skippable_end - skippable_size;
    let magic = read_u32_le(data, skippable_start)?;
    if magic != SKIPPABLE_MAGIC {
        return Err(Error::Corrupt("v2 trailing magic mismatch"));
    }

    let declared_size = usize::try_from(read_u32_le(data, skippable_start + 4)?)
        .map_err(|_| Error::Corrupt("v2 seek table declared size overflow"))?;
    if declared_size != payload_len {
        return Err(Error::Corrupt("v2 seek table size mismatch"));
    }

    let payload_start = skippable_start + 8;
    let payload = data
        .get(payload_start..payload_start + payload_len)
        .ok_or(Error::Corrupt("v2 seek table payload overflow"))?;

    if payload.len() < 4 {
        return Err(Error::Corrupt("v2 seek table payload too short"));
    }
    let frame_count = usize::try_from(read_u32_le(payload, 0)?)
        .map_err(|_| Error::Corrupt("v2 frame count overflow"))?;
    if frame_count > MAX_FRAME_COUNT {
        return Err(Error::Corrupt("v2 frame count too high"));
    }
    let expected_payload_len = 4usize
        .checked_add(
            frame_count
                .checked_mul(4)
                .ok_or(Error::Corrupt("v2 seek table length overflow"))?,
        )
        .and_then(|len| len.checked_add(4))
        .ok_or(Error::Corrupt("v2 seek table length overflow"))?;
    if expected_payload_len != payload_len {
        return Err(Error::Corrupt("v2 seek table length mismatch"));
    }

    let mut lens = Vec::with_capacity(frame_count);
    for i in 0..frame_count {
        lens.push(
            usize::try_from(read_u32_le(payload, 4 + i * 4)?)
                .map_err(|_| Error::Corrupt("v2 frame length overflow"))?,
        );
    }
    let trailing = usize::try_from(read_u32_le(payload, payload_len - 4)?)
        .map_err(|_| Error::Corrupt("v2 trailing payload length overflow"))?;
    if trailing != payload_len {
        return Err(Error::Corrupt("v2 trailing payload length mismatch"));
    }

    let frames_end = skippable_start;
    let total_compressed = lens.iter().try_fold(0usize, |acc, &len| {
        acc.checked_add(len)
            .ok_or(Error::Corrupt("v2 compressed length overflow"))
    })?;
    let frames_len = frames_end
        .checked_sub(data_start)
        .ok_or(Error::Corrupt("v2 compressed frames underflow"))?;
    if total_compressed != frames_len {
        return Err(Error::Corrupt("v2 compressed length mismatch"));
    }

    let mut frames = Vec::with_capacity(frame_count);
    let mut offset = data_start;
    for len in lens {
        let next = offset
            .checked_add(len)
            .ok_or(Error::Corrupt("v2 frame offset overflow"))?;
        if next > data.len() {
            return Err(Error::Corrupt("v2 frame slice overflow"));
        }
        frames.push((offset, len));
        offset = next;
    }

    Ok(frames)
}

/// Write the frame_map sidecar for selective decompression.
fn write_frame_map(db_dir: &Path, frame_map: &[u32], frame_count: usize) -> Result<()> {
    let path = db_dir.join(FRAME_MAP_FILE);
    let mut file = File::create(&path)?;

    let package_count = frame_map.len();
    file.write_all(FRAME_MAP_MAGIC)?;
    file.write_u32::<LittleEndian>(FRAME_MAP_VERSION)?;
    let package_count_u32 =
        u32::try_from(package_count).map_err(|_| Error::Corrupt("package count overflow"))?;
    let frame_count_u32 =
        u32::try_from(frame_count).map_err(|_| Error::Corrupt("frame count overflow"))?;
    file.write_u32::<LittleEndian>(package_count_u32)?;
    file.write_u32::<LittleEndian>(frame_count_u32)?;
    for &frame_idx in frame_map {
        if usize::try_from(frame_idx).map_err(|_| Error::Corrupt("frame index overflow"))?
            >= frame_count
        {
            return Err(Error::Corrupt("frame index out of range"));
        }
        file.write_u32::<LittleEndian>(frame_idx)?;
    }

    file.flush()?;
    Ok(())
}

/// Read the frame_map sidecar for selective decompression.
fn read_frame_map(db_dir: &Path) -> Result<Vec<u32>> {
    let path = db_dir.join(FRAME_MAP_FILE);
    let bytes = std::fs::read(&path)?;

    if bytes.len() < FRAME_MAP_MAGIC.len() + 12 {
        return Err(Error::Corrupt("frame_map too short"));
    }

    let magic = bytes
        .get(..FRAME_MAP_MAGIC.len())
        .ok_or(Error::Corrupt("frame_map magic missing"))?;
    if magic != FRAME_MAP_MAGIC {
        return Err(Error::Corrupt("frame_map magic mismatch"));
    }

    let version = read_u32_le(&bytes, FRAME_MAP_MAGIC.len())?;
    if version != FRAME_MAP_VERSION {
        return Err(Error::Corrupt("frame_map version mismatch"));
    }

    let package_count = usize::try_from(read_u32_le(&bytes, FRAME_MAP_MAGIC.len() + 4)?)
        .map_err(|_| Error::Corrupt("package count overflow"))?;
    let frame_count = usize::try_from(read_u32_le(&bytes, FRAME_MAP_MAGIC.len() + 8)?)
        .map_err(|_| Error::Corrupt("frame count overflow"))?;

    let expected_len = FRAME_MAP_MAGIC.len() + 12 + package_count * 4;
    if bytes.len() != expected_len {
        return Err(Error::Corrupt("frame_map length mismatch"));
    }

    let mut ordinal_to_frame = Vec::with_capacity(package_count);
    for i in 0..package_count {
        let offset = FRAME_MAP_MAGIC.len() + 12 + i * 4;
        let frame_idx = read_u32_le(&bytes, offset)?;
        if usize::try_from(frame_idx).map_err(|_| Error::Corrupt("frame index overflow"))?
            >= frame_count
        {
            return Err(Error::Corrupt("frame index out of range"));
        }
        ordinal_to_frame.push(frame_idx);
    }

    Ok(ordinal_to_frame)
}

/// Write the attrs sidecar for incremental builds.
fn write_attrs_sidecar(db_dir: &Path, attrs: &[(String, String, String)]) -> Result<()> {
    let path = db_dir.join(ATTRS_FILE);
    let mut file = File::create(&path)?;

    let package_count = attrs.len();
    file.write_all(ATTRS_MAGIC)?;
    file.write_u32::<LittleEndian>(ATTRS_VERSION)?;
    let package_count_u32 =
        u32::try_from(package_count).map_err(|_| Error::Corrupt("package count overflow"))?;
    file.write_u32::<LittleEndian>(package_count_u32)?;

    for (attr, output, hash) in attrs {
        write_length_prefixed_string(&mut file, attr)?;
        write_length_prefixed_string(&mut file, output)?;
        write_length_prefixed_string(&mut file, hash)?;
    }

    file.flush()?;
    Ok(())
}

/// Read the attrs sidecar for incremental builds.
///
/// Returns `Ok(None)` if the file is missing or has an invalid magic/version.
pub fn read_attrs_sidecar(db_dir: &Path) -> Result<Option<Vec<(String, String, String)>>> {
    let path = db_dir.join(ATTRS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    // Reject oversized sidecar files immediately.
    if bytes.len() > MAX_ATTRS_BYTES {
        return Err(Error::Corrupt("attrs sidecar exceeds maximum size"));
    }

    if bytes.len() < ATTRS_MAGIC.len() + 8 {
        return Ok(None);
    }

    let magic = bytes
        .get(..ATTRS_MAGIC.len())
        .ok_or(Error::Corrupt("attrs magic missing"))?;
    if magic != ATTRS_MAGIC {
        return Ok(None);
    }

    let version = read_u32_le(&bytes, ATTRS_MAGIC.len())?;
    if version != ATTRS_VERSION {
        return Ok(None);
    }

    let package_count = usize::try_from(read_u32_le(&bytes, ATTRS_MAGIC.len() + 4)?)
        .map_err(|_| Error::Corrupt("package count overflow"))?;

    // Validate package_count against remaining bytes (minimum 12 bytes per record: 3 * 4-byte length headers).
    let remaining_bytes = bytes.len().saturating_sub(ATTRS_MAGIC.len() + 8);
    let min_bytes_per_record = 12; // 3 length-prefixed strings, each with 4-byte header
    if package_count > 0 && remaining_bytes / package_count < min_bytes_per_record {
        return Err(Error::Corrupt(
            "attrs sidecar package count exceeds remaining bytes",
        ));
    }

    let mut attrs = Vec::with_capacity(package_count);
    let mut offset = ATTRS_MAGIC.len() + 8;

    for _ in 0..package_count {
        let (attr, new_offset) = read_length_prefixed_string(&bytes, offset)?;
        offset = new_offset;
        let (output, new_offset) = read_length_prefixed_string(&bytes, offset)?;
        offset = new_offset;
        let (hash, new_offset) = read_length_prefixed_string(&bytes, offset)?;
        offset = new_offset;
        attrs.push((attr, output, hash));
    }

    if offset != bytes.len() {
        return Err(Error::Corrupt("attrs sidecar has trailing data"));
    }

    Ok(Some(attrs))
}

/// Write a length-prefixed string (u32 LE length + UTF-8 bytes).
fn write_length_prefixed_string<W: Write>(writer: &mut W, s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    let len = u32::try_from(bytes.len()).map_err(|_| Error::Corrupt("string length overflow"))?;
    writer.write_u32::<LittleEndian>(len)?;
    writer.write_all(bytes)?;
    Ok(())
}

/// Read a length-prefixed string from bytes at the given offset.
///
/// Returns `(string, new_offset)`.
fn read_length_prefixed_string(bytes: &[u8], offset: usize) -> Result<(String, usize)> {
    let len = usize::try_from(read_u32_le(bytes, offset)?)
        .map_err(|_| Error::Corrupt("string length overflow"))?;
    let string_start = offset + 4;
    let string_end = string_start
        .checked_add(len)
        .ok_or(Error::Corrupt("string end overflow"))?;
    let string_bytes = bytes
        .get(string_start..string_end)
        .ok_or(Error::Corrupt("string slice out of range"))?;
    let string = std::str::from_utf8(string_bytes)
        .map_err(|_| Error::Corrupt("string not valid UTF-8"))?
        .to_string();
    Ok((string, string_end))
}

/// Compute frame_starts: for each frame, the first package ordinal in that frame.
fn compute_frame_starts(frame_map: &[u32], frame_count: usize) -> Vec<u32> {
    let mut frame_starts = vec![u32::MAX; frame_count];
    for (ord, &frame_idx) in frame_map.iter().enumerate() {
        let Ok(frame_idx_usize) = usize::try_from(frame_idx) else {
            continue;
        };
        if frame_idx_usize < frame_count {
            let Ok(ord_u32) = u32::try_from(ord) else {
                continue;
            };
            if let Some(slot) = frame_starts.get_mut(frame_idx_usize)
                && (*slot == u32::MAX || ord_u32 < *slot)
            {
                *slot = ord_u32;
            }
        }
    }
    frame_starts
}

fn read_u32_le(bytes: &[u8], at: usize) -> Result<u32> {
    let end = at
        .checked_add(4)
        .ok_or(Error::Corrupt("u32 offset overflow"))?;
    let slice = bytes
        .get(at..end)
        .ok_or(Error::Corrupt("u32 read past end"))?;
    let arr: [u8; 4] = slice
        .try_into()
        .map_err(|_| Error::Corrupt("u32 slice length"))?;
    Ok(u32::from_le_bytes(arr))
}

fn sidecars_exist(dir: &Path) -> bool {
    [FST_FILE, POSTINGS_FILE, NAMES_FILE]
        .iter()
        .any(|name| dir.join(name).is_file())
}

/// Error consumer for ripgrep's `NoError` matcher, which is never constructed
/// at runtime by the matchers used here.
#[allow(clippy::panic)]
fn consume_no_error<T>(e: NoError) -> T {
    let _ = e;
    unreachable!("NoError is never constructed by ripgrep matchers")
}

/// Find the next line in `buf` starting at or after `start` that matches `matcher`.
/// Returns the byte range `[line_start, line_end)` of the confirmed match.
fn next_matching_line<M: Matcher<Error = NoError>>(
    matcher: M,
    buf: &[u8],
    mut start: usize,
) -> Option<(usize, usize)> {
    while let Some(candidate) = matcher
        .find_candidate_line(buf.get(start..).map_or(&[], |s| s))
        .unwrap_or_else(consume_no_error)
    {
        if start == buf.len() {
            return None;
        }

        let (pos, confirmed) = match candidate {
            LineMatchKind::Confirmed(p) => (start + p, true),
            LineMatchKind::Candidate(p) => (start + p, false),
        };

        let line_start =
            memchr::memrchr(b'\n', buf.get(..pos).map_or(&[], |s| s)).map_or(0, |x| x + 1);
        let line_end = memchr::memchr(b'\n', buf.get(pos..).map_or(&[], |s| s))
            .map_or(buf.len(), |x| pos + x + 1);

        if !confirmed
            && !matcher
                .is_match(buf.get(line_start..line_end).map_or(&[], |s| s))
                .unwrap_or_else(consume_no_error)
        {
            start = line_end;
            continue;
        }

        return Some((line_start, line_end));
    }
    None
}

/// Rewrite the AST of a user regex so that every `^` line-start assertion is
/// replaced by a NUL byte, matching the start of the *path* portion of an frcode
/// line (`METADATA\0PATH`). Mirrors upstream `nix-index`'s query preparation.
fn path_anchored_expr(pattern: &str) -> String {
    let Ok(ast) = regex_syntax::ast::parse::Parser::new().parse(pattern) else {
        return pattern.to_string();
    };

    fn walk(node: &mut Ast) {
        match node {
            Ast::Assertion(a) if a.kind == AssertionKind::StartLine => {
                *node = Ast::Literal(Box::new(regex_syntax::ast::Literal {
                    span: a.span,
                    c: '\0',
                    kind: regex_syntax::ast::LiteralKind::Verbatim,
                }));
            }
            Ast::Group(g) => walk(&mut g.ast),
            Ast::Repetition(r) => walk(&mut r.ast),
            Ast::Concat(c) => {
                for child in &mut c.asts {
                    walk(child);
                }
            }
            Ast::Alternation(a) => {
                for child in &mut a.asts {
                    walk(child);
                }
            }
            _ => {}
        }
    }

    let mut ast = ast;
    walk(&mut ast);
    ast.to_string()
}

/// Matchers for a search frame. `path_matcher` uses `^`→`\0` so anchored
/// patterns match the start of the *path* portion of an frcode line.
/// `package_entry_pattern` matches only `p\0…` package-header lines.
struct FrameMatchers {
    path_matcher: grep::regex::RegexMatcher,
    package_entry_pattern: grep::regex::RegexMatcher,
}

impl FrameMatchers {
    fn new(path_pattern: &str) -> Result<Self> {
        let mut builder = RegexMatcherBuilder::new();
        builder.line_terminator(Some(b'\n')).multi_line(true);

        let path_matcher = builder
            .build(&path_anchored_expr(path_pattern))
            .map_err(|err| Error::Grep(err.to_string()))?;
        let package_entry_pattern = builder
            .build("^p\0")
            .map_err(|err| Error::Grep(err.to_string()))?;
        Ok(Self {
            path_matcher,
            package_entry_pattern,
        })
    }
}

/// Search a decoded frcode stream for entries matching the supplied patterns.
///
/// Mirrors upstream `nix-index`: `find_candidate_line` jumps to file-entry lines
/// whose path matches (with `^`→`\0`), and a separate `^p\0` matcher finds the
/// enclosing package header for each match. A `found_without_package` carry
/// buffer handles entries whose package header is split across a block boundary.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn search_frame_decoder<R: std::io::BufRead>(
    decoder: &mut frcode::Decoder<R>,
    matchers: &FrameMatchers,
    exact_path: &Regex,
    package_pattern: Option<&Regex>,
    hash: Option<&str>,
    package_labels: Option<&IndexSet<String>>,
    frame_start_ordinal: Option<u32>,
    package_ordinals: Option<&RoaringBitmap>,
) -> Result<Vec<(StorePath, FileTreeEntry)>> {
    let mut matches = Vec::new();
    let mut found_without_package: Vec<FileTreeEntry> = Vec::new();
    let mut current_ordinal = match frame_start_ordinal {
        Some(o) => o,
        None => 0,
    };

    loop {
        let block = decoder.decode()?;
        if block.is_empty() {
            break;
        }

        // Pre-index every `p\0` package footer in this decoded block.  Each
        // footer is annotated with its global package ordinal and the JSON
        // slice that describes the package.  This keeps `current_ordinal`
        // correct across block boundaries without depending on matched
        // entries appearing in every block.
        let mut package_footers: Vec<(usize, u32, usize, usize)> = Vec::new();
        let mut footer_pos = 0;
        while let Some((ls, le)) =
            next_matching_line(&matchers.package_entry_pattern, block, footer_pos)
        {
            footer_pos = le;
            let json_start = ls + 2;
            let json_end = le.saturating_sub(1);
            if json_start > json_end {
                continue;
            }
            package_footers.push((le, current_ordinal, json_start, json_end));
            current_ordinal = current_ordinal
                .checked_add(1)
                .ok_or(Error::Corrupt("package ordinal overflow"))?;
        }

        // Entries whose enclosing package footer was split into this block are
        // resolved against the first footer found here.
        if !found_without_package.is_empty() {
            if let Some((_, ord, json_start, json_end)) = package_footers.first() {
                let json = block
                    .get(*json_start..*json_end)
                    .ok_or(Error::Corrupt("package json slice out of bounds"))?;
                let pkg: StorePath =
                    sonic_rs::from_slice(json).map_err(|_| Error::StorePathParse {
                        path: json.to_vec(),
                    })?;
                let label = format!("{}.{}", pkg.origin().attr, pkg.origin().output);
                let ok = package_pattern.is_none_or(|re| re.is_match(pkg.name().as_bytes()))
                    && hash.is_none_or(|h| h == pkg.hash())
                    && package_labels.is_none_or(|labels| labels.contains(&label))
                    && package_ordinals.is_none_or(|ords| ords.contains(*ord));
                if ok {
                    for entry in found_without_package.split_off(0) {
                        matches.push((pkg.clone(), entry));
                    }
                } else {
                    found_without_package.clear();
                }
            }
        }

        let mut pos = 0;
        let mut footer_idx = 0;
        let mut cached_footer_idx = usize::MAX;
        let mut cached_pkg: Option<StorePath> = None;
        let mut cached_label: Option<String> = None;
        while let Some((line_start, line_end)) =
            next_matching_line(&matchers.path_matcher, block, pos)
        {
            pos = line_end;
            let entry = block
                .get(line_start..line_end - 1)
                .ok_or(Error::Corrupt("entry slice out of bounds"))?;

            // A path pattern can also match package footers (e.g. the package
            // name contains the literal).  They are metadata, not file paths.
            if matchers
                .package_entry_pattern
                .is_match(entry)
                .unwrap_or_else(consume_no_error)
            {
                continue;
            }

            // Advance to the first package footer that ends after this entry;
            // that footer belongs to the package containing the entry.
            while footer_idx < package_footers.len()
                && package_footers
                    .get(footer_idx)
                    .is_some_and(|(le, _, _, _)| *le <= line_end)
            {
                footer_idx += 1;
            }

            let sep = memchr::memchr(b'\0', entry).ok_or_else(|| Error::EntryParse {
                entry: entry.to_vec(),
            })?;
            let path = entry.get(sep + 1..).ok_or_else(|| Error::EntryParse {
                entry: entry.to_vec(),
            })?;
            if !exact_path.is_match(path) {
                continue;
            }
            let node =
                FileNode::decode_meta(entry.get(..sep).ok_or_else(|| Error::EntryParse {
                    entry: entry.to_vec(),
                })?)
                .ok_or_else(|| Error::EntryParse {
                    entry: entry.to_vec(),
                })?;
            let ft_entry = FileTreeEntry {
                path: path.to_vec(),
                node,
            };

            if footer_idx >= package_footers.len() {
                found_without_package.push(ft_entry);
                continue;
            }

            if footer_idx != cached_footer_idx {
                let (_, _ord, json_start, json_end) = package_footers[footer_idx];
                let json = block
                    .get(json_start..json_end)
                    .ok_or(Error::Corrupt("package json slice out of bounds"))?;
                let pkg: StorePath =
                    sonic_rs::from_slice(json).map_err(|_| Error::StorePathParse {
                        path: json.to_vec(),
                    })?;
                cached_pkg = Some(pkg);
                cached_label = Some(format!("{}.{}", cached_pkg.as_ref().unwrap().origin().attr, cached_pkg.as_ref().unwrap().origin().output));
                cached_footer_idx = footer_idx;
            }
            let pkg = cached_pkg.as_ref().unwrap();
            let label = cached_label.as_ref().unwrap();
            if !(package_pattern.is_none_or(|re| re.is_match(pkg.name().as_bytes()))
                && hash.is_none_or(|h| h == pkg.hash())
                && package_labels.is_none_or(|labels| labels.contains(label))
                && package_ordinals.is_none_or(|ords| ords.contains(package_footers[footer_idx].1)))
            {
                continue;
            }

            matches.push((pkg.clone(), ft_entry));
        }
    }

    if !found_without_package.is_empty() {
        return Err(Error::MissingPackageEntry);
    }

    Ok(matches)
}

// Per-thread zstd decompression context reused across frames during a parallel
// search. Without reuse, `search_entries` built a new streaming `Decoder` (and
// its window context, which can be tens of MiB at high compression levels)
// for every frame on every query — dominating the cost of small, selective
// queries such as `nix-locate bin/ls`.
thread_local! {
    static SEARCH_DECOMPRESSOR: std::cell::RefCell<Option<zstd::bulk::Decompressor<'static>>> =
        const { std::cell::RefCell::new(None) };
}

/// Decompress one frame into a freshly allocated `Vec<u8>`, reusing a
/// per-thread zstd decompression context so the window buffer is not
/// re-allocated for every frame.
///
/// The output `Vec` is owned by the caller: it outlives the thread-local
/// decompressor and is consumed wholesale by the frcode decoder before the next
/// frame on this worker runs, so there is no aliasing hazard. A defensive cap
/// rejects zstd bombs that would expand past [`crate::MAX_ZSTD_FRAME_BYTES`].
fn decompress_frame_threaded(compressed: &[u8]) -> std::result::Result<Vec<u8>, Error> {
    if compressed.is_empty() {
        return Ok(Vec::new());
    }

    SEARCH_DECOMPRESSOR.with(|cell| {
        let mut guard = cell.borrow_mut();
        let decompressor = match guard.as_mut() {
            Some(d) => d,
            None => {
                let d = match zstd::bulk::Decompressor::new() {
                    Ok(d) => d,
                    Err(err) => return Err(Error::Io(err)),
                };
                guard.insert(d)
            }
        };

        // Size the output from the frame's declared content size when available
        // so a single `bulk` decode usually suffices. If the header does not store
        // the content size (e.g. upstream NIXI v1 prebuilt indexes), fall back to a
        // streaming decode that grows the buffer incrementally rather than
        // repeatedly retrying `decompress_to_buffer` with geometrically larger
        // guesses, which is quadratic for large frames without a content-size field.
        let mut out = match zstd::zstd_safe::get_frame_content_size(compressed) {
            Ok(Some(s)) => {
                let size = usize::try_from(s)
                    .map_err(|_| Error::Corrupt("zstd frame content size exceeds usize"))?;
                if size > crate::MAX_ZSTD_FRAME_BYTES {
                    return Err(Error::Corrupt("zstd decompressed size exceeds limit"));
                }
                Vec::with_capacity(size)
            }
            Ok(None) => {
                // The frame header does not advertise a content size; use the
                // bounded streaming decoder so a zstd bomb cannot allocate past
                // the defensive cap.
                return crate::bounded_zstd_decode(compressed, crate::MAX_ZSTD_FRAME_BYTES)
                    .map_err(Error::Io);
            }
            Err(_) => return Err(Error::Corrupt("invalid zstd frame header")),
        };

        match decompressor.decompress_to_buffer(compressed, &mut out) {
            Ok(written) => {
                out.truncate(written);
                Ok(out)
            }
            Err(err) => Err(Error::Io(err)),
        }
    })
}

fn search_frame_raw(
    raw: &[u8],
    matchers: &FrameMatchers,
    exact_path: &Regex,
    package_pattern: Option<&Regex>,
    hash: Option<&str>,
    package_labels: Option<&IndexSet<String>>,
    frame_start_ordinal: Option<u32>,
    package_ordinals: Option<&RoaringBitmap>,
) -> Result<Vec<(StorePath, FileTreeEntry)>> {
    let mut decoder = frcode::Decoder::new(std::io::Cursor::new(raw));
    search_frame_decoder(
        &mut decoder,
        matchers,
        exact_path,
        package_pattern,
        hash,
        package_labels,
        frame_start_ordinal,
        package_ordinals,
    )
}

fn search_frame(
    compressed: &[u8],
    matchers: &FrameMatchers,
    exact_path: &Regex,
    package_pattern: Option<&Regex>,
    hash: Option<&str>,
    package_labels: Option<&IndexSet<String>>,
    frame_start_ordinal: Option<u32>,
    package_ordinals: Option<&RoaringBitmap>,
) -> Result<Vec<(StorePath, FileTreeEntry)>> {
    let raw = decompress_frame_threaded(compressed)?;
    search_frame_raw(
        &raw,
        matchers,
        exact_path,
        package_pattern,
        hash,
        package_labels,
        frame_start_ordinal,
        package_ordinals,
    )
}

/// Search a single zstd-compressed frame by streaming it through `frcode`.
///
/// This is used for NIXI v1 databases (e.g. the upstream prebuilt index) whose
/// single frame is far larger than the bounded in-memory decode limit and for
/// the first query against a `Reader` that has not yet cached the decompressed
/// frcode bytes.
fn search_frame_stream(
    compressed: &[u8],
    matchers: &FrameMatchers,
    exact_path: &Regex,
    package_pattern: Option<&Regex>,
    hash: Option<&str>,
    package_labels: Option<&IndexSet<String>>,
    frame_start_ordinal: Option<u32>,
    package_ordinals: Option<&RoaringBitmap>,
) -> Result<Vec<(StorePath, FileTreeEntry)>> {
    let cursor = std::io::Cursor::new(compressed);
    let mut zstd_decoder = zstd::stream::read::Decoder::new(cursor)?;
    zstd_decoder.window_log_max(crate::ZSTD_WINDOW_LOG_MAX)?;
    let mut decoder =
        frcode::Decoder::new(std::io::BufReader::with_capacity(1 << 20, zstd_decoder));
    search_frame_decoder(
        &mut decoder,
        matchers,
        exact_path,
        package_pattern,
        hash,
        package_labels,
        frame_start_ordinal,
        package_ordinals,
    )
}

/// Output mode for a search request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Full output with package/path details.
    Full {
        /// Emit ANSI colors in output.
        color: bool,
        /// Group matches that share the same matching path component.
        group: bool,
        /// Only print matches from top-level packages.
        only_toplevel: bool,
    },
    /// Print only attribute names.
    Minimal,
}

/// Sort order for search results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchSort {
    /// Preserve the order returned by the database reader.
    #[default]
    None,
    /// Sort by file size ascending.
    SizeAsc,
    /// Sort by file size descending.
    SizeDesc,
    /// Sort by attribute path ascending.
    AttrAsc,
}

impl std::str::FromStr for SearchSort {
    type Err = crate::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "" | "none" | "relevance" => Ok(Self::None),
            "size" | "size-asc" => Ok(Self::SizeAsc),
            "size-desc" => Ok(Self::SizeDesc),
            "attr" | "attr-asc" => Ok(Self::AttrAsc),
            _ => Err(crate::Error::Parse(format!("unknown sort order: {s}"))),
        }
    }
}

/// Options for a database search.
#[derive(Debug, Clone)]
pub struct SearchOptions<'a> {
    /// Directory that holds the index database.
    pub database: PathBuf,
    /// Pattern to search for (regex-ready string from the CLI).
    pub pattern: String,
    /// Restrict results to a store-path hash.
    pub hash: Option<String>,
    /// Restrict results to package names matching this pattern.
    pub package_pattern: Option<String>,
    /// Exact basename (final path component) to look up via the FST sidecar.
    pub exact_basename: Option<String>,
    /// Exact full path to look up via the path index sidecar.
    pub exact_path: Option<String>,
    /// Path prefix to look up via the path index sidecar.
    pub path_prefix: Option<String>,
    /// Literal (non-regex) substring pattern used to narrow candidate packages
    /// via the trigram inverted index. `None` for regex queries or when the
    /// n-gram sidecar is not available.
    pub literal_pattern: Option<String>,
    /// File-type filter (empty means "all types").
    pub file_type: &'a [FileType],
    /// Output formatting mode.
    pub mode: SearchMode,
    /// Emit each match as a JSON object (one per line) instead of the default
    /// human-readable format.
    pub json: bool,
    /// Maximum number of results to print. `None` means unlimited.
    pub limit: Option<usize>,
    /// Print the number of matching entries instead of the entries themselves.
    pub count: bool,
    /// Sort order for results.
    pub sort: SearchSort,
    /// Minimum file size in bytes.
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    pub max_size: Option<u64>,
    /// Exclude results from FHS-style packages (`-fhs` / `-usr-target`).
    pub exclude_fhs: bool,
}

/// Resolve the set of candidate package ordinals from the basename secondary index.
///
/// Errors are logged and treated as a full-scan fallback; `None` is returned
/// when there are no sidecar candidates.
#[allow(clippy::cognitive_complexity)]
fn resolve_package_ordinals(
    reader: &Reader,
    exact_basename: Option<&str>,
) -> Option<RoaringBitmap> {
    let base = exact_basename?;
    let dir = reader.path.parent()?;
    if dir.as_os_str().is_empty() {
        return None;
    }
    // Prefer the index already opened with the Reader; fall back to opening it
    // from disk so behaviour is unchanged when sidecars were absent at open time.
    let index = reader
        .basename
        .get_or_init(|| match BasenameIndex::open(dir) {
            Ok(b) => b,
            Err(err) => {
                if sidecars_exist(dir) {
                    tracing::warn!(%err, "basename index sidecars unreadable; falling back to full scan");
                }
                panic!("basename init failed: {err}");
            }
        });
    resolve_basename_with(index, base, dir)
}

fn resolve_basename_with(
    index: &BasenameIndex,
    base: &str,
    dir: &Path,
) -> Option<RoaringBitmap> {
    match index.lookup_basename_ordinals(base.as_bytes()) {
        Ok(ordinals) => Some(ordinals.into_iter().collect()),
        Err(err) => {
            if sidecars_exist(dir) {
                tracing::warn!(%err, "basename index sidecars unreadable; falling back to full scan");
            }
            None
        }
    }
}

/// Resolve the set of candidate package ordinals from the path secondary index.
///
/// Tries exact path lookup first, then prefix lookup. Errors are logged and
/// treated as a full-scan fallback; `None` is returned when there are no sidecar candidates.
#[allow(clippy::cognitive_complexity)]
fn resolve_path_ordinals(
    reader: &Reader,
    exact_path: Option<&str>,
    path_prefix: Option<&str>,
) -> Option<RoaringBitmap> {
    let dir = reader.path.parent()?;
    if dir.as_os_str().is_empty() {
        return None;
    }
    let index = reader
        .path_index
        .get_or_init(|| match PathIndex::open(dir) {
            Ok(idx) => idx,
            Err(err) => {
                if path_sidecars_exist(dir) {
                    tracing::warn!(%err, "path index sidecars unreadable; falling back to full scan");
                }
                panic!("path_index init failed: {err}");
            }
        });
    resolve_path_with(index, exact_path, path_prefix, dir)
}

fn resolve_path_with(
    index: &PathIndex,
    exact_path: Option<&str>,
    path_prefix: Option<&str>,
    dir: &Path,
) -> Option<RoaringBitmap> {
    // Try exact path lookup first
    if let Some(path) = exact_path {
        match index.lookup_path_ordinals(path.as_bytes()) {
            Ok(ordinals) => return Some(ordinals.into_iter().collect()),
            Err(err) => {
                if path_sidecars_exist(dir) {
                    tracing::warn!(%err, "path index exact lookup failed; falling back to full scan");
                }
                return None;
            }
        }
    }

    // Try prefix lookup
    if let Some(prefix) = path_prefix {
        match index.lookup_prefix_ordinals(prefix.as_bytes()) {
            Ok(ordinals) => return Some(ordinals.into_iter().collect()),
            Err(err) => {
                if path_sidecars_exist(dir) {
                    tracing::warn!(%err, "path index prefix lookup failed; falling back to full scan");
                }
                return None;
            }
        }
    }

    None
}

/// Check if path index sidecars exist in the directory.
fn path_sidecars_exist(dir: &Path) -> bool {
    [
        crate::path_index::FST_FILE,
        crate::path_index::POSTINGS_FILE,
    ]
    .iter()
    .any(|name| dir.join(name).is_file())
}

/// Whether the trigram inverted-index sidecars are present in `dir`.
fn ngram_sidecars_exist(dir: &Path) -> bool {
    [
        crate::ngram_index::FST_FILE,
        crate::ngram_index::POSTINGS_FILE,
    ]
    .iter()
    .any(|name| dir.join(name).is_file())
}

/// Resolve the set of candidate package ordinals from the trigram (n-gram)
/// inverted path index for a LITERAL (non-regex) substring pattern.
///
/// Returns `None` when the pattern is not literal (too short / regex-like), the
/// sidecars are absent, or they are unreadable — callers fall back to a full scan.
#[allow(clippy::cognitive_complexity)]
fn resolve_ngram_ordinals(reader: &Reader, literal: Option<&str>) -> Option<RoaringBitmap> {
    let pat = literal?;
    let dir = reader.path.parent()?;
    if dir.as_os_str().is_empty() {
        return None;
    }
    // Prefer the index already opened with the Reader; fall back to opening it
    // from disk so behaviour is unchanged when sidecars were absent at open time.
    let index = reader
        .ngram
        .get_or_init(|| match NgramIndex::open(dir) {
            Ok(idx) => idx,
            Err(err) => {
                if ngram_sidecars_exist(dir) {
                    tracing::warn!(%err, "ngram index unreadable; falling back to full scan");
                }
                panic!("ngram init failed: {err}");
            }
        });
    resolve_ngram_with(index, pat, dir)
}

fn resolve_ngram_with(index: &NgramIndex, pat: &str, dir: &Path) -> Option<RoaringBitmap> {
    match index.candidate_ordinals(pat) {
        Ok(Some(bm)) => Some(bm),
        // Not a literal pattern, or the intersection is empty (matches nothing).
        Ok(None) => None,
        Err(err) => {
            if ngram_sidecars_exist(dir) {
                tracing::warn!(%err, "ngram candidate lookup failed; falling back to full scan");
            }
            None
        }
    }
}

/// Format the `attr.output` label for a result, wrapping non-toplevels in parentheses.
fn format_attr(store_path: &StorePath) -> String {
    let mut attr = format!(
        "{}.{}",
        store_path.origin().attr,
        store_path.origin().output
    );
    if !store_path.origin().toplevel {
        attr = format!("({attr})");
    }
    attr
}

/// Returns `true` if `entry` should be printed under the current search options.
fn should_include_match(
    options: &SearchOptions<'_>,
    path_pattern: &Regex,
    store_path: &StorePath,
    entry: &FileTreeEntry,
) -> bool {
    let group = matches!(options.mode, SearchMode::Full { group: true, .. });
    if group
        && path_pattern.find_iter(&entry.path).last().is_some_and(|m| {
            entry
                .path
                .get(m.end()..)
                .is_some_and(|rest| rest.contains(&b'/'))
        })
    {
        return false;
    }

    let only_toplevel = matches!(
        options.mode,
        SearchMode::Full {
            only_toplevel: true,
            ..
        }
    );
    if only_toplevel && !store_path.origin().toplevel {
        return false;
    }

    let entry_type = entry.node.get_type();
    if !options.file_type.is_empty() && !options.file_type.contains(&entry_type) {
        return false;
    }

    let size = entry.node.size();
    if options.min_size.is_some_and(|min| size < min) {
        return false;
    }
    if options.max_size.is_some_and(|max| size > max) {
        return false;
    }

    if options.exclude_fhs {
        let name = store_path.name();
        if name.contains("-fhs") || name.contains("-usr-target") {
            return false;
        }
    }

    true
}

/// JSON-serializable full search result emitted by `--json`.
#[derive(Serialize)]
struct MatchJson {
    attr: String,
    size: u64,
    #[serde(rename = "type")]
    kind: String,
    path: String,
    store_path: String,
}

/// JSON-serializable minimal search result emitted by `--minimal --json`.
#[derive(Serialize)]
struct MinimalMatchJson {
    attr: String,
}

/// Print a single search result according to `SearchMode` and color settings.
///
/// Mutates `printed_attrs` for `--minimal` de-duplication.
///
/// Returns `true` if a line was actually emitted.
#[allow(clippy::print_stdout)] // search is a CLI-facing printer for now
fn print_match(
    options: &SearchOptions<'_>,
    path_pattern: &Regex,
    printed_attrs: &mut IndexSet<String>,
    store_path: &StorePath,
    entry: &FileTreeEntry,
) -> bool {
    let attr = format_attr(store_path);

    if options.json {
        print_match_json(options, printed_attrs, store_path, entry, &attr)
    } else {
        print_match_text(
            options,
            path_pattern,
            printed_attrs,
            store_path,
            entry,
            &attr,
        )
    }
}

#[allow(clippy::print_stdout)]
fn print_match_json(
    options: &SearchOptions<'_>,
    printed_attrs: &mut IndexSet<String>,
    store_path: &StorePath,
    entry: &FileTreeEntry,
    attr: &str,
) -> bool {
    match options.mode {
        SearchMode::Minimal => {
            if printed_attrs.insert(attr.into()) {
                let record = MinimalMatchJson {
                    attr: attr.to_string(),
                };
                if let Ok(line) = sonic_rs::to_string(&record) {
                    println!("{line}");
                    return true;
                }
            }
            false
        }
        SearchMode::Full { .. } => {
            let (kind, size) = match &entry.node {
                FileNode::Regular { executable, size } => {
                    (if *executable { "x" } else { "r" }, *size)
                }
                FileNode::Directory { size, .. } => ("d", *size),
                FileNode::Symlink { .. } => ("s", 0),
            };
            let record = MatchJson {
                attr: attr.to_string(),
                size,
                kind: kind.to_string(),
                path: String::from_utf8_lossy(&entry.path).into_owned(),
                store_path: store_path.as_str(),
            };
            if let Ok(line) = sonic_rs::to_string(&record) {
                println!("{line}");
                true
            } else {
                false
            }
        }
    }
}

#[allow(clippy::print_stdout)]
fn print_match_text(
    options: &SearchOptions<'_>,
    path_pattern: &Regex,
    printed_attrs: &mut IndexSet<String>,
    store_path: &StorePath,
    entry: &FileTreeEntry,
    attr: &str,
) -> bool {
    match options.mode {
        SearchMode::Minimal => {
            if printed_attrs.insert(attr.into()) {
                println!("{attr}");
                return true;
            }
            false
        }
        SearchMode::Full { color, .. } => {
            let (typ, size) = match &entry.node {
                FileNode::Regular { executable, size } => {
                    (if *executable { "x" } else { "r" }, *size)
                }
                FileNode::Directory { size, .. } => ("d", *size),
                FileNode::Symlink { .. } => ("s", 0),
            };
            let size_str = format_grouped(size);
            print!("{attr:<40} {size_str:>14} {typ:>1} {}", store_path.as_str());

            let path_str = String::from_utf8_lossy(&entry.path);
            if color {
                // Highlight all non-empty matches in the path.
                let mut prev = 0usize;
                let bytes = path_str.as_bytes();
                for mat in path_pattern.find_iter(bytes) {
                    if mat.start() == mat.end() {
                        continue;
                    }
                    // Safe because we only slice on byte offsets from the same str.
                    if let (Some(before), Some(matched)) = (
                        path_str.get(prev..mat.start()),
                        path_str.get(mat.start()..mat.end()),
                    ) {
                        print!("{before}\x1b[31m{matched}\x1b[0m");
                    }
                    prev = mat.end();
                }
                if let Some(rest) = path_str.get(prev..) {
                    println!("{rest}");
                } else {
                    println!();
                }
            } else {
                println!("{path_str}");
            }
            true
        }
    }
}

/// Search the database for entries matching the supplied options.
///
/// Returns the filtered and sorted `(StorePath, FileTreeEntry)` pairs. This is
/// the shared engine used by both the CLI `nix-locate` and the daemon's
/// `/nix-locate` endpoint.
///
/// Compile a user-supplied regex with defensive size/length limits.
fn compile_search_regex(pattern: &str, kind: &str) -> crate::Result<Regex> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(crate::Error::Parse(format!(
            "{kind} regex exceeds maximum length of {MAX_PATTERN_BYTES} bytes"
        )));
    }
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|err| crate::Error::Parse(format!("invalid {kind} regex '{pattern}': {err}")))
}

/// # Errors
///
/// Returns an error if the database cannot be read or the pattern is invalid.
#[allow(clippy::cognitive_complexity)]
pub fn search_results(
    options: &SearchOptions<'_>,
) -> crate::Result<Vec<(crate::StorePath, crate::files::FileTreeEntry)>> {
    let index_file = options.database.join("files");
    let reader = Reader::open(&index_file).map_err(|source| crate::Error::ReadDatabase {
        path: index_file.clone(),
        source: Box::new(source),
    })?;
    search_results_with_reader(&reader, &index_file, options)
}

/// Like [`search_results`], but uses an already-opened [`Reader`].
///
/// This lets long-running holders such as the resident daemon reuse the same
/// mmap-backed reader (and any cached decoded frames) across queries instead of
/// paying the open/decompression cost on every request.
///
/// # Errors
///
/// Returns an error if the pattern is invalid or the search fails.
#[allow(clippy::cognitive_complexity)]
#[allow(clippy::too_many_lines)]
pub fn search_results_with_reader(
    reader: &Reader,
    index_file: &Path,
    options: &SearchOptions<'_>,
) -> crate::Result<Vec<(crate::StorePath, crate::files::FileTreeEntry)>> {
    let path_pattern = compile_search_regex(&options.pattern, "path")?;
    let package_re = match &options.package_pattern {
        Some(pat) => Some(compile_search_regex(pat, "package")?),
        None => None,
    };

    // Resolve ordinals from basename index (for exact basename queries)
    let basename_ordinals = resolve_package_ordinals(reader, options.exact_basename.as_deref());

    // Resolve ordinals from path index (for rooted/prefix queries)
    let path_ordinals = resolve_path_ordinals(
        reader,
        options.exact_path.as_deref(),
        options.path_prefix.as_deref(),
    );

    // Combine basename + path ordinals (union) into the base candidate set.
    let base_ordinals: Option<RoaringBitmap> = match (basename_ordinals, path_ordinals) {
        (Some(b), Some(p)) => Some(b | &p),
        (Some(b), None) => Some(b),
        (None, Some(p)) => Some(p),
        (None, None) => None,
    };

    // Narrow the candidate set further with the trigram inverted index when the
    // query is a literal substring. An empty intersection means nothing matches.
    let ngram_ordinals = resolve_ngram_ordinals(reader, options.literal_pattern.as_deref());

    let package_ordinals: Option<RoaringBitmap> = match (base_ordinals, ngram_ordinals) {
        (Some(b), Some(ng)) => Some(b & &ng),
        (Some(b), None) => Some(b),
        (None, Some(ng)) => Some(ng),
        (None, None) => None,
    };

    // Only short-circuit before the fast paths when we already know the exact
    // basename/path/prefix/literal candidates are empty and no fast path could
    // change that. If any fast path may apply, let it run.
    if package_ordinals
        .as_ref()
        .is_some_and(RoaringBitmap::is_empty)
        && options.exact_path.is_none()
        && options.exact_basename.is_none()
        && options.literal_pattern.is_none()
    {
        return Ok(Vec::new());
    }

    // Try the fast path sidecars in order of specificity:
    // 1. exact full path, 2. exact basename, 3. literal substring.
    let mut results = Vec::new();
    let mut used_fast_path = false;

    // 1. Exact full path: prefer the per-path entry cache, then redb.
    if let Some(exact_path) = &options.exact_path {
        let bytes = exact_path.as_bytes();
        if let Some(path_entry) = reader.path_entry.get() {
            let mut hits = Vec::new();
            for (store_path, entry) in
                path_entry
                    .lookup_entries(bytes)
                    .map_err(|source| crate::Error::ReadDatabase {
                        path: index_file.to_path_buf(),
                        source: Box::new(Error::PathEntryIndex(source)),
                    })?
            {
                if package_re
                    .as_ref()
                    .is_none_or(|re| re.is_match(store_path.name().as_bytes()))
                    && options
                        .hash
                        .as_deref()
                        .is_none_or(|h| h == store_path.hash())
                    && should_include_match(options, &path_pattern, &store_path, &entry)
                {
                    hits.push((store_path, entry));
                }
            }
            results = hits;
            used_fast_path = true;
        } else if let Some(redb) = reader.redb.get() {
            let mut hits = Vec::new();
            match redb.exact_path_entries(bytes) {
                Ok(Some(redb_hits)) => {
                    for (store_path, entry) in redb_hits {
                        if package_re
                            .as_ref()
                            .is_none_or(|re| re.is_match(store_path.name().as_bytes()))
                            && options
                                .hash
                                .as_deref()
                                .is_none_or(|h| h == store_path.hash())
                            && should_include_match(options, &path_pattern, &store_path, &entry)
                        {
                            hits.push((store_path, entry));
                        }
                    }
                    results = hits;
                    used_fast_path = true;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::debug!(%err, "redb exact-path lookup failed; falling back to scan");
                }
            }
        }
    }

    // 2. Exact basename (whole_name without --root, or basename portion only).
    if !used_fast_path
        && options.exact_basename.is_some()
        && options.exact_path.is_none()
        && options.path_prefix.is_none()
    {
        let base_bytes = match options.exact_basename.as_deref() {
            Some(s) => s.as_bytes(),
            None => b"",
        };
        match reader.search_entry_index(
            base_bytes,
            &path_pattern,
            package_re.as_ref(),
            options.hash.as_deref(),
            options,
        ) {
            Ok(Some(entry_results)) => {
                results = entry_results;
                used_fast_path = true;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(%err, "entry index search failed; falling back");
            }
        }
    }

    // 3. Literal substring: path-level trigram + entry cache.
    if !used_fast_path
        && options.literal_pattern.is_some()
        && options.exact_basename.is_none()
        && options.exact_path.is_none()
        && options.path_prefix.is_none()
    {
        let pattern_str: &str = match options.literal_pattern.as_deref() {
            Some(s) => s,
            None => &options.pattern,
        };
        match reader.search_path_trigram(
            pattern_str,
            &path_pattern,
            package_re.as_ref(),
            options.hash.as_deref(),
            options,
        ) {
            Ok(Some(path_results)) => {
                results = path_results;
                used_fast_path = true;
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(%err, "path trigram search failed; falling back");
            }
        }
    }

    // 4. Fall back to frcode scanning when no fast path applied.
    if !used_fast_path {
        results = reader
            .search_entries(
                &path_pattern,
                package_re.as_ref(),
                options.hash.as_deref(),
                None, // package_labels not used with ordinals
                package_ordinals.as_ref(),
            )
            .map_err(|source| crate::Error::ReadDatabase {
                path: index_file.to_path_buf(),
                source: Box::new(source),
            })?;
    }


    match options.sort {
        SearchSort::None => {}
        SearchSort::SizeAsc => {
            results.sort_by_key(|(_, entry)| entry.node.size());
        }
        SearchSort::SizeDesc => {
            results.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.node.size()));
        }
        SearchSort::AttrAsc => {
            results.sort_by(|(a, _), (b, _)| a.origin().attr.cmp(&b.origin().attr));
        }
    }

    let mut results: Vec<_> = results
        .into_iter()
        .filter(|(store_path, entry)| {
            should_include_match(options, &path_pattern, store_path, entry)
        })
        .collect();

    // Apply limit before returning to avoid materializing full results
    if let Some(limit) = options.limit {
        results.truncate(limit);
    }

    Ok(results)
}

/// Search the database for entries matching the supplied options and print them.
///
/// # Errors
///
/// Returns an error if the database cannot be read or the pattern is invalid.
pub fn search(options: &SearchOptions<'_>) -> crate::Result<()> {
    // Compile the path pattern once with defensive size limits for both the
    // search and the output pass (highlighting, grouping).
    let path_pattern = compile_search_regex(&options.pattern, "path")?;
    let results = search_results(options)?;

    // Track printed attrs for --minimal de-duplication (ordered set).
    let mut printed_attrs: IndexSet<String> = IndexSet::new();

    let mut matched = 0usize;
    let mut printed = 0usize;

    for (store_path, entry) in results {
        matched += 1;

        if options.count {
            continue;
        }

        if options.limit.is_some_and(|limit| printed >= limit) {
            break;
        }

        if print_match(
            options,
            &path_pattern,
            &mut printed_attrs,
            &store_path,
            &entry,
        ) {
            printed += 1;
        }
    }

    if options.count {
        println!("{matched}");
    }

    Ok(())
}

/// Generate nixdex sidecars for an existing NIXI database.
///
/// Reads the `files` database at `db_path`, extracts all package basenames and
/// metadata, and writes the sidecar files (`files.basename.fst`,
/// `files.basename.postings`, `files.basename.names`, `files.frame_map`,
/// `packages.json`) to the same directory.
///
/// This enables fast basename lookups via `BasenameIndex` and package search
/// via `SearchDb` for prebuilt indexes downloaded from upstream
/// nix-index-database releases.
///
/// # Errors
///
/// Returns an error if the database cannot be read, sidecars cannot be written,
/// or the database is corrupt.
pub fn generate_sidecars(db_path: &Path) -> Result<()> {
    generate_sidecars_impl(db_path, true)
}

/// Like [`generate_sidecars`], but when `include_heavy` is `false` the heavy
/// entry/ngram secondary indexes are skipped. The daemon uses this in `Lru`
/// cache mode to defer their construction; queries fall back to full scans
/// until the sidecars are built.
#[cfg(feature = "daemon")]
pub(crate) fn generate_sidecars_mode(db_path: &Path, include_heavy: bool) -> Result<()> {
    generate_sidecars_impl(db_path, include_heavy)
}

#[allow(clippy::cognitive_complexity)]
#[allow(clippy::too_many_lines)]
fn generate_sidecars_impl(db_path: &Path, include_heavy: bool) -> Result<()> {
    let reader = Reader::open(db_path)?;
    let db_dir = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(Error::Corrupt("database path has no parent directory"))?;

    // Scan all frames to extract package paths and build secondary indexes.
    let mut builder = BasenameIndexBuilder::new();
    let mut path_builder = PathIndexBuilder::new();
    let mut entry_builder = if include_heavy {
        Some(crate::entry_index::EntryIndexBuilder::new())
    } else {
        None
    };
    let mut path_entry_builder = if include_heavy {
        Some(crate::path_entry_index::PathEntryIndexBuilder::new())
    } else {
        None
    };
    let mut ngram_builder = if include_heavy {
        Some(crate::ngram_index::NgramIndexBuilder::new())
    } else {
        None
    };
    let mut all_package_labels: Vec<String> = Vec::new();
    let mut all_package_meta: Vec<PackageMeta> = Vec::new();
    let mut all_package_attrs: Vec<(String, String, String)> = Vec::new();

    // NIXI v1 databases (upstream prebuilt indexes) are a single large zstd frame.
    // Stream-decode them instead of materialising the whole frame in memory.
    if reader.version == 1 {
        if let Some((offset, len)) = reader.frames.first() {
            let start = *offset;
            let end = start + *len;
            let compressed = reader
                .data
                .get(start..end)
                .ok_or(Error::Corrupt("frame slice out of range"))?;
            scan_frame_stream_for_packages(
                compressed,
                &mut builder,
                &mut path_builder,
                entry_builder.as_mut(),
                path_entry_builder.as_mut(),
                ngram_builder.as_mut(),
                &mut all_package_labels,
                &mut all_package_meta,
                &mut all_package_attrs,
            )?;
        }
    } else {
        for (offset, len) in &reader.frames {
            let start = *offset;
            let end = start + *len;
            let compressed = reader
                .data
                .get(start..end)
                .ok_or(Error::Corrupt("frame slice out of range"))?;

            let raw = if compressed.is_empty() {
                Vec::new()
            } else {
                crate::bounded_zstd_decode(compressed, crate::MAX_ZSTD_FRAME_BYTES)?
            };

            scan_frame_for_packages(
                &raw,
                &mut builder,
                &mut path_builder,
                entry_builder.as_mut(),
                path_entry_builder.as_mut(),
                ngram_builder.as_mut(),
                &mut all_package_labels,
                &mut all_package_meta,
                &mut all_package_attrs,
            )?;
        }
    }

    // Write basename and path sidecars.
    builder
        .write_sidecars(db_dir)
        .map_err(Error::SecondaryIndex)?;
    path_builder
        .write_sidecars(db_dir)
        .map_err(Error::PathIndex)?;

    // Build and write the heavy secondary indexes when requested.
    if let (Some(entry_builder), Some(path_entry_builder), Some(ngram_builder)) =
        (entry_builder, path_entry_builder, ngram_builder)
    {
        // Write entry-level basename sidecar (fast command-not-found lookups).
        entry_builder
            .write_sidecars(db_dir)
            .map_err(Error::EntryIndex)?;

        // Build path-level trigram index from the sorted path entry map.
        let mut path_trigram_builder = crate::path_trigram_index::PathTrigramIndexBuilder::new();
        let mut path_id = 0u32;
        for (path, _records) in path_entry_builder.iter() {
            path_trigram_builder
                .record_path(path_id, path)
                .map_err(Error::PathTrigramIndex)?;
            path_id = path_id
                .checked_add(1)
                .ok_or(Error::Corrupt("path-id overflow in path trigram index"))?;
        }
        path_trigram_builder
            .write_sidecars(db_dir)
            .map_err(Error::PathTrigramIndex)?;

        // Write per-path entry cache last, after the trigram builder has consumed
        // the sorted iterator.
        path_entry_builder
            .write_sidecars(db_dir)
            .map_err(Error::PathEntryIndex)?;

        // Write trigram (n-gram) inverted path index for fast literal queries.
        ngram_builder
            .write_sidecars(db_dir)
            .map_err(Error::NgramIndex)?;
    }

    // Write frame_map sidecar if we have v2 frame data.
    if let (Some(frame_map), Some(_frame_starts)) = (&reader.frame_map, &reader.frame_starts) {
        write_frame_map(db_dir, frame_map, reader.frames.len())?;
    }

    // Synthesize a packages.json sidecar from package footers so that
    // package metadata search works for prebuilt file-only indexes.
    write_packages_json(db_dir, &all_package_meta)?;

    // Write attrs sidecar so prebuilt indexes can be reused for incremental builds.
    write_attrs_sidecar(db_dir, &all_package_attrs)?;

    tracing::info!(
        db_path = %db_path.display(),
        packages = all_package_labels.len(),
        "generated nixdex sidecars"
    );

    Ok(())
}

/// Write a `packages.json` NDJSON sidecar from extracted package metadata.
fn write_packages_json(db_dir: &Path, packages: &[PackageMeta]) -> Result<()> {
    let path = db_dir.join("packages.json");
    let file = std::fs::File::create(&path)?;
    let mut writer = std::io::BufWriter::new(file);

    for package in packages {
        let line = sonic_rs::to_string(package).map_err(|err| Error::Json(err.to_string()))?;
        writeln!(writer, "{line}")?;
    }

    writer.flush()?;
    Ok(())
}

/// Scan a decoded frcode stream to extract package paths.
#[allow(clippy::too_many_arguments)]
fn scan_decoder_for_packages<R: std::io::BufRead>(
    decoder: &mut frcode::Decoder<R>,
    builder: &mut BasenameIndexBuilder,
    path_builder: &mut PathIndexBuilder,
    mut entry_builder: Option<&mut crate::entry_index::EntryIndexBuilder>,
    mut path_entry_builder: Option<&mut crate::path_entry_index::PathEntryIndexBuilder>,
    mut ngram_builder: Option<&mut crate::ngram_index::NgramIndexBuilder>,
    all_package_labels: &mut Vec<String>,
    all_package_meta: &mut Vec<PackageMeta>,
    all_package_attrs: &mut Vec<(String, String, String)>,
) -> Result<()> {
    let mut current_paths: Vec<Vec<u8>> = Vec::new();
    let mut current_entries: Vec<FileTreeEntry> = Vec::new();

    loop {
        let block = decoder.decode()?;
        if block.is_empty() {
            break;
        }

        for line in block.split(|c| *c == b'\n') {
            if line.is_empty() {
                continue;
            }

            if line.starts_with(b"p\0") {
                // Package footer: record accumulated paths and reset.
                if !current_paths.is_empty() {
                    let json = line.get(2..).ok_or_else(|| Error::StorePathParse {
                        path: line.to_vec(),
                    })?;
                    let pkg: StorePath =
                        sonic_rs::from_slice(json).map_err(|_| Error::StorePathParse {
                            path: json.to_vec(),
                        })?;
                    let attr = pkg.origin().attr.clone();
                    let output = pkg.origin().output.clone();
                    let label = format!("{}.{}", attr, output);
                    all_package_labels.push(label.clone());
                    all_package_meta.push(PackageMeta {
                        attr: attr.clone(),
                        name: pkg.name().to_string(),
                        description: None,
                        main_program: None,
                    });
                    all_package_attrs.push((attr, output, pkg.hash().to_string()));

                    let paths = std::mem::take(&mut current_paths);
                    let entries = std::mem::take(&mut current_entries);
                    let ordinal = builder
                        .record_package(label, paths.clone())
                        .map_err(Error::SecondaryIndex)?;
                    path_builder
                        .record_package(ordinal, paths.clone())
                        .map_err(Error::PathIndex)?;
                    if let Some(entry_builder) = entry_builder.as_mut() {
                        entry_builder
                            .record_package(&pkg, &entries)
                            .map_err(Error::EntryIndex)?;
                    }
                    if let Some(path_entry_builder) = path_entry_builder.as_mut() {
                        path_entry_builder
                            .record_package(&pkg, &entries)
                            .map_err(Error::PathEntryIndex)?;
                    }
                    if let Some(ngram_builder) = ngram_builder.as_mut() {
                        ngram_builder
                            .record_package(ordinal, paths)
                            .map_err(Error::NgramIndex)?;
                    }
                }
            } else {
                // File entry: extract path and (when building the entry index) node.
                let sep = memchr::memchr(b'\0', line).ok_or_else(|| Error::EntryParse {
                    entry: line.to_vec(),
                })?;
                let (meta, rest) = line.split_at(sep);
                let path = rest.get(1..).ok_or_else(|| Error::EntryParse {
                    entry: line.to_vec(),
                })?;
                current_paths.push(path.to_vec());
                if entry_builder.is_some() || path_entry_builder.is_some() {
                    let node =
                        FileNode::<()>::decode_meta(meta).ok_or_else(|| Error::EntryParse {
                            entry: line.to_vec(),
                        })?;
                    current_entries.push(FileTreeEntry {
                        path: path.to_vec(),
                        node,
                    });
                }
            }
        }
    }

    Ok(())
}

/// Scan a single in-memory frcode frame to extract package paths.
#[allow(clippy::too_many_arguments)]
fn scan_frame_for_packages(
    raw: &[u8],
    builder: &mut BasenameIndexBuilder,
    path_builder: &mut PathIndexBuilder,
    entry_builder: Option<&mut crate::entry_index::EntryIndexBuilder>,
    path_entry_builder: Option<&mut crate::path_entry_index::PathEntryIndexBuilder>,
    ngram_builder: Option<&mut crate::ngram_index::NgramIndexBuilder>,
    all_package_labels: &mut Vec<String>,
    all_package_meta: &mut Vec<PackageMeta>,
    all_package_attrs: &mut Vec<(String, String, String)>,
) -> Result<()> {
    let mut decoder = frcode::Decoder::new(std::io::Cursor::new(raw));
    scan_decoder_for_packages(
        &mut decoder,
        builder,
        path_builder,
        entry_builder,
        path_entry_builder,
        ngram_builder,
        all_package_labels,
        all_package_meta,
        all_package_attrs,
    )
}

/// Scan a single zstd-compressed frcode frame by streaming decompression.
///
/// This is used for NIXI v1 databases whose single frame is far larger than the
/// bounded in-memory decode limit.
#[allow(clippy::too_many_arguments)]
fn scan_frame_stream_for_packages(
    compressed: &[u8],
    builder: &mut BasenameIndexBuilder,
    path_builder: &mut PathIndexBuilder,
    entry_builder: Option<&mut crate::entry_index::EntryIndexBuilder>,
    path_entry_builder: Option<&mut crate::path_entry_index::PathEntryIndexBuilder>,
    ngram_builder: Option<&mut crate::ngram_index::NgramIndexBuilder>,
    all_package_labels: &mut Vec<String>,
    all_package_meta: &mut Vec<PackageMeta>,
    all_package_attrs: &mut Vec<(String, String, String)>,
) -> Result<()> {
    let cursor = std::io::Cursor::new(compressed);
    let mut zstd_decoder = zstd::stream::read::Decoder::new(cursor)?;
    zstd_decoder.window_log_max(crate::ZSTD_WINDOW_LOG_MAX)?;
    let mut decoder =
        frcode::Decoder::new(std::io::BufReader::with_capacity(1 << 20, zstd_decoder));
    scan_decoder_for_packages(
        &mut decoder,
        builder,
        path_builder,
        entry_builder,
        path_entry_builder,
        ngram_builder,
        all_package_labels,
        all_package_meta,
        all_package_attrs,
    )
}

/// Format an integer with thousands separators (e.g. `16_524` → `"16,524"`).
fn format_grouped(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        // Insert a comma before every group of three digits counting from the right.
        let remaining = bytes.len() - i;
        if i > 0 && remaining.is_multiple_of(3) {
            out.push(',');
        }
        out.push(char::from(*b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::FileTree;
    use crate::store_path::Origin;
    use bytes::Bytes;
    use std::io::Read;

    fn sample_store_path() -> StorePath {
        StorePath::new(
            "/nix/store".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "hello-2.12".into(),
            Origin {
                attr: "hello".into(),
                output: "out".into(),
                toplevel: true,
                system: Some("x86_64-linux".into()),
            },
        )
    }

    fn sample_tree() -> FileTree {
        FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(vec![(
                Bytes::from_static(b"hello"),
                FileTree::regular(64472, true),
            )]),
        )])
    }

    #[test]
    fn writer_reader_roundtrip_and_search() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let path = sample_store_path();
        let tree = sample_tree();

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&path, &tree, b"").expect("add");
            let size = writer.finish().expect("finish");
            assert!(size > 0);
        }

        // Magic check
        let mut f = File::open(&db_path).expect("open");
        let mut magic = [0u8; 4];
        f.read_exact(&mut magic).expect("magic");
        assert_eq!(&magic, b"NIXI");

        let reader = Reader::open(&db_path).expect("reader");
        let re = Regex::new(&regex::escape("bin/hello")).expect("regex");
        let hits = reader
            .search_entries(&re, None, None, None, None)
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits.first().map(|(p, _)| p.name()), Some("hello-2.12"));
        assert_eq!(
            hits.first().map(|(_, e)| e.path.as_slice()),
            Some(b"/bin/hello".as_slice())
        );

        // Public search() printer
        let options = SearchOptions {
            database: dir.path().to_path_buf(),
            pattern: "bin/hello".into(),
            hash: None,
            package_pattern: None,
            exact_basename: None,
            exact_path: None,
            path_prefix: None,
            literal_pattern: None,
            file_type: &[],
            mode: SearchMode::Minimal,
            json: false,
            limit: None,
            count: false,
            sort: SearchSort::None,
            min_size: None,
            max_size: None,
            exclude_fhs: false,
        };
        search(&options).expect("search ok");
    }

    #[test]
    fn ngram_narrows_search_results() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");
        let path = sample_store_path();
        let tree = sample_tree();

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&path, &tree, b"").expect("add");
            writer.finish().expect("finish");
        }

        // Build the secondary sidecars (basename, path, entry, ngram).
        generate_sidecars(&db_path).expect("sidecars");

        let options = SearchOptions {
            database: dir.path().to_path_buf(),
            pattern: "bin/hello".into(),
            hash: None,
            package_pattern: None,
            exact_basename: None,
            exact_path: None,
            path_prefix: None,
            literal_pattern: Some("bin/hello".into()),
            file_type: &[],
            mode: SearchMode::Minimal,
            json: false,
            limit: None,
            count: false,
            sort: SearchSort::None,
            min_size: None,
            max_size: None,
            exclude_fhs: false,
        };

        let hits = search_results(&options).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.path.as_slice(), b"/bin/hello");
        assert_eq!(hits[0].0.name(), "hello-2.12");
    }

    // Differential test: sidecar-pruned `search_results` must return exactly the
    // same (store_path, path) set as a full-scan regex over the same literal
    // substring. The ngram sidecar narrows by *package* ordinal, so this exercises
    // that package ordinals in the sidecar indexes align with the decoder's
    // per-frame `current_ordinal` (incl. multi-frame `frame_starts` threading).
    #[test]
    fn sidecar_search_matches_full_scan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let make_sp = |hash: &str, name: &str, attr: &str| {
            StorePath::new(
                "/nix/store".into(),
                hash.into(),
                name.into(),
                Origin {
                    attr: attr.into(),
                    output: "out".into(),
                    toplevel: true,
                    system: Some("x86_64-linux".into()),
                },
            )
        };
        let hello = make_sp("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "hello-2.12", "hello");
        let coreutils = make_sp(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "coreutils-9.5",
            "coreutils",
        );
        let grep = make_sp("cccccccccccccccccccccccccccccccc", "grep-3.11", "grep");

        // Filler so the raw database is large enough to span multiple zstd frames,
        // which is the path that exercises `frame_map`/`frame_starts` alignment.
        let filler: Vec<(Bytes, FileTree)> = (0..60)
            .map(|i| {
                (
                    Bytes::from(format!("tool{i}").into_bytes()),
                    FileTree::regular(64472, true),
                )
            })
            .collect();

        let hello_tree = FileTree::directory(vec![
            (
                Bytes::from_static(b"bin"),
                FileTree::directory(
                    std::iter::once((Bytes::from_static(b"hello"), FileTree::regular(64472, true)))
                        .chain(filler.iter().cloned())
                        .collect(),
                ),
            ),
            (
                Bytes::from_static(b"lib"),
                FileTree::directory(vec![(
                    Bytes::from_static(b"hello.a"),
                    FileTree::regular(64472, false),
                )]),
            ),
            (
                Bytes::from_static(b"share"),
                FileTree::directory(vec![(
                    Bytes::from_static(b"doc"),
                    FileTree::directory(vec![(
                        Bytes::from_static(b"hello"),
                        FileTree::directory(vec![(
                            Bytes::from_static(b"README"),
                            FileTree::regular(64472, false),
                        )]),
                    )]),
                )]),
            ),
        ]);
        let coreutils_tree = FileTree::directory(vec![
            (
                Bytes::from_static(b"bin"),
                FileTree::directory(
                    std::iter::once((Bytes::from_static(b"ls"), FileTree::regular(64472, true)))
                        .chain(std::iter::once((
                            Bytes::from_static(b"cat"),
                            FileTree::regular(64472, true),
                        )))
                        .chain(filler.iter().cloned())
                        .collect(),
                ),
            ),
            (
                Bytes::from_static(b"lib"),
                FileTree::directory(vec![(
                    Bytes::from_static(b"libc.so"),
                    FileTree::regular(64472, false),
                )]),
            ),
            (
                Bytes::from_static(b"share"),
                FileTree::directory(vec![(
                    Bytes::from_static(b"man"),
                    FileTree::directory(vec![(
                        Bytes::from_static(b"man1"),
                        FileTree::directory(vec![(
                            Bytes::from_static(b"ls.1"),
                            FileTree::regular(64472, false),
                        )]),
                    )]),
                )]),
            ),
        ]);
        let grep_tree = FileTree::directory(vec![
            (
                Bytes::from_static(b"bin"),
                FileTree::directory(
                    std::iter::once((Bytes::from_static(b"grep"), FileTree::regular(64472, true)))
                        .chain(filler.iter().cloned())
                        .collect(),
                ),
            ),
            (
                Bytes::from_static(b"lib"),
                FileTree::directory(vec![(
                    Bytes::from_static(b"libgrep.a"),
                    FileTree::regular(64472, false),
                )]),
            ),
            (
                Bytes::from_static(b"share"),
                FileTree::directory(vec![(
                    Bytes::from_static(b"man"),
                    FileTree::directory(vec![(
                        Bytes::from_static(b"man1"),
                        FileTree::directory(vec![(
                            Bytes::from_static(b"grep.1"),
                            FileTree::regular(64472, false),
                        )]),
                    )]),
                )]),
            ),
        ]);

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&hello, &hello_tree, b"").expect("add");
            writer.add(&coreutils, &coreutils_tree, b"").expect("add");
            writer.add(&grep, &grep_tree, b"").expect("add");
            writer.finish().expect("finish");
        }

        generate_sidecars(&db_path).expect("sidecars");

        let reader = Reader::open(&db_path).expect("reader");
        assert!(
            reader.frame_starts.is_some(),
            "expected frame_starts sidecar"
        );

        let normalize = |results: Vec<(StorePath, FileTreeEntry)>| -> Vec<(String, Vec<u8>)> {
            let mut v: Vec<(String, Vec<u8>)> = results
                .into_iter()
                .map(|(sp, e)| (sp.name().to_string(), e.path.to_vec()))
                .collect();
            v.sort();
            v
        };

        for pat in [
            "bin", "lib", "share", "hello", "grep", "man1", "README", "/bin", "libc", "hello.a",
        ] {
            // Full-scan baseline: every entry whose path contains the literal substring.
            let re = Regex::new(&regex::escape(pat)).expect("regex");
            let baseline = reader
                .search_entries(&re, None, None, None, None)
                .expect("baseline search");

            // Sidecar-pruned equivalent via `search_results`.
            let options = SearchOptions {
                database: dir.path().to_path_buf(),
                pattern: regex::escape(pat),
                hash: None,
                package_pattern: None,
                exact_basename: None,
                exact_path: None,
                path_prefix: None,
                literal_pattern: Some(pat.into()),
                file_type: &[],
                mode: SearchMode::Minimal,
                json: false,
                limit: None,
                count: false,
                sort: SearchSort::None,
                min_size: None,
                max_size: None,
                exclude_fhs: false,
            };
            let pruned = search_results(&options).expect("sidecar search");

            assert_eq!(
                normalize(pruned),
                normalize(baseline),
                "sidecar pruning diverged from full scan for literal {pat:?}"
            );
        }
    }

    #[test]
    fn entry_index_exact_basename_matches_full_scan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let hello = sample_store_path();
        let hello2 = StorePath::new(
            "/nix/store".into(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "hello-2.13".into(),
            Origin {
                attr: "hello".into(),
                output: "out".into(),
                toplevel: true,
                system: Some("x86_64-linux".into()),
            },
        );
        let tree = sample_tree();

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&hello, &tree, b"").expect("add");
            writer.add(&hello2, &tree, b"").expect("add");
            writer.finish().expect("finish");
        }

        generate_sidecars(&db_path).expect("sidecars");

        let reader = Reader::open(&db_path).expect("reader");
        let re = Regex::new(&(regex::escape("hello") + "$")).expect("regex");
        let baseline = reader
            .search_entries(&re, None, None, None, None)
            .expect("baseline");

        let options = SearchOptions {
            database: dir.path().to_path_buf(),
            pattern: "hello$".into(),
            hash: None,
            package_pattern: None,
            exact_basename: Some("hello".into()),
            exact_path: None,
            path_prefix: None,
            literal_pattern: None,
            file_type: &[],
            mode: SearchMode::Minimal,
            json: false,
            limit: None,
            count: false,
            sort: SearchSort::None,
            min_size: None,
            max_size: None,
            exclude_fhs: false,
        };
        let pruned = search_results(&options).expect("entry-index search");

        let normalize = |results: Vec<(StorePath, FileTreeEntry)>| -> Vec<(String, Vec<u8>)> {
            let mut v: Vec<(String, Vec<u8>)> = results
                .into_iter()
                .map(|(sp, e)| (sp.name().to_string(), e.path.to_vec()))
                .collect();
            v.sort();
            v
        };

        assert_eq!(
            normalize(pruned),
            normalize(baseline),
            "entry-index exact basename diverged from full scan"
        );
    }

    #[test]
    fn fast_path_fallbacks_for_regex_and_short_literals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let make_sp = |hash: &str, name: &str, attr: &str| {
            StorePath::new(
                "/nix/store".into(),
                hash.into(),
                name.into(),
                Origin {
                    attr: attr.into(),
                    output: "out".into(),
                    toplevel: true,
                    system: Some("x86_64-linux".into()),
                },
            )
        };
        let hello = make_sp(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "hello-2.12",
            "hello",
        );
        let hello_tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(vec![(
                Bytes::from_static(b"hello"),
                FileTree::regular(64472, true),
            )]),
        )]);

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&hello, &hello_tree, b"").expect("add");
            writer.finish().expect("finish");
        }

        generate_sidecars(&db_path).expect("sidecars");

        let reader = Reader::open(&db_path).expect("reader");

        let normalize = |results: Vec<(StorePath, FileTreeEntry)>| -> Vec<(String, Vec<u8>)> {
            let mut v: Vec<(String, Vec<u8>)> = results
                .into_iter()
                .map(|(sp, e)| (sp.name().to_string(), e.path.to_vec()))
                .collect();
            v.sort();
            v
        };

        // Regex: path trigram fast path is skipped, falls back to frcode.
        let regex_options = SearchOptions {
            database: dir.path().to_path_buf(),
            pattern: "b.n".into(),
            hash: None,
            package_pattern: None,
            exact_basename: None,
            exact_path: None,
            path_prefix: None,
            literal_pattern: None,
            file_type: &[],
            mode: SearchMode::Minimal,
            json: false,
            limit: None,
            count: false,
            sort: SearchSort::None,
            min_size: None,
            max_size: None,
            exclude_fhs: false,
        };
        let regex_baseline = reader
            .search_entries(&Regex::new("b.n").expect("regex"), None, None, None, None)
            .expect("baseline");
        let regex_pruned = search_results(&regex_options).expect("regex search");
        assert_eq!(
            normalize(regex_pruned),
            normalize(regex_baseline),
            "regex query diverged from full scan"
        );

        // Short literal (<3 bytes): path trigram returns None, falls back.
        let short_options = SearchOptions {
            database: dir.path().to_path_buf(),
            pattern: "he".into(),
            literal_pattern: Some("he".into()),
            ..regex_options.clone()
        };
        let short_baseline = reader
            .search_entries(&Regex::new("he").expect("regex"), None, None, None, None)
            .expect("baseline");
        let short_pruned = search_results(&short_options).expect("short literal search");
        assert_eq!(
            normalize(short_pruned),
            normalize(short_baseline),
            "short literal query diverged from full scan"
        );
    }

    #[test]
    fn missing_path_trigram_sidecars_fall_back_to_frcode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");
        let path = sample_store_path();
        let tree = sample_tree();

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&path, &tree, b"").expect("add");
            writer.finish().expect("finish");
        }

        generate_sidecars(&db_path).expect("sidecars");

        // Remove the new path-level sidecars so the fast paths cannot run.
        let dir = db_path.parent().expect("parent");
        let _ = std::fs::remove_file(dir.join(crate::path_trigram_index::FST_FILE));
        let _ = std::fs::remove_file(dir.join(crate::path_trigram_index::POSTINGS_FILE));
        let _ = std::fs::remove_file(dir.join(crate::path_entry_index::ENTRIES_FILE));
        let _ = std::fs::remove_file(dir.join(crate::path_entry_index::FST_FILE));

        let reader = Reader::open(&db_path).expect("reader");
        let re = Regex::new(&regex::escape("bin/hello")).expect("regex");
        let baseline = reader
            .search_entries(&re, None, None, None, None)
            .expect("baseline");

        let options = SearchOptions {
            database: dir.to_path_buf(),
            pattern: "bin/hello".into(),
            hash: None,
            package_pattern: None,
            exact_basename: None,
            exact_path: None,
            path_prefix: None,
            literal_pattern: Some("bin/hello".into()),
            file_type: &[],
            mode: SearchMode::Minimal,
            json: false,
            limit: None,
            count: false,
            sort: SearchSort::None,
            min_size: None,
            max_size: None,
            exclude_fhs: false,
        };
        let pruned = search_results(&options).expect("fallback search");

        let mut baseline: Vec<_> = baseline.into_iter().map(|(_, e)| e.path).collect();
        baseline.sort();
        let mut pruned: Vec<_> = pruned.into_iter().map(|(_, e)| e.path).collect();
        pruned.sort();

        assert_eq!(
            pruned, baseline,
            "missing sidecars should fall back to frcode"
        );
    }

    #[test]
    fn filter_prefix_skips_non_matching() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");
        let path = sample_store_path();
        let tree = sample_tree();

        {
            let mut writer = Writer::create(&db_path, 1).expect("create");
            // No entries under /lib → package should be omitted entirely.
            writer.add(&path, &tree, b"/lib").expect("add");
            writer.finish().expect("finish");
        }

        let reader = Reader::open(&db_path).expect("reader");
        let re = Regex::new(".*").expect("regex");
        let hits = reader
            .search_entries(&re, None, None, None, None)
            .expect("search");
        assert!(hits.is_empty());
    }

    #[test]
    fn format_grouped_numbers() {
        assert_eq!(format_grouped(0), "0");
        assert_eq!(format_grouped(12), "12");
        assert_eq!(format_grouped(1234), "1,234");
        assert_eq!(format_grouped(16_524), "16,524");
        assert_eq!(format_grouped(1_000_000), "1,000,000");
    }

    #[test]
    fn compile_search_regex_rejects_oversized_patterns() {
        let long = "a".repeat(MAX_PATTERN_BYTES + 1);
        assert!(compile_search_regex(&long, "path").is_err());
    }

    #[test]
    fn compile_search_regex_accepts_valid_patterns() {
        let re = compile_search_regex(r"bin/hello", "path").unwrap();
        assert!(re.is_match(b"/bin/hello"));
    }

    #[test]
    fn writer_builds_fst_sidecar_queryable_by_basename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let hello = sample_store_path();
        let hello_tree = sample_tree();
        let coreutils = StorePath::new(
            "/nix/store".into(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "coreutils-9.11".into(),
            Origin {
                attr: "coreutils".into(),
                output: "out".into(),
                toplevel: true,
                system: Some("x86_64-linux".into()),
            },
        );
        let coreutils_tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(vec![
                (Bytes::from_static(b"ls"), FileTree::regular(0, true)),
                (Bytes::from_static(b"cat"), FileTree::regular(0, true)),
            ]),
        )]);

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&hello, &hello_tree, b"").expect("add hello");
            writer
                .add(&coreutils, &coreutils_tree, b"")
                .expect("add coreutils");
            writer.finish().expect("finish");
        }

        assert!(dir.path().join("files.basename.fst").is_file());
        assert!(dir.path().join("files.basename.postings").is_file());
        assert!(dir.path().join("files.packages.names").is_file());

        let reader = Reader::open(&db_path).expect("reader");
        let mut hits = reader.query_fst("ls").expect("query ls");
        hits.sort();
        assert_eq!(hits, vec!["coreutils.out"]);

        let hello_hits = reader.query_fst("hello").expect("query hello");
        assert_eq!(hello_hits, vec!["hello.out"]);

        let none = reader.query_fst("missing-binary").expect("missing");
        assert!(none.is_empty());
    }

    #[test]
    fn writer_builds_path_index_queryable_by_full_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let hello = sample_store_path();
        let hello_tree = sample_tree();
        let coreutils = StorePath::new(
            "/nix/store".into(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "coreutils-9.11".into(),
            Origin {
                attr: "coreutils".into(),
                output: "out".into(),
                toplevel: true,
                system: Some("x86_64-linux".into()),
            },
        );
        let coreutils_tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(vec![
                (Bytes::from_static(b"ls"), FileTree::regular(0, true)),
                (Bytes::from_static(b"cat"), FileTree::regular(0, true)),
            ]),
        )]);

        {
            let mut writer = Writer::create(&db_path, 3).expect("create");
            writer.add(&hello, &hello_tree, b"").expect("add hello");
            writer
                .add(&coreutils, &coreutils_tree, b"")
                .expect("add coreutils");
            writer.finish().expect("finish");
        }

        assert!(dir.path().join("files.path.fst").is_file());
        assert!(dir.path().join("files.path.postings").is_file());

        let reader = Reader::open(&db_path).expect("reader");

        // Test exact path lookup
        let mut ls_ordinals = reader
            .query_path_ordinals(b"/bin/ls")
            .expect("query /bin/ls");
        ls_ordinals.sort();
        assert_eq!(ls_ordinals, vec![1]); // coreutils only

        let hello_ordinals = reader
            .query_path_ordinals(b"/bin/hello")
            .expect("query /bin/hello");
        assert_eq!(hello_ordinals, vec![0]); // hello only

        // Test prefix lookup
        let mut bin_ordinals = reader
            .query_prefix_ordinals(b"/bin/")
            .expect("query /bin/ prefix");
        bin_ordinals.sort();
        assert_eq!(bin_ordinals, vec![0, 1]); // both have files under /bin

        let missing = reader.query_path_ordinals(b"/bin/nope").expect("missing");
        assert!(missing.is_empty());
    }

    #[test]
    fn v2_writer_reader_roundtrip_and_search() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let path = sample_store_path();
        let tree = sample_tree();

        {
            let mut writer = Writer::create_with_version(&db_path, 3, 2, false).expect("create v2");
            writer.add(&path, &tree, b"").expect("add");
            let size = writer.finish().expect("finish");
            assert!(size > 0);
        }

        let reader = Reader::open(&db_path).expect("reader");
        assert_eq!(reader.version(), 2);

        let re = Regex::new(&regex::escape("bin/hello")).expect("regex");
        let hits = reader
            .search_entries(&re, None, None, None, None)
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits.first().map(|(p, _)| p.name()), Some("hello-2.12"));
        assert_eq!(
            hits.first().map(|(_, e)| e.path.as_slice()),
            Some(b"/bin/hello".as_slice())
        );

        // Sidecars are still produced on finish.
        assert!(dir.path().join("files.basename.fst").is_file());
    }

    #[test]
    fn v2_multiple_packages_yield_per_cpu_frames() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let hello = sample_store_path();
        let hello_tree = sample_tree();
        let coreutils = StorePath::new(
            "/nix/store".into(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "coreutils-9.11".into(),
            Origin {
                attr: "coreutils".into(),
                output: "out".into(),
                toplevel: true,
                system: Some("x86_64-linux".into()),
            },
        );
        let coreutils_tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(vec![
                (Bytes::from_static(b"ls"), FileTree::regular(0, true)),
                (Bytes::from_static(b"cat"), FileTree::regular(0, true)),
            ]),
        )]);

        {
            let mut writer = Writer::create_with_version(&db_path, 1, 2, false).expect("create v2");
            writer.add(&hello, &hello_tree, b"").expect("add hello");
            writer
                .add(&coreutils, &coreutils_tree, b"")
                .expect("add coreutils");
            writer.finish().expect("finish");
        }

        let reader = Reader::open(&db_path).expect("reader");
        assert_eq!(reader.version(), 2);

        let re = Regex::new(".*").expect("regex");
        let hits = reader
            .search_entries(&re, None, None, None, None)
            .expect("search");
        // Each tree also emits a synthetic root entry with an empty path.
        assert_eq!(hits.len(), 7);

        // Parse the trailing seek table and confirm the frame count respects
        // per-CPU grouping: one frame per package, but capped by available CPUs.
        let data = fs::read(&db_path).expect("read db");
        let payload_len_bytes = data
            .get(data.len() - 4..)
            .expect("last 4 bytes")
            .try_into()
            .expect("4 bytes");
        let payload_len = u32::from_le_bytes(payload_len_bytes) as usize;
        let payload_start = data.len() - payload_len;
        let frame_count_bytes = data
            .get(payload_start..payload_start + 4)
            .expect("frame count bytes")
            .try_into()
            .expect("4 bytes");
        let frame_count = u32::from_le_bytes(frame_count_bytes) as usize;
        let num_cpus = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .max(1);
        assert!(frame_count >= 1 && frame_count <= num_cpus.max(2));
    }

    #[test]
    fn v2_selective_search_by_ordinals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let hello = sample_store_path();
        let hello_tree = sample_tree();
        let coreutils = StorePath::new(
            "/nix/store".into(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            "coreutils-9.11".into(),
            Origin {
                attr: "coreutils".into(),
                output: "out".into(),
                toplevel: true,
                system: Some("x86_64-linux".into()),
            },
        );
        let coreutils_tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(vec![
                (Bytes::from_static(b"ls"), FileTree::regular(0, true)),
                (Bytes::from_static(b"cat"), FileTree::regular(0, true)),
            ]),
        )]);

        {
            let mut writer = Writer::create_with_version(&db_path, 1, 2, false).expect("create v2");
            writer.add(&hello, &hello_tree, b"").expect("add hello");
            writer
                .add(&coreutils, &coreutils_tree, b"")
                .expect("add coreutils");
            writer.finish().expect("finish");
        }

        let reader = Reader::open(&db_path).expect("reader");
        assert!(reader.frame_map.is_some());
        assert!(reader.frame_starts.is_some());

        let re = Regex::new(".*").expect("regex");
        let mut ordinals = RoaringBitmap::new();
        ordinals.insert(1u32);

        let hits = reader
            .search_entries(&re, None, None, None, Some(&ordinals))
            .expect("search");

        // coreutils yields the synthetic root plus /bin and /bin/{ls,cat}.
        assert_eq!(hits.len(), 4);
        assert!(hits.iter().all(|(p, _)| p.name() == "coreutils-9.11"));
        assert!(hits.iter().any(|(_, e)| e.path == b"/bin/ls"));
        assert!(hits.iter().any(|(_, e)| e.path == b"/bin/cat"));
    }

    #[test]
    fn v2_corrupt_seek_table_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let path = sample_store_path();
        let tree = sample_tree();

        {
            let mut writer = Writer::create_with_version(&db_path, 1, 2, false).expect("create v2");
            writer.add(&path, &tree, b"").expect("add");
            writer.finish().expect("finish");
        }

        let mut data = fs::read(&db_path).expect("read db");
        // Corrupt the trailing payload-length duplicate so it no longer matches
        // the declared frame size.
        if let Some(last) = data.last_mut() {
            *last = last.wrapping_add(1);
        }
        fs::write(&db_path, &data).expect("rewrite corrupt db");

        let err = Reader::open(&db_path).expect_err("corrupt db should fail");
        assert!(matches!(err, Error::Corrupt(_)));
    }

    #[test]
    fn v2_seek_table_smaller_than_header_plus_trailer_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        // 20-byte v2 file: 12-byte header + 4 padding bytes + payload_len=8.
        // This is too short to contain the header plus a 16-byte skippable
        // frame, so `parse_seek_table` should reject it instead of underflowing
        // `frames_end - data_start`.
        let mut data = Vec::new();
        data.extend_from_slice(FILE_MAGIC);
        data.extend_from_slice(&2u64.to_le_bytes());
        data.extend_from_slice(&[0u8; 4]);
        data.extend_from_slice(&8u32.to_le_bytes());
        fs::write(&db_path, &data).expect("write truncated db");

        let err = Reader::open(&db_path).expect_err("truncated db should fail");
        assert!(matches!(err, Error::Corrupt(_)));
    }

    #[test]
    fn add_skips_entries_with_forbidden_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("files");

        let tree = FileTree::directory(vec![(
            Bytes::from_static(b"bin"),
            FileTree::directory(vec![
                (Bytes::from_static(b"hello"), FileTree::regular(64472, true)),
                // Symlink target containing a newline is invalid in frcode.
                (
                    Bytes::from_static(b"broken"),
                    FileTree::symlink(Bytes::from_static(b"/etc/passwd\n")),
                ),
            ]),
        )]);

        let mut writer = Writer::create(&db_path, 1).expect("create");
        writer.add(&sample_store_path(), &tree, b"").expect("add");
        writer.finish().expect("finish");

        let reader = Reader::open(&db_path).expect("reader");

        let broken_re = Regex::new("broken").expect("regex");
        let broken_hits = reader
            .search_entries(&broken_re, None, None, None, None)
            .expect("search");
        assert!(
            broken_hits.is_empty(),
            "symlink with newline target should be skipped"
        );

        let hello_re = Regex::new("hello").expect("regex");
        let hello_hits = reader
            .search_entries(&hello_re, None, None, None, None)
            .expect("search");
        assert_eq!(hello_hits.len(), 1);
        assert_eq!(
            hello_hits.first().map(|(_, e)| e.path.as_slice()),
            Some(b"/bin/hello".as_slice())
        );
    }
}
