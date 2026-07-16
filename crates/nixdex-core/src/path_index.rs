//! Full-path secondary index: FST map + postings of package ordinals.
//!
//! Sidecar layout (siblings of the NIXI `files` database):
//! - `files.path.fst` — [`fst::Map`] from full path bytes → cookie (`u64`)
//! - `files.path.postings` — cookie points at a packed ordinal list
//! - `files.packages.names` — ordinal → package label (reused from basename index)
//!
//! This index enables fast prefix/rooted queries like `/bin/ls` or `/lib/libfoo.so`
//! without scanning the full database.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use byteorder::LittleEndian;
use fst::{Map, Streamer};
use mmap_guard;
use roaring::RoaringBitmap;
use thiserror::Error;

/// Magic for the postings blob.
const POSTINGS_MAGIC: &[u8] = b"NPPO";
/// Sidecar format version.
const SIDE_VERSION: u32 = 1;

/// Maximum total size of the postings sidecar (defensive cap).
const MAX_POSTINGS_BYTES: usize = 1024 * 1024 * 1024;

/// Maximum number of package ordinals returned for a single path.
///
/// This bounds the allocation in `read_ordinals_at` to a few megabytes per
/// lookup instead of the full postings file size.
const MAX_ORDINALS_PER_PATH: usize = 1_000_000;

/// Maximum total size of the FST sidecar (defensive cap).
const MAX_FST_BYTES: usize = 512 * 1024 * 1024;

/// Sidecar basenames relative to the database directory.
pub const FST_FILE: &str = "files.path.fst";
/// Postings table filename.
pub const POSTINGS_FILE: &str = "files.path.postings";

/// Errors while building or querying the path secondary index.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Secondary index files are missing from the database directory.
    #[error("path secondary index missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("path secondary index corrupt: {0}")]
    Corrupt(String),

    /// FST crate reported a build or query error.
    #[error("fst error: {0}")]
    Fst(String),

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Accumulates full path → package ordinal mappings while a NIXI database is written.
#[derive(Debug, Default)]
pub struct PathIndexBuilder {
    /// full path → package ordinals (may contain many packages for common paths).
    paths: BTreeMap<Vec<u8>, RoaringBitmap>,
}

impl PathIndexBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one package and every full path from its absolute paths.
    ///
    /// The ordinal is assigned by the caller (from BasenameIndexBuilder) to ensure
    /// consistency across both indices.
    pub fn record_package(
        &mut self,
        ordinal: u32,
        paths: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<()> {
        for path in paths {
            if path.is_empty() {
                continue;
            }
            self.paths.entry(path).or_default().insert(ordinal);
        }
        Ok(())
    }

    /// Write sidecar files into `db_dir` (the directory that holds `files`).
    ///
    /// # Errors
    ///
    /// Returns an error if any sidecar cannot be written or the FST build fails.
    pub fn write_sidecars(&self, db_dir: &Path) -> Result<()> {
        let mut raw = Vec::new();
        raw.extend_from_slice(POSTINGS_MAGIC);
        raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());

        // BTreeMap keeps paths sorted — required by `fst::MapBuilder`.
        let mut cookies: Vec<(Vec<u8>, u64)> = Vec::with_capacity(self.paths.len());
        for (path, bitmap) in &self.paths {
            let cookie = u64::try_from(raw.len())
                .map_err(|_| Error::Corrupt("postings cookie overflow".into()))?;
            let mut ordinals: Vec<u32> = bitmap.iter().collect();
            ordinals.sort_unstable();
            ordinals.dedup();
            let count = u32::try_from(ordinals.len())
                .map_err(|_| Error::Corrupt("too many ordinals for one path".into()))?;
            raw.extend_from_slice(&count.to_le_bytes());
            for o in ordinals {
                raw.extend_from_slice(&o.to_le_bytes());
            }
            cookies.push((path.clone(), cookie));
        }
        std::fs::write(db_dir.join(POSTINGS_FILE), &raw)?;

        let mut builder = fst::MapBuilder::memory();
        for (path, cookie) in &cookies {
            builder
                .insert(path, *cookie)
                .map_err(|err| Error::Fst(err.to_string()))?;
        }
        let fst_bytes = builder
            .into_inner()
            .map_err(|err| Error::Fst(err.to_string()))?;
        std::fs::write(db_dir.join(FST_FILE), fst_bytes)?;
        Ok(())
    }
}

/// Opened path secondary index for prefix/full-path queries.
#[derive(Debug)]
pub struct PathIndex {
    // FileData implements AsRef<[u8]>, so fst::Map can use it directly.
    map: Map<mmap_guard::FileData>,
    postings: mmap_guard::FileData,
}

impl PathIndex {
    /// Open sidecars from a database directory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Missing`] when any required sidecar is absent, or
    /// [`Error::Corrupt`] / [`Error::Fst`] when the files cannot be parsed.
    pub fn open(db_dir: &Path) -> Result<Self> {
        let fst_path = db_dir.join(FST_FILE);
        let postings_path = db_dir.join(POSTINGS_FILE);

        if !fst_path.is_file() {
            return Err(Error::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {FST_FILE}"),
            });
        }
        if !postings_path.is_file() {
            return Err(Error::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {POSTINGS_FILE}"),
            });
        }

        let fst_data = mmap_guard::map_file(&fst_path).map_err(Error::Io)?;
        if fst_data.len() > MAX_FST_BYTES {
            return Err(Error::Corrupt("fst file too large".into()));
        }
        let map = Map::new(fst_data).map_err(|err| Error::Fst(err.to_string()))?;

        let postings = mmap_guard::map_file(&postings_path).map_err(Error::Io)?;
        if postings.len() > MAX_POSTINGS_BYTES {
            return Err(Error::Corrupt("postings file too large".into()));
        }
        validate_postings_header(&postings)?;

        Ok(Self { map, postings })
    }

    /// Look up raw package ordinals that contain an exact full path.
    ///
    /// Returns an empty list when the path is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when postings for a present FST key are corrupt.
    pub fn lookup_path_ordinals(&self, path: &[u8]) -> Result<Vec<u32>> {
        let Some(cookie) = self.map.get(path) else {
            return Ok(Vec::new());
        };
        read_ordinals_at(&self.postings, cookie)
    }

    /// Look up package ordinals for paths that start with the given prefix.
    ///
    /// Returns ordinals for all paths where the path bytes start with `prefix`.
    /// This is useful for rooted queries like `/bin/` to find all files under `/bin`.
    ///
    /// Returns an empty list when no paths match the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error when the FST stream operation fails or postings are corrupt.
    pub fn lookup_prefix_ordinals(&self, prefix: &[u8]) -> Result<Vec<u32>> {
        let mut matched = RoaringBitmap::new();
        let mut stream = self.map.stream();

        while let Some((key, cookie)) = stream.next() {
            if key.starts_with(prefix) {
                let ordinals = read_ordinals_at(&self.postings, cookie)?;
                for ord in ordinals {
                    matched.insert(ord);
                }
            }
        }

        Ok(matched.iter().collect())
    }
}

fn read_u32_le(bytes: &[u8], at: usize) -> Result<u32> {
    let end = at
        .checked_add(4)
        .ok_or_else(|| Error::Corrupt("u32 offset overflow".into()))?;
    let slice = bytes
        .get(at..end)
        .ok_or_else(|| Error::Corrupt(format!("u32 read past end at {at}")))?;
    let arr: [u8; 4] = slice
        .try_into()
        .map_err(|_| Error::Corrupt("u32 slice length".into()))?;
    Ok(u32::from_le_bytes(arr))
}

fn validate_postings_header(postings: &[u8]) -> Result<()> {
    if postings.len() < POSTINGS_MAGIC.len() + 4 {
        return Err(Error::Corrupt("postings too short".into()));
    }
    let magic = postings
        .get(..POSTINGS_MAGIC.len())
        .ok_or_else(|| Error::Corrupt("postings too short for magic".into()))?;
    if magic != POSTINGS_MAGIC {
        return Err(Error::Corrupt(format!(
            "postings magic {magic:?}, expected {:?}",
            POSTINGS_MAGIC
        )));
    }
    let ver = read_u32_le(postings, POSTINGS_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(Error::Corrupt(format!(
            "postings version {ver}, expected {SIDE_VERSION}"
        )));
    }
    Ok(())
}

fn read_ordinals_at(postings: &[u8], cookie: u64) -> Result<Vec<u32>> {
    let start = usize::try_from(cookie)
        .map_err(|_| Error::Corrupt(format!("cookie {cookie} does not fit usize")))?;
    let count = usize::try_from(read_u32_le(postings, start)?)
        .map_err(|_| Error::Corrupt("ordinal count too large".into()))?;
    if count > MAX_ORDINALS_PER_PATH {
        return Err(Error::Corrupt(format!(
            "too many ordinals for one path: {count} (max {MAX_ORDINALS_PER_PATH})"
        )));
    }
    let body = start
        .checked_add(4)
        .ok_or_else(|| Error::Corrupt("ordinal body offset overflow".into()))?;
    let need = count
        .checked_mul(4)
        .and_then(|b| body.checked_add(b))
        .ok_or_else(|| Error::Corrupt("ordinal list size overflow".into()))?;
    if need > postings.len() {
        return Err(Error::Corrupt(format!(
            "ordinal list truncated: need {need}, have {}",
            postings.len()
        )));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = body + i * 4;
        out.push(read_u32_le(postings, off)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_query_exact_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = PathIndexBuilder::new();
        builder
            .record_package(0, vec![b"/bin/ls".to_vec(), b"/bin/cat".to_vec()])
            .expect("pkg0");
        builder
            .record_package(1, vec![b"/bin/hello".to_vec()])
            .expect("pkg1");
        builder
            .record_package(2, vec![b"/bin/ls".to_vec()])
            .expect("pkg2");
        builder.write_sidecars(dir.path()).expect("write");

        let index = PathIndex::open(dir.path()).expect("open");

        let mut ls_ordinals = index.lookup_path_ordinals(b"/bin/ls").expect("ls ordinals");
        ls_ordinals.sort();
        assert_eq!(ls_ordinals, vec![0, 2]); // pkg0, pkg2

        let hello_ordinals = index
            .lookup_path_ordinals(b"/bin/hello")
            .expect("hello ordinals");
        assert_eq!(hello_ordinals, vec![1]);

        let missing = index.lookup_path_ordinals(b"/bin/nope").expect("nope");
        assert!(missing.is_empty());
    }

    #[test]
    fn build_and_query_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = PathIndexBuilder::new();
        builder
            .record_package(0, vec![b"/bin/ls".to_vec(), b"/bin/cat".to_vec()])
            .expect("pkg0");
        builder
            .record_package(1, vec![b"/lib/libfoo.so".to_vec()])
            .expect("pkg1");
        builder.write_sidecars(dir.path()).expect("write");

        let index = PathIndex::open(dir.path()).expect("open");

        let bin_ordinals = index.lookup_prefix_ordinals(b"/bin/").expect("bin prefix");
        bin_ordinals.sort();
        assert_eq!(bin_ordinals, vec![0]);

        let lib_ordinals = index.lookup_prefix_ordinals(b"/lib/").expect("lib prefix");
        assert_eq!(lib_ordinals, vec![1]);

        let empty = index
            .lookup_prefix_ordinals(b"/nope/")
            .expect("nope prefix");
        assert!(empty.is_empty());
    }

    #[test]
    fn open_missing_sidecar_is_missing_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = PathIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Missing { .. }));
    }
}
