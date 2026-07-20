//! Entry-level basename index: FST map + postings of `(store path, file entry)`.
//!
//! Sidecar layout (siblings of the NIXI `files` database):
//! - `files.entry.fst` — [`fst::Map`] from a raw basename → cookie (`u64`)
//! - `files.entry.postings` — cookie → `u32` blob length + postcard blob of
//!   `Vec<EntryRecord>`
//! - `files.entryidx.sp` — postcard-serialized `Vec<StorePath>`, indexed by the
//!   `store_path_idx` stored inside each [`EntryRecord`]
//!
//! For `command-not-found` / `find_command_providers` we look up an exact
//! basename, then return every `(StorePath, FileTreeEntry)` that contributes a
//! file with that basename — without decoding any zstd frame.

use std::io;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use fst::Map;
use mmap_guard;
use postcard;
use serde::{Deserialize, Serialize};

use crate::basename_index::basename_of;
use crate::files::{FileNode, FileTreeEntry};
use crate::store_path::StorePath;

/// Magic for the postings blob.
const POSTINGS_MAGIC: &[u8] = b"NBEN";
/// Sidecar format version.
const SIDE_VERSION: u32 = 1;

/// Maximum total size of the postings sidecar (defensive cap).
const MAX_POSTINGS_BYTES: usize = 1 << 30;

/// Maximum total size of the FST sidecar (defensive cap).
const MAX_FST_BYTES: usize = 512 << 20;

/// Maximum size of a single postcard blob for one basename (defensive cap).
const MAX_BLOB_BYTES: usize = 1 << 28;

/// Sidecar basename for the entry FST.
pub const FST_FILE: &str = "files.entry.fst";
/// Postings table filename.
pub const POSTINGS_FILE: &str = "files.entry.postings";
/// Store-path table filename (indexed by `store_path_idx`).
pub const STORE_PATHS_FILE: &str = "files.entryidx.sp";

/// Errors while building or querying the entry secondary index.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum IndexError {
    /// Secondary index files are missing from the database directory.
    #[error("entry secondary index missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("entry secondary index corrupt: {0}")]
    Corrupt(String),

    /// FST crate reported a build or query error.
    #[error("fst error: {0}")]
    Fst(String),

    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Convenience alias.
pub type IndexResult<T> = std::result::Result<T, IndexError>;

/// One occurrence of a basename inside a single store path.
#[derive(Debug, Serialize, Deserialize)]
struct EntryRecord {
    /// Index into the store-path table written alongside this index.
    store_path_idx: u32,
    /// Absolute in-store path of the entry (starts with `/`).
    path: Vec<u8>,
    /// Content-free node (type / executable / symlink target).
    node: FileNode<()>,
}

/// Accumulates basename → `(store path, entry)` mappings while a NIXI database is written.
#[derive(Debug, Default)]
pub struct EntryIndexBuilder {
    /// Parallel to `store_path_idx` referenced by [`EntryRecord`].
    store_paths: Vec<StorePath>,
    /// basename → records contributed by any store path.
    basename_map: IndexMap<Vec<u8>, Vec<EntryRecord>>,
}

impl EntryIndexBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record every file entry belonging to `store_path`.
    ///
    /// Packages with no entries are skipped (they can never satisfy a lookup).
    ///
    /// # Errors
    ///
    /// Returns an error if more than `u32::MAX` packages are recorded.
    pub fn record_package(
        &mut self,
        store_path: &StorePath,
        entries: &[FileTreeEntry],
    ) -> IndexResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let sp_idx = u32::try_from(self.store_paths.len())
            .map_err(|_| IndexError::Corrupt("too many packages for entry index".into()))?;
        self.store_paths.push(store_path.clone());
        for entry in entries {
            let base = basename_of(&entry.path).to_vec();
            if base.is_empty() {
                continue;
            }
            self.basename_map
                .entry(base)
                .or_default()
                .push(EntryRecord {
                    store_path_idx: sp_idx,
                    path: entry.path.clone(),
                    node: entry.node.clone(),
                });
        }
        Ok(())
    }

    /// Number of packages recorded.
    #[must_use]
    pub fn package_count(&self) -> usize {
        self.store_paths.len()
    }

    /// Write sidecar files into `db_dir` (the directory that holds `files`).
    ///
    /// # Errors
    ///
    /// Returns an error if any sidecar cannot be written or the FST build fails.
    pub fn write_sidecars(&self, db_dir: &Path) -> IndexResult<()> {
        self.write_sidecars_inner(db_dir)
    }

    fn write_sidecars_inner(&self, db_dir: &Path) -> IndexResult<()> {
        let sp_bytes = postcard::to_allocvec(&self.store_paths)
            .map_err(|e| IndexError::Corrupt(format!("serialize store paths: {e}")))?;
        std::fs::write(db_dir.join(STORE_PATHS_FILE), &sp_bytes)?;

        let mut raw = Vec::new();
        raw.extend_from_slice(POSTINGS_MAGIC);
        raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());

        // Sort basenames for the FST builder.
        let mut keys: Vec<&[u8]> = self.basename_map.keys().map(Vec::as_slice).collect();
        keys.sort_unstable();

        let mut cookies: Vec<(&[u8], u64)> = Vec::with_capacity(keys.len());
        for key in keys {
            let records = self
                .basename_map
                .get(key)
                .ok_or_else(|| IndexError::Corrupt("missing basename key".into()))?;
            let blob = postcard::to_allocvec(records)
                .map_err(|e| IndexError::Corrupt(format!("serialize entries: {e}")))?;
            if blob.len() > MAX_BLOB_BYTES {
                return Err(IndexError::Corrupt("basename blob too large".into()));
            }
            let len_u32 = u32::try_from(blob.len())
                .map_err(|_| IndexError::Corrupt("basename blob too large".into()))?;
            let cookie = u64::try_from(raw.len())
                .map_err(|_| IndexError::Corrupt("postings cookie overflow".into()))?;
            raw.extend_from_slice(&len_u32.to_le_bytes());
            raw.extend_from_slice(&blob);
            cookies.push((key, cookie));
        }
        std::fs::write(db_dir.join(POSTINGS_FILE), &raw)?;

        let mut builder = fst::MapBuilder::memory();
        for (key, cookie) in &cookies {
            builder
                .insert(key, *cookie)
                .map_err(|err| IndexError::Fst(err.to_string()))?;
        }
        let fst_bytes = builder
            .into_inner()
            .map_err(|err| IndexError::Fst(err.to_string()))?;
        std::fs::write(db_dir.join(FST_FILE), fst_bytes)?;
        Ok(())
    }
}

/// Opened entry secondary index for exact-basename queries.
#[derive(Debug)]
pub struct EntryIndex {
    // FileData implements AsRef<[u8]>, so fst::Map can use it directly.
    map: Map<mmap_guard::FileData>,
    postings: mmap_guard::FileData,
    store_paths: Vec<StorePath>,
}

impl EntryIndex {
    /// Open sidecars from a database directory.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError::Missing`] when any required sidecar is absent, or
    /// [`IndexError::Corrupt`] / [`IndexError::Fst`] when the files cannot be parsed.
    pub fn open(db_dir: &Path) -> IndexResult<Self> {
        Self::open_inner(db_dir)
    }

    fn open_inner(db_dir: &Path) -> IndexResult<Self> {
        let fst_path = db_dir.join(FST_FILE);
        let postings_path = db_dir.join(POSTINGS_FILE);
        let sp_path = db_dir.join(STORE_PATHS_FILE);

        if !fst_path.is_file() {
            return Err(IndexError::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {FST_FILE}"),
            });
        }
        if !postings_path.is_file() {
            return Err(IndexError::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {POSTINGS_FILE}"),
            });
        }
        if !sp_path.is_file() {
            return Err(IndexError::Missing {
                dir: db_dir.to_path_buf(),
                detail: format!("expected {STORE_PATHS_FILE}"),
            });
        }

        let fst_data = mmap_guard::map_file(&fst_path).map_err(IndexError::Io)?;
        if fst_data.len() > MAX_FST_BYTES {
            return Err(IndexError::Corrupt("fst file too large".into()));
        }
        let map = Map::new(fst_data).map_err(|err| IndexError::Fst(err.to_string()))?;

        let postings = mmap_guard::map_file(&postings_path).map_err(IndexError::Io)?;
        if postings.len() > MAX_POSTINGS_BYTES {
            return Err(IndexError::Corrupt("postings file too large".into()));
        }
        validate_postings_header(&postings)?;

        let sp_bytes = std::fs::read(&sp_path).map_err(IndexError::Io)?;
        let store_paths: Vec<StorePath> = postcard::from_bytes(&sp_bytes)
            .map_err(|e| IndexError::Corrupt(format!("store paths: {e}")))?;

        Ok(Self {
            map,
            postings,
            store_paths,
        })
    }

    /// Look up every `(StorePath, FileTreeEntry)` whose entry basename equals `basename`.
    ///
    /// Returns an empty vector when the basename is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when postings for a present FST key are corrupt.
    pub fn lookup_entries(&self, basename: &[u8]) -> IndexResult<Vec<(StorePath, FileTreeEntry)>> {
        self.lookup_entries_inner(basename)
    }

    fn lookup_entries_inner(
        &self,
        basename: &[u8],
    ) -> IndexResult<Vec<(StorePath, FileTreeEntry)>> {
        let Some(cookie) = self.map.get(basename) else {
            return Ok(Vec::new());
        };
        let records = read_entries_at(&self.postings, cookie)?;
        let mut out = Vec::with_capacity(records.len());
        for r in records {
            let idx = usize::try_from(r.store_path_idx).map_err(|_| {
                IndexError::Corrupt(format!("store path idx {} out of range", r.store_path_idx))
            })?;
            let sp = self.store_paths.get(idx).cloned().ok_or_else(|| {
                IndexError::Corrupt(format!("store path idx {} out of range", r.store_path_idx))
            })?;
            out.push((
                sp,
                FileTreeEntry {
                    path: r.path,
                    node: r.node,
                },
            ));
        }
        Ok(out)
    }
}

fn read_u32_le(bytes: &[u8], at: usize) -> IndexResult<u32> {
    let end = at
        .checked_add(4)
        .ok_or_else(|| IndexError::Corrupt("u32 offset overflow".into()))?;
    let slice = bytes
        .get(at..end)
        .ok_or_else(|| IndexError::Corrupt(format!("u32 read past end at {at}")))?;
    let arr: [u8; 4] = slice
        .try_into()
        .map_err(|_| IndexError::Corrupt("u32 slice length".into()))?;
    Ok(u32::from_le_bytes(arr))
}

fn validate_postings_header(postings: &[u8]) -> IndexResult<()> {
    if postings.len() < POSTINGS_MAGIC.len() + 4 {
        return Err(IndexError::Corrupt("postings too short".into()));
    }
    let magic = postings
        .get(..POSTINGS_MAGIC.len())
        .ok_or_else(|| IndexError::Corrupt("postings too short for magic".into()))?;
    if magic != POSTINGS_MAGIC {
        return Err(IndexError::Corrupt(format!(
            "postings magic {magic:?}, expected {POSTINGS_MAGIC:?}"
        )));
    }
    let ver = read_u32_le(postings, POSTINGS_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(IndexError::Corrupt(format!(
            "postings version {ver}, expected {SIDE_VERSION}"
        )));
    }
    Ok(())
}

fn read_entries_at(postings: &[u8], cookie: u64) -> IndexResult<Vec<EntryRecord>> {
    let start = usize::try_from(cookie)
        .map_err(|_| IndexError::Corrupt(format!("cookie {cookie} does not fit usize")))?;
    let len = usize::try_from(read_u32_le(postings, start)?)
        .map_err(|_| IndexError::Corrupt("blob length too large".into()))?;
    if len > MAX_BLOB_BYTES {
        return Err(IndexError::Corrupt(format!(
            "blob too large: {len} (max {MAX_BLOB_BYTES})"
        )));
    }
    let body = start
        .checked_add(4)
        .ok_or_else(|| IndexError::Corrupt("blob body offset overflow".into()))?;
    let end = body
        .checked_add(len)
        .ok_or_else(|| IndexError::Corrupt("blob range overflow".into()))?;
    let blob = postings
        .get(body..end)
        .ok_or_else(|| IndexError::Corrupt("blob truncated".into()))?;
    postcard::from_bytes(blob).map_err(|e| IndexError::Corrupt(format!("decode entries: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::FileType;
    use crate::store_path::Origin;

    fn entry(path: &[u8], executable: bool) -> FileTreeEntry {
        FileTreeEntry {
            path: path.to_vec(),
            node: FileNode::Regular {
                size: 0,
                executable,
            },
        }
    }

    fn sp(attr: &str, hash: &str) -> StorePath {
        StorePath::new(
            "/nix/store".to_string(),
            hash.to_string(),
            attr.to_string(),
            Origin {
                attr: attr.to_string(),
                output: "out".to_string(),
                toplevel: false,
                system: None,
            },
        )
    }

    #[test]
    fn build_and_lookup_returns_store_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = EntryIndexBuilder::new();
        let sp0 = sp(
            "ape",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        let sp1 = sp(
            "bee",
            "1111111111111111111111111111111111111111111111111111111111111111",
        );
        builder
            .record_package(
                &sp0,
                &[
                    entry(b"/bin/python", true),
                    entry(b"/usr/bin/python3", true),
                ],
            )
            .expect("pkg0");
        builder
            .record_package(&sp1, &[entry(b"/bin/emacs", false)])
            .expect("pkg1");
        builder.write_sidecars(dir.path()).expect("write");

        let index = EntryIndex::open(dir.path()).expect("open");
        let hits = index.lookup_entries(b"python").expect("lookup");
        let labels: Vec<_> = hits.iter().map(|(sp, _)| sp.to_string()).collect();
        assert_eq!(labels, vec![sp0.to_string()]);
        let paths: Vec<_> = hits
            .iter()
            .map(|(_, e)| String::from_utf8_lossy(&e.path).into_owned())
            .collect();
        assert_eq!(paths, vec!["/bin/python".to_string()]);

        // The index is exact-basename: `python3` is a distinct basename.
        let py3 = index.lookup_entries(b"python3").expect("lookup3");
        assert_eq!(py3.len(), 1);
        assert_eq!(String::from_utf8_lossy(&py3[0].1.path), "/usr/bin/python3");

        let emacs = index.lookup_entries(b"emacs").expect("lookup");
        assert_eq!(emacs.len(), 1);
        assert_eq!(emacs[0].0, sp1);
        assert_eq!(
            emacs[0].1.node.get_type(),
            FileType::Regular { executable: false }
        );

        assert!(index.lookup_entries(b"absent").expect("lookup").is_empty());
    }
}
