//! Basename secondary index: FST map + postings of package ordinals.
//!
//! Sidecar layout (siblings of the NIXI `files` database):
//! - `files.basename.fst` — [`fst::Map`] from basename bytes → cookie (`u64`)
//! - `files.basename.postings` — cookie points at a packed ordinal list
//! - `files.packages.names` — ordinal → package label (one length-prefixed UTF-8 name)
//!
//! See `research/modernization-2026.md` for design rationale.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use byteorder::{LittleEndian, WriteBytesExt};
use fst::Map;
use mmap_guard;
use roaring::RoaringBitmap;
use thiserror::Error;

/// Magic for the postings blob.
const POSTINGS_MAGIC: &[u8] = b"NBPO";
/// Magic for the package-name table.
const NAMES_MAGIC: &[u8] = b"NPKG";
/// Sidecar format version.
const SIDE_VERSION: u32 = 1;

/// Maximum total size of the package-names sidecar (defensive cap).
const MAX_NAMES_BYTES: usize = 64 * 1024 * 1024;

/// Maximum number of package labels in the names sidecar.
///
/// This prevents a malicious sidecar full of zero-length names from causing a
/// huge `Vec<String>` allocation (the file-size cap alone is not enough).
const MAX_NAME_COUNT: usize = 2_000_000;

/// Maximum length of a single package label in the names sidecar.
const MAX_NAME_BYTES: usize = 64 * 1024;

/// Maximum total size of the postings sidecar (defensive cap).
const MAX_POSTINGS_BYTES: usize = 1024 * 1024 * 1024;

/// Maximum number of package ordinals returned for a single basename.
///
/// This bounds the allocation in `read_ordinals_at` to a few megabytes per
/// lookup instead of the full postings file size.
const MAX_ORDINALS_PER_BASENAME: usize = 1_000_000;

/// Maximum total size of the FST sidecar (defensive cap).
const MAX_FST_BYTES: usize = 128 * 1024 * 1024;

/// Sidecar basenames relative to the database directory.
pub const FST_FILE: &str = "files.basename.fst";
/// Postings table filename.
pub const POSTINGS_FILE: &str = "files.basename.postings";
/// Package name table filename.
pub const NAMES_FILE: &str = "files.packages.names";

/// Errors while building or querying the basename secondary index.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Secondary index files are missing from the database directory.
    #[error("basename secondary index missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("basename secondary index corrupt: {0}")]
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

/// Accumulates basename → package ordinal mappings while a NIXI database is written.
#[derive(Debug, Default)]
pub struct BasenameIndexBuilder {
    next_ordinal: u32,
    /// basename → package ordinals (may contain many packages for common names).
    basenames: BTreeMap<Vec<u8>, RoaringBitmap>,
    /// package ordinal → label used by `query_fst` (typically `attr.output`).
    package_names: Vec<String>,
}

impl BasenameIndexBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one package and every basename from its absolute paths (with leading `/`).
    ///
    /// Returns the assigned package ordinal. The ordinal is consumed unconditionally;
    /// callers should skip empty packages before calling if they do not want empty
    /// packages to consume ordinals.
    pub fn record_package(
        &mut self,
        package_label: String,
        paths: impl IntoIterator<Item = Vec<u8>>,
    ) -> Result<u32> {
        let ordinal = self.next_ordinal;
        let next = ordinal
            .checked_add(1)
            .ok_or_else(|| Error::Corrupt("package ordinal overflow".into()))?;
        self.next_ordinal = next;
        self.package_names.push(package_label);

        for path in paths {
            let base = basename_of(&path);
            if base.is_empty() {
                continue;
            }
            self.basenames
                .entry(base.to_vec())
                .or_default()
                .insert(ordinal);
        }
        Ok(ordinal)
    }

    /// Number of packages recorded.
    #[must_use]
    pub fn package_count(&self) -> u32 {
        self.next_ordinal
    }

    /// Write sidecar files into `db_dir` (the directory that holds `files`).
    ///
    /// # Errors
    ///
    /// Returns an error if any sidecar cannot be written or the FST build fails.
    pub fn write_sidecars(&self, db_dir: &Path) -> Result<()> {
        write_package_names(&db_dir.join(NAMES_FILE), &self.package_names)?;

        let mut raw = Vec::new();
        raw.extend_from_slice(POSTINGS_MAGIC);
        raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());

        // BTreeMap keeps basenames sorted — required by `fst::MapBuilder`.
        let mut cookies: Vec<(Vec<u8>, u64)> = Vec::with_capacity(self.basenames.len());
        for (base, bitmap) in &self.basenames {
            let cookie = u64::try_from(raw.len())
                .map_err(|_| Error::Corrupt("postings cookie overflow".into()))?;
            let mut ordinals: Vec<u32> = bitmap.iter().collect();
            ordinals.sort_unstable();
            ordinals.dedup();
            let count = u32::try_from(ordinals.len())
                .map_err(|_| Error::Corrupt("too many ordinals for one basename".into()))?;
            raw.extend_from_slice(&count.to_le_bytes());
            for o in ordinals {
                raw.extend_from_slice(&o.to_le_bytes());
            }
            cookies.push((base.clone(), cookie));
        }
        std::fs::write(db_dir.join(POSTINGS_FILE), &raw)?;

        let mut builder = fst::MapBuilder::memory();
        for (base, cookie) in &cookies {
            builder
                .insert(base, *cookie)
                .map_err(|err| Error::Fst(err.to_string()))?;
        }
        let fst_bytes = builder
            .into_inner()
            .map_err(|err| Error::Fst(err.to_string()))?;
        std::fs::write(db_dir.join(FST_FILE), fst_bytes)?;
        Ok(())
    }
}

/// Opened basename secondary index for exact-basename queries.
#[derive(Debug)]
pub struct BasenameIndex {
    // FileData implements AsRef<[u8]>, so fst::Map can use it directly.
    map: Map<mmap_guard::FileData>,
    postings: mmap_guard::FileData,
    names: mmap_guard::FileData,
    // Validated byte ranges into `names` for each package ordinal.
    name_ranges: Vec<(usize, usize)>,
}

impl BasenameIndex {
    /// Open sidecars from a database directory.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Missing`] when any required sidecar is absent, or
    /// [`Error::Corrupt`] / [`Error::Fst`] when the files cannot be parsed.
    pub fn open(db_dir: &Path) -> Result<Self> {
        let fst_path = db_dir.join(FST_FILE);
        let postings_path = db_dir.join(POSTINGS_FILE);
        let names_path = db_dir.join(NAMES_FILE);

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
        if !names_path.is_file() {
            return Err(Error::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {NAMES_FILE}"),
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

        let names = mmap_guard::map_file(&names_path).map_err(Error::Io)?;
        if names.len() > MAX_NAMES_BYTES {
            return Err(Error::Corrupt("package names file too large".into()));
        }
        let name_ranges = parse_name_ranges(&names)?;

        Ok(Self {
            map,
            postings,
            names,
            name_ranges,
        })
    }

    /// Look up package labels that contain an exact basename (final path component).
    ///
    /// Returns borrowed string slices into the mmapped names data.
    /// Returns an empty list when the basename is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when postings for a present FST key are corrupt.
    pub fn lookup_basename(&self, basename: &[u8]) -> Result<Vec<&str>> {
        let Some(cookie) = self.map.get(basename) else {
            return Ok(Vec::new());
        };
        let ordinals = read_ordinals_at(&self.postings, cookie)?;
        let mut labels = Vec::with_capacity(ordinals.len());
        for ord in ordinals {
            let index = usize::try_from(ord)
                .map_err(|_| Error::Corrupt(format!("package ordinal {ord} does not fit usize")))?;
            let Some((start, end)) = self.name_ranges.get(index) else {
                return Err(Error::Corrupt(format!(
                    "package ordinal {ord} out of range (names={})",
                    self.name_ranges.len()
                )));
            };
            let bytes = self.names.get(*start..*end).ok_or_else(|| {
                Error::Corrupt(format!("package ordinal {ord} name range out of bounds"))
            })?;
            let s = std::str::from_utf8(bytes)
                .map_err(|e| Error::Corrupt(format!("package ordinal {ord} invalid UTF-8: {e}")))?;
            labels.push(s);
        }
        Ok(labels)
    }

    /// Look up raw package ordinals that contain an exact basename.
    ///
    /// Returns an empty list when the basename is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when postings for a present FST key are corrupt.
    pub fn lookup_basename_ordinals(&self, basename: &[u8]) -> Result<Vec<u32>> {
        let Some(cookie) = self.map.get(basename) else {
            return Ok(Vec::new());
        };
        read_ordinals_at(&self.postings, cookie)
    }

    /// Number of packages recorded in the name table.
    #[must_use]
    pub fn package_count(&self) -> usize {
        self.name_ranges.len()
    }
}

/// Final component of a store-relative path (`/bin/ls` → `ls`).
#[must_use]
pub fn basename_of(path: &[u8]) -> &[u8] {
    match memchr::memrchr(b'/', path) {
        Some(i) => match path.get(i + 1..) {
            Some(rest) => rest,
            None => &[],
        },
        None => path,
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
    if count > MAX_ORDINALS_PER_BASENAME {
        return Err(Error::Corrupt(format!(
            "too many ordinals for one basename: {count} (max {MAX_ORDINALS_PER_BASENAME})"
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

fn write_package_names(path: &Path, names: &[String]) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(NAMES_MAGIC)?;
    w.write_u32::<LittleEndian>(SIDE_VERSION)?;
    w.write_u32::<LittleEndian>(
        u32::try_from(names.len()).map_err(|_| Error::Corrupt("too many packages".into()))?,
    )?;
    for name in names {
        let bytes = name.as_bytes();
        w.write_u32::<LittleEndian>(
            u32::try_from(bytes.len()).map_err(|_| Error::Corrupt("name too long".into()))?,
        )?;
        w.write_all(bytes)?;
    }
    w.flush()?;
    Ok(())
}

/// Parse the names sidecar into byte ranges without cloning strings.
fn parse_name_ranges(bytes: &[u8]) -> Result<Vec<(usize, usize)>> {
    let magic = bytes
        .get(..NAMES_MAGIC.len())
        .ok_or(Error::Corrupt("names too short for magic".into()))?;
    if magic != NAMES_MAGIC {
        return Err(Error::Corrupt(format!(
            "names magic {magic:?}, expected {:?}",
            NAMES_MAGIC
        )));
    }

    let ver = read_u32_le(bytes, NAMES_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(Error::Corrupt(format!(
            "names version {ver}, expected {SIDE_VERSION}"
        )));
    }

    let count = usize::try_from(read_u32_le(bytes, NAMES_MAGIC.len() + 4)?)
        .map_err(|_| Error::Corrupt("package name count too large".into()))?;
    if count > MAX_NAME_COUNT {
        return Err(Error::Corrupt(format!(
            "package name count too large: {count} (max {MAX_NAME_COUNT})"
        )));
    }
    let header_size = NAMES_MAGIC.len() + 4 + 4;
    if count
        .checked_mul(4)
        .is_none_or(|need| need > bytes.len().saturating_sub(header_size))
    {
        return Err(Error::Corrupt("package name count too large".into()));
    }

    let mut ranges = Vec::with_capacity(count);
    let mut pos = header_size;
    for _ in 0..count {
        let len = usize::try_from(read_u32_le(bytes, pos)?)
            .map_err(|_| Error::Corrupt("package name length too large".into()))?;
        if len == 0 {
            return Err(Error::Corrupt("empty package name".into()));
        }
        if len > MAX_NAME_BYTES {
            return Err(Error::Corrupt(format!("package name too long: {len}")));
        }
        let body_start = pos + 4;
        let body_end = body_start
            .checked_add(len)
            .ok_or(Error::Corrupt("package name length overflow".into()))?;
        if body_end > bytes.len() {
            return Err(Error::Corrupt("package name truncated".into()));
        }
        ranges.push((body_start, body_end));
        pos = body_end;
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_of_paths() {
        assert_eq!(basename_of(b"/bin/ls"), b"ls");
        assert_eq!(basename_of(b"ls"), b"ls");
        assert_eq!(basename_of(b"/"), b"");
        assert_eq!(basename_of(b"/bin/"), b"");
    }

    #[test]
    fn build_and_query_exact_basename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = BasenameIndexBuilder::new();
        builder
            .record_package(
                "coreutils.out".into(),
                vec![b"/bin/ls".to_vec(), b"/bin/cat".to_vec()],
            )
            .expect("pkg0");
        builder
            .record_package("hello.out".into(), vec![b"/bin/hello".to_vec()])
            .expect("pkg1");
        builder
            .record_package(
                "busybox.out".into(),
                // shares basename `ls` with coreutils
                vec![b"/bin/ls".to_vec()],
            )
            .expect("pkg2");
        builder.write_sidecars(dir.path()).expect("write");

        let index = BasenameIndex::open(dir.path()).expect("open");
        assert_eq!(index.package_count(), 3);

        let mut ls = index.lookup_basename(b"ls").expect("ls");
        ls.sort();
        assert_eq!(ls, vec!["busybox.out", "coreutils.out"]);

        let hello = index.lookup_basename(b"hello").expect("hello");
        assert_eq!(hello, vec!["hello.out"]);

        let missing = index.lookup_basename(b"nope").expect("nope");
        assert!(missing.is_empty());

        // Test lookup_basename_ordinals
        let mut ls_ordinals = index.lookup_basename_ordinals(b"ls").expect("ls ordinals");
        ls_ordinals.sort();
        assert_eq!(ls_ordinals, vec![0, 2]); // coreutils (0), busybox (2)
    }

    #[test]
    fn open_missing_sidecar_is_missing_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = BasenameIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Missing { .. }));
    }

    #[test]
    fn parse_name_ranges_rejects_empty_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(NAMES_FILE);
        {
            let mut w = BufWriter::new(File::create(&path).expect("create"));
            w.write_all(NAMES_MAGIC).expect("magic");
            w.write_u32::<LittleEndian>(SIDE_VERSION).expect("ver");
            w.write_u32::<LittleEndian>(1).expect("count");
            w.write_u32::<LittleEndian>(0).expect("empty len");
            w.flush().expect("flush");
        }

        let bytes = std::fs::read(&path).expect("read");
        let err = parse_name_ranges(&bytes).expect_err("empty name should fail");
        assert!(matches!(err, Error::Corrupt(_)));
        assert!(err.to_string().contains("empty package name"));
    }

    #[test]
    fn parse_name_ranges_rejects_excessive_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(NAMES_FILE);
        {
            let mut w = BufWriter::new(File::create(&path).expect("create"));
            w.write_all(NAMES_MAGIC).expect("magic");
            w.write_u32::<LittleEndian>(SIDE_VERSION).expect("ver");
            let count = u32::try_from(MAX_NAME_COUNT + 1).expect("count fits");
            w.write_u32::<LittleEndian>(count).expect("count");
            w.flush().expect("flush");
        }

        let bytes = std::fs::read(&path).expect("read");
        let err = parse_name_ranges(&bytes).expect_err("excessive count should fail");
        assert!(matches!(err, Error::Corrupt(_)));
        assert!(err.to_string().contains("package name count too large"));
    }

    #[test]
    fn read_ordinals_at_rejects_excessive_count() {
        let mut postings = Vec::new();
        postings.extend_from_slice(POSTINGS_MAGIC);
        postings.extend_from_slice(&SIDE_VERSION.to_le_bytes());
        // Cookie 8 (right after header) points at a count that exceeds the cap.
        let count = u32::try_from(MAX_ORDINALS_PER_BASENAME + 1).expect("count fits");
        postings.extend_from_slice(&count.to_le_bytes());

        let err = read_ordinals_at(&postings, 8).expect_err("excessive ordinal count should fail");
        assert!(matches!(err, Error::Corrupt(_)));
        assert!(err.to_string().contains("too many ordinals"));
    }

    #[test]
    fn lookup_basename_empty_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let builder = BasenameIndexBuilder::new();
        builder.write_sidecars(dir.path()).expect("write");

        let index = BasenameIndex::open(dir.path()).expect("open");
        assert_eq!(index.package_count(), 0);

        let result = index.lookup_basename(b"anything").expect("lookup");
        assert!(result.is_empty());
    }

    #[test]
    fn lookup_basename_multiple_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = BasenameIndexBuilder::new();

        // Multiple packages with the same basename
        for i in 0..5 {
            let label = format!("pkg{}.out", i);
            builder
                .record_package(label, vec![b"/bin/ls".to_vec()])
                .expect("record");
        }

        builder.write_sidecars(dir.path()).expect("write");

        let index = BasenameIndex::open(dir.path()).expect("open");
        let results = index.lookup_basename(b"ls").expect("lookup");
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn open_missing_fst_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Create only the postings file, missing FST
        let path = dir.path().join(POSTINGS_FILE);
        std::fs::write(&path, b"dummy").expect("write");

        let err = BasenameIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Missing { .. }));
        assert!(err.to_string().contains(FST_FILE));
    }

    #[test]
    fn open_missing_postings_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Create only the FST file, missing postings
        let path = dir.path().join(FST_FILE);
        std::fs::write(&path, b"dummy").expect("write");

        let err = BasenameIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Missing { .. }));
        assert!(err.to_string().contains(POSTINGS_FILE));
    }

    #[test]
    fn open_missing_names_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Create FST and postings, missing names
        std::fs::write(dir.path().join(FST_FILE), b"dummy").expect("write");
        std::fs::write(dir.path().join(POSTINGS_FILE), b"dummy").expect("write");

        let err = BasenameIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Missing { .. }));
        assert!(err.to_string().contains(NAMES_FILE));
    }

    #[test]
    fn oversized_fst_sidecar_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");

        // BasenameIndex::open checks that all sidecars exist before it validates size.
        std::fs::write(dir.path().join(POSTINGS_FILE), &[]).expect("write");
        std::fs::write(dir.path().join(NAMES_FILE), &[]).expect("write");

        let path = dir.path().join(FST_FILE);
        let oversized = vec![0u8; MAX_FST_BYTES + 1];
        std::fs::write(&path, &oversized).expect("write");

        let err = BasenameIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Corrupt(_)));
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn oversized_postings_sidecar_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Create a minimal valid FST so the open reaches the postings size check.
        let fst_bytes = fst::MapBuilder::memory().into_inner().expect("fst");
        std::fs::write(dir.path().join(FST_FILE), &fst_bytes).expect("write");
        std::fs::write(dir.path().join(NAMES_FILE), &[]).expect("write");

        let path = dir.path().join(POSTINGS_FILE);
        let oversized = vec![0u8; MAX_POSTINGS_BYTES + 1];
        std::fs::write(&path, &oversized).expect("write");

        let err = BasenameIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Corrupt(_)));
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn basename_of_edge_cases() {
        assert_eq!(basename_of(b""), b"");
        assert_eq!(basename_of(b"file"), b"file");
        assert_eq!(basename_of(b"/"), b"");
        assert_eq!(basename_of(b"/file"), b"file");
        assert_eq!(basename_of(b"/path/to/file"), b"file");
        assert_eq!(basename_of(b"/path/to/"), b"");
        assert_eq!(basename_of(b"//double"), b"double");
    }

    #[test]
    fn record_package_ordinal_overflow() {
        let mut builder = BasenameIndexBuilder::new();
        builder.next_ordinal = u32::MAX;

        let result = builder.record_package("test".into(), vec![b"/bin/test".to_vec()]);
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::Corrupt(_))));
        assert!(result.unwrap_err().to_string().contains("overflow"));
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::read_u32_le;

    #[kani::proof]
    #[kani::unwind(9)]
    fn check_read_u32_le_no_panic() {
        let mut bytes = [0u8; 8];
        let len: usize = kani::any();
        kani::assume(len <= bytes.len());
        for i in 0..len {
            bytes[i] = kani::any();
        }
        let at: usize = kani::any();
        kani::assume(at <= bytes.len());
        let _ = read_u32_le(&bytes[..len], at);
    }
}
