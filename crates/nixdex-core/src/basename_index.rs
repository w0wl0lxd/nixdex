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
use roaring::RoaringBitmap;
use thiserror::Error;

/// Magic for the postings blob.
const POSTINGS_MAGIC: &[u8] = b"NBPO";
/// Magic for the package-name table.
const NAMES_MAGIC: &[u8] = b"NPKG";
/// Sidecar format version.
const SIDE_VERSION: u32 = 1;

/// Maximum total size of the package-names sidecar (defensive cap).
const MAX_NAMES_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum length of a single package label in the names sidecar.
const MAX_NAME_BYTES: usize = 64 * 1024;

/// Maximum total size of the postings sidecar (defensive cap).
const MAX_POSTINGS_BYTES: u64 = 1024 * 1024 * 1024;

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
    map: Map<Vec<u8>>,
    postings: Vec<u8>,
    package_names: Vec<String>,
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

        let fst_bytes = std::fs::read(&fst_path)?;
        let map = Map::new(fst_bytes).map_err(|err| Error::Fst(err.to_string()))?;

        let postings = std::fs::read(&postings_path)?;
        if postings.len() > MAX_POSTINGS_BYTES as usize {
            return Err(Error::Corrupt("postings file too large".into()));
        }
        validate_postings_header(&postings)?;

        let package_names = read_package_names(&names_path)?;

        Ok(Self {
            map,
            postings,
            package_names,
        })
    }

    /// Look up package labels that contain an exact basename (final path component).
    ///
    /// Returns an empty list when the basename is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when postings for a present FST key are corrupt.
    pub fn lookup_basename(&self, basename: &[u8]) -> Result<Vec<String>> {
        let Some(cookie) = self.map.get(basename) else {
            return Ok(Vec::new());
        };
        let ordinals = read_ordinals_at(&self.postings, cookie)?;
        let mut labels = Vec::with_capacity(ordinals.len());
        for ord in ordinals {
            let Some(name) = self.package_names.get(ord as usize) else {
                return Err(Error::Corrupt(format!(
                    "package ordinal {ord} out of range (names={})",
                    self.package_names.len()
                )));
            };
            labels.push(name.clone());
        }
        Ok(labels)
    }

    /// Number of packages recorded in the name table.
    #[must_use]
    pub fn package_count(&self) -> usize {
        self.package_names.len()
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
    let count = read_u32_le(postings, start)? as usize;
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

fn read_package_names(path: &Path) -> Result<Vec<String>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > MAX_NAMES_BYTES as usize {
        return Err(Error::Corrupt("package names file too large".into()));
    }

    let magic = bytes
        .get(..NAMES_MAGIC.len())
        .ok_or(Error::Corrupt("names too short for magic".into()))?;
    if magic != NAMES_MAGIC {
        return Err(Error::Corrupt(format!(
            "names magic {magic:?}, expected {:?}",
            NAMES_MAGIC
        )));
    }

    let ver = read_u32_le(&bytes, NAMES_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(Error::Corrupt(format!(
            "names version {ver}, expected {SIDE_VERSION}"
        )));
    }

    let count = u64::from(read_u32_le(&bytes, NAMES_MAGIC.len() + 4)?);
    let header_size = NAMES_MAGIC.len() + 4 + 4;
    if count
        .checked_mul(4)
        .is_none_or(|need| need > (bytes.len() as u64).saturating_sub(header_size as u64))
    {
        return Err(Error::Corrupt("package name count too large".into()));
    }

    let mut names = Vec::with_capacity(count as usize);
    let mut pos = header_size;
    for _ in 0..count {
        let len = read_u32_le(&bytes, pos)? as usize;
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
        let s = String::from_utf8(
            bytes
                .get(body_start..body_end)
                .ok_or(Error::Corrupt("package name slice missing".into()))?
                .to_vec(),
        )
        .map_err(|err| Error::Corrupt(err.to_string()))?;
        names.push(s);
        pos = body_end;
    }
    Ok(names)
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
    }

    #[test]
    fn open_missing_sidecar_is_missing_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = BasenameIndex::open(dir.path()).expect_err("should fail");
        assert!(matches!(err, Error::Missing { .. }));
    }
}
