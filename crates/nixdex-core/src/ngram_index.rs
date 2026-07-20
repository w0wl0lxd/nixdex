//! Trigram (n=3) inverted path index: FST map + postings of package ordinals.
//!
//! Sidecar layout (siblings of the NIXI `files` database):
//! - `files.ngram.fst` — [`fst::Map`] from a raw 3-byte trigram → cookie (`u64`)
//! - `files.ngram.postings` — cookie → `u32` byte length + native
//!   [`roaring::RoaringBitmap`] (format v2). Legacy v1 stored a raw `u32`
//!   ordinal list and is still readable via the same query path.
//!
//! For a literal query pattern we extract every overlapping 3-byte window
//! (trigram), look up each trigram's posting list, and intersect (AND) them to
//! obtain the candidate package ordinals. An empty intersection means the
//! pattern cannot match any package. This is the zoekt/livegrep/Russ-Cox
//! trigram-candidate technique.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use fst::Map;
use indexmap::IndexSet;
use mmap_guard;
use roaring::RoaringBitmap;
use thiserror::Error;

/// Magic for the postings blob.
const POSTINGS_MAGIC: &[u8] = b"NNPO";
/// Current sidecar format version (postings store native roaring bitmaps).
const SIDE_VERSION: u32 = 2;
/// Legacy sidecar format version (postings stored raw `u32` ordinal lists).
const SIDE_VERSION_V1: u32 = 1;

/// Maximum number of package ordinals stored for a single trigram (defensive cap, v1 path).
///
/// Bounds the allocation in `read_ordinals_at` to a few megabytes per lookup.
const MAX_ORDINALS_PER_TRIGRAM: usize = 2_000_000;

/// Maximum serialized size of a single trigram roaring bitmap (defensive cap, v2 path).
const MAX_SERIALIZED_BYTES: usize = 1 << 28;

/// Maximum total size of the postings sidecar (defensive cap).
const MAX_POSTINGS_BYTES: usize = 1 << 30;

/// Maximum total size of the FST sidecar (defensive cap).
const MAX_FST_BYTES: usize = 512 << 20;

/// Sidecar basename for the trigram FST.
pub const FST_FILE: &str = "files.ngram.fst";
/// Postings table filename.
pub const POSTINGS_FILE: &str = "files.ngram.postings";

/// Errors while building or querying the trigram secondary index.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Secondary index files are missing from the database directory.
    #[error("ngram secondary index missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("ngram secondary index corrupt: {0}")]
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

/// Accumulates trigram → package ordinal mappings while a NIXI database is written.
#[derive(Debug, Default)]
pub struct NgramIndexBuilder {
    /// trigram (`[u8; 3]`) → roaring bitmap of package ordinals.
    trigrams: BTreeMap<[u8; 3], RoaringBitmap>,
}

impl NgramIndexBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// For each path, extract all overlapping 3-byte windows; insert `ordinal`
    /// into the posting list of every trigram. Paths shorter than 3 bytes are skipped.
    ///
    /// # Errors
    ///
    /// Always returns `Ok(())` for the in-memory accumulation; errors surface at
    /// [`write_sidecars`](Self::write_sidecars). Retained for API symmetry.
    pub fn record_package(
        &mut self,
        ordinal: u32,
        paths: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<()> {
        for path in paths {
            if path.len() < 3 {
                continue;
            }
            let len = path.len();
            for i in 0..len.saturating_sub(2) {
                let trigram: [u8; 3] = match (path.get(i), path.get(i + 1), path.get(i + 2)) {
                    (Some(a), Some(b), Some(c)) => (*a, *b, *c).into(),
                    _ => continue,
                };
                self.trigrams.entry(trigram).or_default().insert(ordinal);
            }
        }
        Ok(())
    }

    /// Write sidecar files into `db_dir` (the directory that holds `files`).
    ///
    /// # Errors
    ///
    /// Returns an error if any sidecar cannot be written or the FST build fails.
    pub fn write_sidecars(&self, db_dir: &Path) -> Result<()> {
        self.write_sidecars_inner(db_dir)
    }

    fn write_sidecars_inner(&self, db_dir: &Path) -> Result<()> {
        let mut raw = Vec::new();
        raw.extend_from_slice(POSTINGS_MAGIC);
        raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());

        // BTreeMap keeps trigrams sorted — required by `fst::MapBuilder`.
        let mut cookies: Vec<([u8; 3], u64)> = Vec::with_capacity(self.trigrams.len());
        for (trigram, bitmap) in &self.trigrams {
            let cookie = u64::try_from(raw.len())
                .map_err(|_| Error::Corrupt("postings cookie overflow".into()))?;
            let mut buf = Vec::new();
            bitmap
                .serialize_into(&mut buf)
                .map_err(|e| Error::Corrupt(format!("serialize trigram bitmap: {e}")))?;
            if buf.len() > MAX_SERIALIZED_BYTES {
                return Err(Error::Corrupt("trigram bitmap too large".into()));
            }
            let len = u32::try_from(buf.len())
                .map_err(|_| Error::Corrupt("trigram bitmap too large".into()))?;
            raw.extend_from_slice(&len.to_le_bytes());
            raw.extend_from_slice(&buf);
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

/// Opened trigram secondary index for literal candidate-ordinal queries.
#[derive(Debug)]
pub struct NgramIndex {
    // FileData implements AsRef<[u8]>, so fst::Map can use it directly.
    map: Map<mmap_guard::FileData>,
    postings: mmap_guard::FileData,
    /// Postings format version this sidecar was written with (v1 or v2).
    version: u32,
}

impl NgramIndex {
    /// Open sidecars from a database directory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Missing`] when any required sidecar is absent, or
    /// [`Error::Corrupt`] / [`Error::Fst`] when the files cannot be parsed.
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
        let version = read_postings_version(&postings)?;

        Ok(Self {
            map,
            postings,
            version,
        })
    }

    /// Candidate package ordinals for a LITERAL pattern.
    ///
    /// Returns `Ok(None)` if the pattern is shorter than 3 bytes (no trigram) or
    /// looks like a regex (contains `. * + ? ( ) [ ] { } | ^ $ \`). Otherwise
    /// extracts the pattern's trigrams, intersects their posting lists (AND), and
    /// returns `Some(bitmap)`. An empty intersection yields an empty bitmap
    /// (meaning: matches nothing).
    ///
    /// # Errors
    ///
    /// Returns an error when postings for a present FST key are corrupt.
    pub fn candidate_ordinals(&self, pattern: &str) -> Result<Option<RoaringBitmap>> {
        self.candidate_ordinals_inner(pattern)
    }

    fn candidate_ordinals_inner(&self, pattern: &str) -> Result<Option<RoaringBitmap>> {
        let bytes = pattern.as_bytes();

        // Too short to have a trigram.
        if bytes.len() < 3 {
            return Ok(None);
        }

        // Regex-like characters disqualify the pattern as a literal.
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

        // Collect all distinct trigrams of the pattern, recording each posting's
        // leading `u32` header (v1: ordinal count, v2: serialized byte length) so
        // we can intersect smallest-first without materializing every list up front.
        let mut entries: Vec<(u64, u32)> = Vec::new();
        let mut seen = IndexSet::new();
        for i in 0..bytes.len().saturating_sub(2) {
            let trigram: [u8; 3] = match (bytes.get(i), bytes.get(i + 1), bytes.get(i + 2)) {
                (Some(a), Some(b), Some(c)) => (*a, *b, *c).into(),
                _ => continue,
            };
            if seen.insert(trigram) {
                match self.map.get(trigram) {
                    Some(cookie) => {
                        let at = usize::try_from(cookie).map_err(|_| {
                            Error::Corrupt(format!("cookie {cookie} does not fit usize"))
                        })?;
                        let header = read_u32_le(&self.postings, at)?;
                        entries.push((cookie, header));
                    }
                    // A trigram absent from the index means nothing can match.
                    None => return Ok(Some(RoaringBitmap::new())),
                }
            }
        }

        if entries.is_empty() {
            // Pattern has only repeating trigrams (e.g. "aaa"), none in the index.
            return Ok(Some(RoaringBitmap::new()));
        }

        // Intersect the smallest posting first; short-circuit once the candidate
        // set becomes empty (nothing can match).
        entries.sort_by_key(|&(_, header)| header);
        let mut acc = read_bitmap_at(&self.postings, entries.remove(0).0, self.version)?;
        for (cookie, _) in entries {
            let bm = read_bitmap_at(&self.postings, cookie, self.version)?;
            acc &= bm;
            if acc.is_empty() {
                break;
            }
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

fn read_postings_version(postings: &[u8]) -> Result<u32> {
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
    if ver != SIDE_VERSION && ver != SIDE_VERSION_V1 {
        return Err(Error::Corrupt(format!(
            "unsupported postings version {ver}"
        )));
    }
    Ok(ver)
}

/// Decode the roaring posting bitmap stored at `cookie` for the given format `version`.
///
/// v1 postings store a raw `u32` ordinal list; v2 postings store the bitmap's
/// native serialization. Both yield an equivalent [`RoaringBitmap`].
fn read_bitmap_at(postings: &[u8], cookie: u64, version: u32) -> Result<RoaringBitmap> {
    if version == SIDE_VERSION_V1 {
        let ordinals = read_ordinals_at(postings, cookie)?;
        let mut bm = RoaringBitmap::new();
        bm.extend(ordinals);
        return Ok(bm);
    }

    let start = usize::try_from(cookie)
        .map_err(|_| Error::Corrupt(format!("cookie {cookie} does not fit usize")))?;
    let len = usize::try_from(read_u32_le(postings, start)?)
        .map_err(|_| Error::Corrupt("serialized length too large".into()))?;
    if len > MAX_SERIALIZED_BYTES {
        return Err(Error::Corrupt(format!(
            "serialized bitmap too large: {len} (max {MAX_SERIALIZED_BYTES})"
        )));
    }
    let body = start
        .checked_add(4)
        .ok_or_else(|| Error::Corrupt("serialized body offset overflow".into()))?;
    let end = body
        .checked_add(len)
        .ok_or_else(|| Error::Corrupt("serialized range overflow".into()))?;
    let blob = postings
        .get(body..end)
        .ok_or_else(|| Error::Corrupt("serialized bitmap truncated".into()))?;
    RoaringBitmap::deserialize_from(blob)
        .map_err(|e| Error::Corrupt(format!("deserialize trigram bitmap: {e}")))
}

fn read_ordinals_at(postings: &[u8], cookie: u64) -> Result<Vec<u32>> {
    let start = usize::try_from(cookie)
        .map_err(|_| Error::Corrupt(format!("cookie {cookie} does not fit usize")))?;
    let count = usize::try_from(read_u32_le(postings, start)?)
        .map_err(|_| Error::Corrupt("ordinal count too large".into()))?;
    if count > MAX_ORDINALS_PER_TRIGRAM {
        return Err(Error::Corrupt(format!(
            "too many ordinals for one trigram: {count} (max {MAX_ORDINALS_PER_TRIGRAM})"
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
    fn build_and_query_shared_trigram() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = NgramIndexBuilder::new();
        builder
            .record_package(
                0,
                vec![b"/bin/firefox".to_vec(), b"/usr/lib/libfoo.so".to_vec()],
            )
            .expect("pkg0");
        builder
            .record_package(
                1,
                vec![
                    b"/usr/bin/firefox-bin".to_vec(),
                    b"/etc/firefox.cfg".to_vec(),
                ],
            )
            .expect("pkg1");
        builder
            .record_package(2, vec![b"/bin/emacs".to_vec()])
            .expect("pkg2");
        builder.write_sidecars(dir.path()).expect("write");

        let index = NgramIndex::open(dir.path()).expect("open");

        // Both pkg0 and pkg1 contain "firefox" (share trigrams fir/ire/ref/efo/fox).
        let candidates = index
            .candidate_ordinals("firefox")
            .expect("candidates")
            .expect("some");
        let mut ordinals: Vec<u32> = candidates.iter().collect();
        ordinals.sort();
        assert_eq!(ordinals, vec![0, 1]);
    }

    #[test]
    fn no_shared_package_is_empty_bitmap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = NgramIndexBuilder::new();
        builder
            .record_package(0, vec![b"/bin/firefox".to_vec()])
            .expect("pkg0");
        builder.write_sidecars(dir.path()).expect("write");

        let index = NgramIndex::open(dir.path()).expect("open");

        let candidates = index
            .candidate_ordinals("zzzzzunique")
            .expect("candidates")
            .expect("some");
        assert!(candidates.is_empty());
    }

    #[test]
    fn pattern_shorter_than_three_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = NgramIndexBuilder::new();
        builder
            .record_package(0, vec![b"/bin/firefox".to_vec()])
            .expect("pkg0");
        builder.write_sidecars(dir.path()).expect("write");

        let index = NgramIndex::open(dir.path()).expect("open");
        assert!(
            index
                .candidate_ordinals("ab")
                .expect("candidates")
                .is_none()
        );
    }

    #[test]
    fn regex_like_pattern_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = NgramIndexBuilder::new();
        builder
            .record_package(0, vec![b"/bin/firefox".to_vec()])
            .expect("pkg0");
        builder.write_sidecars(dir.path()).expect("write");

        let index = NgramIndex::open(dir.path()).expect("open");
        assert!(
            index
                .candidate_ordinals("firefo*")
                .expect("candidates")
                .is_none(),
            "glob should return None"
        );
        assert!(
            index
                .candidate_ordinals("fire.ox")
                .expect("candidates")
                .is_none()
        );
    }

    #[test]
    fn open_missing_sidecar_is_missing_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = NgramIndex::open(dir.path()).expect_err("should fail");
        assert!(
            matches!(err, Error::Missing { .. }),
            "unexpected error variant: {err:?}"
        );
    }

    #[test]
    fn short_paths_are_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = NgramIndexBuilder::new();
        // Path shorter than 3 bytes must be ignored but not error.
        builder
            .record_package(0, vec![b"/a".to_vec(), b"ab".to_vec(), b"/bin/ls".to_vec()])
            .expect("pkg0");
        builder.write_sidecars(dir.path()).expect("write");

        let index = NgramIndex::open(dir.path()).expect("open");
        let candidates = index
            .candidate_ordinals("bin")
            .expect("candidates")
            .expect("some");
        let ordinals: Vec<u32> = candidates.iter().collect();
        assert_eq!(ordinals, vec![0]);
    }

    #[test]
    fn intersecting_trigrams_narrows_candidates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = NgramIndexBuilder::new();
        // pkg0 has both "abcdef" and "xyzabc"
        builder
            .record_package(0, vec![b"/abcdef".to_vec(), b"/xyzabc".to_vec()])
            .expect("pkg0");
        // pkg1 has only "abcdef"
        builder
            .record_package(1, vec![b"/abcdef".to_vec()])
            .expect("pkg1");
        builder.write_sidecars(dir.path()).expect("write");

        let index = NgramIndex::open(dir.path()).expect("open");
        // "abc" appears in both packages.
        let ordinals: Vec<u32> = index
            .candidate_ordinals("abc")
            .expect("candidates")
            .expect("some")
            .iter()
            .collect();
        assert_eq!(ordinals, vec![0, 1]);

        // "abcde" appears only in pkg0 and pkg1 (both have "abcdef").
        let ordinals: Vec<u32> = index
            .candidate_ordinals("abcde")
            .expect("candidates")
            .expect("some")
            .iter()
            .collect();
        assert_eq!(ordinals, vec![0, 1]);

        // "xyzabc" appears only in pkg0.
        let ordinals: Vec<u32> = index
            .candidate_ordinals("xyzabc")
            .expect("candidates")
            .expect("some")
            .iter()
            .collect();
        assert_eq!(ordinals, vec![0]);
    }

    #[test]
    fn read_bitmap_at_v1_decodes_raw_ordinals() {
        let mut raw = Vec::new();
        raw.extend_from_slice(POSTINGS_MAGIC);
        raw.extend_from_slice(&SIDE_VERSION_V1.to_le_bytes());
        let cookie = u64::try_from(raw.len()).expect("cookie");
        let ordinals: [u32; 3] = [3, 7, 42];
        raw.extend_from_slice(&u32::try_from(ordinals.len()).expect("len").to_le_bytes());
        for o in ordinals {
            raw.extend_from_slice(&o.to_le_bytes());
        }
        let bm = read_bitmap_at(&raw, cookie, SIDE_VERSION_V1).expect("decode");
        let got: Vec<u32> = bm.iter().collect();
        assert_eq!(got, vec![3, 7, 42]);
    }

    #[test]
    fn read_bitmap_at_v2_round_trips_native_bitmap() {
        let mut raw = Vec::new();
        raw.extend_from_slice(POSTINGS_MAGIC);
        raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());
        let mut bm = RoaringBitmap::new();
        for o in [1u32, 5, 1000, 1_000_000] {
            bm.insert(o);
        }
        let mut buf = Vec::new();
        bm.serialize_into(&mut buf).expect("serialize");
        let cookie = u64::try_from(raw.len()).expect("cookie");
        raw.extend_from_slice(&u32::try_from(buf.len()).expect("len").to_le_bytes());
        raw.extend_from_slice(&buf);
        let got = read_bitmap_at(&raw, cookie, SIDE_VERSION).expect("decode");
        assert_eq!(
            got.iter().collect::<Vec<u32>>(),
            vec![1, 5, 1000, 1_000_000]
        );
    }

    #[test]
    fn intersecting_lazy_short_circuits_on_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = NgramIndexBuilder::new();
        // pkg0 has "abcdef"; "xyz" shares nothing with it.
        builder
            .record_package(0, vec![b"/abcdef".to_vec()])
            .expect("pkg0");
        builder.write_sidecars(dir.path()).expect("write");

        let index = NgramIndex::open(dir.path()).expect("open");
        // "xyzabcdef" requires "xyz" (only in no package) AND "abc"/"bcd"/"cde"/"def"
        // (in pkg0). The absent "xyz" trigram makes the intersection empty.
        let ordinals: Vec<u32> = index
            .candidate_ordinals("xyzabcdef")
            .expect("candidates")
            .expect("some")
            .iter()
            .collect();
        assert!(ordinals.is_empty());
    }
}
