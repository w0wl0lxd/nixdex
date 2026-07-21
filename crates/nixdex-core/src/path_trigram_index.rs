//! Path-level trigram inverted index: FST map + postings of `path-id`s.
//!
//! Sidecar layout (siblings of the NIXI `files` database):
//! - `files.path-trigram.fst`      — [`fst::Map`] from a raw 3-byte trigram → cookie (`u64`)
//! - `files.path-trigram.postings` — cookie → packed `u32` path-id list
//!
//! For a literal query pattern we extract every overlapping 3-byte window
//! (trigram), look up each trigram's posting list, and intersect (AND) them to
//! obtain the candidate path-ids. An empty intersection means the pattern cannot
//! match any path.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use fst::Map;
use indexmap::IndexSet;
use mmap_guard;
use roaring::RoaringBitmap;
use thiserror::Error;

/// Magic for the postings blob.
const POSTINGS_MAGIC: &[u8] = b"NNPT";
/// Sidecar format version.
const SIDE_VERSION: u32 = 1;

/// Maximum number of path-ids stored for a single trigram (defensive cap).
const MAX_IDS_PER_TRIGRAM: usize = 20_000_000;

/// Maximum total size of the postings sidecar (defensive cap).
const MAX_POSTINGS_BYTES: usize = 8 * 1024 * 1024 * 1024;

/// Maximum total size of the FST sidecar (defensive cap).
const MAX_FST_BYTES: usize = 512 << 20;

/// Sidecar basename for the trigram FST.
pub const FST_FILE: &str = "files.path-trigram.fst";
/// Postings table filename.
pub const POSTINGS_FILE: &str = "files.path-trigram.postings";

/// Errors while building or querying the path-level trigram index.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Secondary index files are missing from the database directory.
    #[error("path trigram index missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("path trigram index corrupt: {0}")]
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

/// Accumulates trigram → path-id mappings as paths are assigned ids.
#[derive(Debug, Default)]
pub struct PathTrigramIndexBuilder {
    /// trigram (`[u8; 3]`) → roaring bitmap of path-ids.
    trigrams: BTreeMap<[u8; 3], RoaringBitmap>,
}

impl PathTrigramIndexBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record all trigrams from `path` as belonging to `path_id`.
    ///
    /// Paths shorter than 3 bytes are skipped.
    pub fn record_path(&mut self, path_id: u32, path: &[u8]) -> Result<()> {
        if path.len() < 3 {
            return Ok(());
        }
        for i in 0..path.len().saturating_sub(2) {
            let trigram: [u8; 3] = match (path.get(i), path.get(i + 1), path.get(i + 2)) {
                (Some(a), Some(b), Some(c)) => (*a, *b, *c).into(),
                _ => continue,
            };
            self.trigrams.entry(trigram).or_default().insert(path_id);
        }
        Ok(())
    }

    /// Write sidecar files into `db_dir` (the directory that holds `files`).
    pub fn write_sidecars(&self, db_dir: &Path) -> Result<()> {
        self.write_sidecars_inner(db_dir)
    }

    fn write_sidecars_inner(&self, db_dir: &Path) -> Result<()> {
        let mut raw = Vec::new();
        raw.extend_from_slice(POSTINGS_MAGIC);
        raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());

        let mut cookies: Vec<([u8; 3], u64)> = Vec::with_capacity(self.trigrams.len());
        for (trigram, bitmap) in &self.trigrams {
            let cookie = u64::try_from(raw.len())
                .map_err(|_| Error::Corrupt("postings cookie overflow".into()))?;
            let mut ids: Vec<u32> = bitmap.iter().collect();
            ids.sort_unstable();
            ids.dedup();
            let count = u32::try_from(ids.len())
                .map_err(|_| Error::Corrupt("too many ids for one trigram".into()))?;
            raw.extend_from_slice(&count.to_le_bytes());
            for id in ids {
                raw.extend_from_slice(&id.to_le_bytes());
            }
            cookies.push((*trigram, cookie));
        }
        std::fs::write(db_dir.join(POSTINGS_FILE), &raw)?;

        let mut builder = fst::MapBuilder::memory();
        for (trigram, cookie) in &cookies {
            builder
                .insert(&trigram[..], *cookie)
                .map_err(|err| Error::Fst(err.to_string()))?;
        }
        let fst_bytes = builder
            .into_inner()
            .map_err(|err| Error::Fst(err.to_string()))?;
        std::fs::write(db_dir.join(FST_FILE), fst_bytes)?;
        Ok(())
    }
}

/// Opened path-level trigram index for literal candidate path-id queries.
#[derive(Debug)]
pub struct PathTrigramIndex {
    // FileData implements AsRef<[u8]>, so fst::Map can use it directly.
    map: Map<mmap_guard::FileData>,
    postings: mmap_guard::FileData,
}

impl PathTrigramIndex {
    /// Open sidecars from a database directory.
    pub fn open(db_dir: &Path) -> Result<Self> {
        Self::open_inner(db_dir)
    }

    fn open_inner(db_dir: &Path) -> Result<Self> {
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

    /// Candidate path-ids for a LITERAL pattern.
    ///
    /// Returns `Ok(None)` if the pattern is shorter than 3 bytes or looks like a
    /// regex. Otherwise returns `Some(bitmap)`. An empty bitmap means no matches.
    pub fn candidate_path_ids(&self, pattern: &str) -> Result<Option<RoaringBitmap>> {
        self.candidate_path_ids_inner(pattern)
    }

    fn candidate_path_ids_inner(&self, pattern: &str) -> Result<Option<RoaringBitmap>> {
        let bytes = pattern.as_bytes();

        if bytes.len() < 3 {
            return Ok(None);
        }

        if bytes.iter().any(|&b| {
            matches!(
                b,
                b'.' | b'*'
                    | b'+'
                    | b'?'
                    | b'('
                    | b')'
                    | b'['
                    | b']'
                    | b'{'
                    | b'}'
                    | b'|'
                    | b'^'
                    | b'$'
                    | b'\\'
            )
        }) {
            return Ok(None);
        }

        let mut cookies: Vec<u64> = Vec::new();
        let mut seen = IndexSet::new();
        for i in 0..bytes.len().saturating_sub(2) {
            let trigram: [u8; 3] = match (bytes.get(i), bytes.get(i + 1), bytes.get(i + 2)) {
                (Some(a), Some(b), Some(c)) => (*a, *b, *c).into(),
                _ => continue,
            };
            if seen.insert(trigram) {
                match self.map.get(trigram) {
                    Some(cookie) => cookies.push(cookie),
                    None => return Ok(Some(RoaringBitmap::new())),
                }
            }
        }

        if cookies.is_empty() {
            return Ok(Some(RoaringBitmap::new()));
        }

        let mut lists: Vec<RoaringBitmap> = Vec::with_capacity(cookies.len());
        for cookie in &cookies {
            let ids = read_ids_at(&self.postings, *cookie)?;
            let mut bm = RoaringBitmap::new();
            bm.extend(ids);
            lists.push(bm);
        }

        lists.sort_by_key(RoaringBitmap::len);

        let mut acc = lists.remove(0);
        for bm in lists {
            acc &= bm;
        }
        Ok(Some(acc))
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
            "postings magic {magic:?}, expected {POSTINGS_MAGIC:?}"
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

fn read_ids_at(postings: &[u8], cookie: u64) -> Result<Vec<u32>> {
    let start = usize::try_from(cookie)
        .map_err(|_| Error::Corrupt(format!("cookie {cookie} does not fit usize")))?;
    let count = usize::try_from(read_u32_le(postings, start)?)
        .map_err(|_| Error::Corrupt("id count too large".into()))?;
    if count > MAX_IDS_PER_TRIGRAM {
        return Err(Error::Corrupt(format!(
            "too many ids for one trigram: {count} (max {MAX_IDS_PER_TRIGRAM})"
        )));
    }
    let body = start
        .checked_add(4)
        .ok_or_else(|| Error::Corrupt("id body offset overflow".into()))?;
    let need = count
        .checked_mul(4)
        .and_then(|b| body.checked_add(b))
        .ok_or_else(|| Error::Corrupt("id list size overflow".into()))?;
    if need > postings.len() {
        return Err(Error::Corrupt(format!(
            "id list truncated: need {need}, have {}",
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
    fn build_and_intersect() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = PathTrigramIndexBuilder::new();

        let paths: Vec<Vec<u8>> = vec![
            b"/bin/hello".to_vec(),
            b"/bin/ls".to_vec(),
            b"/lib/libc.so".to_vec(),
            b"/nix/store/abc/bin/hello".to_vec(),
        ];

        for (id, path) in paths.iter().enumerate() {
            builder.record_path(id as u32, path).expect("record");
        }

        builder.write_sidecars(dir.path()).expect("write");

        let index = PathTrigramIndex::open(dir.path()).expect("open");

        let ids = index
            .candidate_path_ids("bin/hello")
            .expect("lookup")
            .expect("some");
        assert_eq!(ids.iter().collect::<Vec<_>>(), vec![0, 3]);

        let no_match = index
            .candidate_path_ids("zzzz")
            .expect("lookup")
            .expect("some");
        assert!(no_match.is_empty());
    }
}
