//! Per-path entry cache: maps a full path to every `(StorePath, FileTreeEntry)`
//! that contributes it, without decoding any zstd frame.
//!
//! Sidecar layout (siblings of the NIXI `files` database):
//! - `files.path-id.fst` — [`fst::Map`] from full path bytes → `path-id` (`u64`)
//! - `files.path-entries` — header + offset table + postcard blobs of
//!   `Vec<PathEntryRecord>`, indexed by `path-id`
//! - `files.path-strings` — concatenated null-terminated path bytes
//! - `files.path-offsets` — `path-id` → byte offset in `files.path-strings`
//! - `files.path-entries.sp` — postcard `Vec<StorePath>` indexed by `store_path_idx`
//!
//! For path-level trigram queries, the `path-id` returned by `PathTrigramIndex`
//! is looked up directly in the entry and string tables; the FST is only used
//! for exact-path queries.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use fst::Map;
use mmap_guard;
use postcard;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::files::{FileNode, FileTreeEntry};
use crate::store_path::StorePath;

/// Magic for the entries blob.
const ENTRIES_MAGIC: &[u8] = b"NNPE";
/// Magic for the path string table.
const STRINGS_MAGIC: &[u8] = b"NNPS";
/// Magic for the path string offset table.
const OFFSETS_MAGIC: &[u8] = b"NNOF";
/// Sidecar format version.
const SIDE_VERSION: u32 = 1;

/// Maximum total size of the entries sidecar (defensive cap).
const MAX_ENTRIES_BYTES: usize = 4 << 30;
/// Maximum total size of the path string sidecar (defensive cap).
const MAX_STRINGS_BYTES: usize = 2 << 30;
/// Maximum total size of the offset sidecar (defensive cap).
const MAX_OFFSETS_BYTES: usize = 128 << 20;
/// Maximum total size of the FST sidecar (defensive cap).
const MAX_FST_BYTES: usize = 512 << 20;

/// Maximum size of a single postcard blob for one path (defensive cap).
const MAX_BLOB_BYTES: usize = 1 << 28;

/// Sidecar basename for the path → path-id FST.
pub const FST_FILE: &str = "files.path-id.fst";
/// Entries table filename.
pub const ENTRIES_FILE: &str = "files.path-entries";
/// Path string table filename.
pub const STRINGS_FILE: &str = "files.path-strings";
/// Path string offset table filename.
pub const OFFSETS_FILE: &str = "files.path-offsets";
/// Store-path table filename.
pub const STORE_PATHS_FILE: &str = "files.path-entries.sp";

/// Errors while building or querying the per-path entry cache.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum IndexError {
    /// Secondary index files are missing from the database directory.
    #[error("path entry index missing under {dir}: {detail}")]
    Missing {
        /// Database directory that was searched.
        dir: PathBuf,
        /// Human-readable detail.
        detail: String,
    },

    /// Sidecar magic/version mismatch or truncated payload.
    #[error("path entry index corrupt: {0}")]
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

/// One occurrence of a path inside a single store path.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PathEntryRecord {
    /// Index into the store-path table written alongside this index.
    store_path_idx: u32,
    /// Content-free node (type / executable / symlink target).
    node: FileNode<()>,
}

/// Accumulates full path → `(store path, entry)` mappings while a NIXI database is written.
#[derive(Debug, Default)]
pub struct PathEntryIndexBuilder {
    /// Parallel to `store_path_idx` referenced by [`PathEntryRecord`].
    store_paths: Vec<StorePath>,
    /// path → records contributed by any store path.
    path_map: BTreeMap<Vec<u8>, Vec<PathEntryRecord>>,
}

impl PathEntryIndexBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record every file entry belonging to `store_path`.
    ///
    /// Packages with no entries are skipped.
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
            .map_err(|_| IndexError::Corrupt("too many packages for path entry index".into()))?;
        self.store_paths.push(store_path.clone());
        for entry in entries {
            if entry.path.is_empty() {
                continue;
            }
            self.path_map
                .entry(entry.path.clone())
                .or_default()
                .push(PathEntryRecord {
                    store_path_idx: sp_idx,
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

    /// Number of distinct paths recorded.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.path_map.len()
    }

    /// Iterate the sorted `(path, records)` pairs that will be written.
    ///
    /// The iteration order is the sorted path order used to assign path-ids.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &Vec<PathEntryRecord>)> {
        self.path_map.iter()
    }

    /// Write sidecar files into `db_dir` (the directory that holds `files`).
    ///
    /// # Errors
    ///
    /// Returns an error if any sidecar cannot be written or a build step fails.
    pub fn write_sidecars(&self, db_dir: &Path) -> IndexResult<()> {
        self.write_sidecars_inner(db_dir)
    }

    fn write_sidecars_inner(&self, db_dir: &Path) -> IndexResult<()> {
        let sp_bytes = postcard::to_allocvec(&self.store_paths)
            .map_err(|e| IndexError::Corrupt(format!("serialize store paths: {e}")))?;
        std::fs::write(db_dir.join(STORE_PATHS_FILE), &sp_bytes)?;

        let path_count = self.path_map.len();

        // Build entries sidecar with an inline offset table.
        let mut entries_raw = Vec::new();
        entries_raw.extend_from_slice(ENTRIES_MAGIC);
        entries_raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());

        // Reserve space for count and (count + 1) offsets.
        let header_len = ENTRIES_MAGIC.len() + 4 + 4 + (path_count + 1) * 4;
        entries_raw.resize(header_len, 0);

        let count_u32 = u32::try_from(path_count)
            .map_err(|_| IndexError::Corrupt("path count overflow".into()))?;
        // Write count at offset 8 (version is at offset 4).
        let count_range = ENTRIES_MAGIC.len() + 4..ENTRIES_MAGIC.len() + 8;
        entries_raw
            .get_mut(count_range)
            .ok_or_else(|| IndexError::Corrupt("entries count slice out of range".into()))?
            .copy_from_slice(&count_u32.to_le_bytes());

        let mut fst_builder = fst::MapBuilder::memory();
        let mut string_offsets: Vec<u32> = Vec::with_capacity(path_count);
        let mut strings_raw = Vec::new();
        strings_raw.extend_from_slice(STRINGS_MAGIC);
        strings_raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());

        let mut path_id = 0u32;
        for (path, records) in &self.path_map {
            // Write entries blob and offset.
            let blob = postcard::to_allocvec(records)
                .map_err(|e| IndexError::Corrupt(format!("serialize entries: {e}")))?;
            if blob.len() > MAX_BLOB_BYTES {
                return Err(IndexError::Corrupt("path entry blob too large".into()));
            }

            let start_offset = entries_raw.len();
            entries_raw.extend_from_slice(&blob);
            let end_offset = entries_raw.len();

            let off_idx = ENTRIES_MAGIC.len()
                + 8
                + usize::try_from(path_id)
                    .map_err(|_| IndexError::Corrupt("path-id overflow".into()))?
                    * 4;
            entries_raw
                .get_mut(off_idx..off_idx + 4)
                .ok_or_else(|| IndexError::Corrupt("entry start slice out of range".into()))?
                .copy_from_slice(
                    &u32::try_from(start_offset)
                        .map_err(|_| IndexError::Corrupt("entry start offset overflow".into()))?
                        .to_le_bytes(),
                );
            entries_raw
                .get_mut(off_idx + 4..off_idx + 8)
                .ok_or_else(|| IndexError::Corrupt("entry end slice out of range".into()))?
                .copy_from_slice(
                    &u32::try_from(end_offset)
                        .map_err(|_| IndexError::Corrupt("entry end offset overflow".into()))?
                        .to_le_bytes(),
                );

            // Insert path → path-id into FST.
            fst_builder
                .insert(path, u64::from(path_id))
                .map_err(|err| IndexError::Fst(err.to_string()))?;

            // Write path string and offset.
            let string_off = u32::try_from(strings_raw.len() - (STRINGS_MAGIC.len() + 4))
                .map_err(|_| IndexError::Corrupt("string offset overflow".into()))?;
            string_offsets.push(string_off);
            strings_raw.extend_from_slice(path);
            strings_raw.push(0);

            path_id = path_id
                .checked_add(1)
                .ok_or_else(|| IndexError::Corrupt("path-id overflow".into()))?;
        }

        std::fs::write(db_dir.join(ENTRIES_FILE), &entries_raw)?;

        let fst_bytes = fst_builder
            .into_inner()
            .map_err(|err| IndexError::Fst(err.to_string()))?;
        std::fs::write(db_dir.join(FST_FILE), fst_bytes)?;

        let mut offsets_raw = Vec::new();
        offsets_raw.extend_from_slice(OFFSETS_MAGIC);
        offsets_raw.extend_from_slice(&SIDE_VERSION.to_le_bytes());
        offsets_raw.extend_from_slice(&count_u32.to_le_bytes());
        for off in &string_offsets {
            offsets_raw.extend_from_slice(&off.to_le_bytes());
        }
        std::fs::write(db_dir.join(STRINGS_FILE), &strings_raw)?;
        std::fs::write(db_dir.join(OFFSETS_FILE), &offsets_raw)?;

        Ok(())
    }
}

/// Opened per-path entry cache for exact-path and path-id lookups.
#[derive(Debug)]
pub struct PathEntryIndex {
    // FileData implements AsRef<[u8]>, so fst::Map can use it directly.
    map: Map<mmap_guard::FileData>,
    entries: mmap_guard::FileData,
    strings: mmap_guard::FileData,
    offsets: mmap_guard::FileData,
    store_paths: Vec<StorePath>,
}

impl PathEntryIndex {
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
        for name in [
            FST_FILE,
            ENTRIES_FILE,
            STRINGS_FILE,
            OFFSETS_FILE,
            STORE_PATHS_FILE,
        ] {
            let p = db_dir.join(name);
            if !p.is_file() {
                return Err(IndexError::Missing {
                    dir: db_dir.to_path_buf(),
                    detail: format!("expected {name}"),
                });
            }
        }

        let fst_data = mmap_guard::map_file(db_dir.join(FST_FILE)).map_err(IndexError::Io)?;
        if fst_data.len() > MAX_FST_BYTES {
            return Err(IndexError::Corrupt("fst file too large".into()));
        }
        let map = Map::new(fst_data).map_err(|err| IndexError::Fst(err.to_string()))?;

        let entries = mmap_guard::map_file(db_dir.join(ENTRIES_FILE)).map_err(IndexError::Io)?;
        if entries.len() > MAX_ENTRIES_BYTES {
            return Err(IndexError::Corrupt("entries file too large".into()));
        }
        validate_entries_header(&entries)?;

        let strings = mmap_guard::map_file(db_dir.join(STRINGS_FILE)).map_err(IndexError::Io)?;
        if strings.len() > MAX_STRINGS_BYTES {
            return Err(IndexError::Corrupt("strings file too large".into()));
        }
        validate_strings_header(&strings)?;

        let offsets = mmap_guard::map_file(db_dir.join(OFFSETS_FILE)).map_err(IndexError::Io)?;
        if offsets.len() > MAX_OFFSETS_BYTES {
            return Err(IndexError::Corrupt("offsets file too large".into()));
        }
        validate_offsets_header(&offsets)?;

        let sp_bytes = std::fs::read(db_dir.join(STORE_PATHS_FILE)).map_err(IndexError::Io)?;
        let store_paths: Vec<StorePath> = postcard::from_bytes(&sp_bytes)
            .map_err(|e| IndexError::Corrupt(format!("decode store paths: {e}")))?;

        Ok(Self {
            map,
            entries,
            strings,
            offsets,
            store_paths,
        })
    }

    /// Look up the `path-id` for an exact full path, if present.
    ///
    /// Returns `None` when the path is absent.
    pub fn lookup_path_id(&self, path: &[u8]) -> IndexResult<Option<u32>> {
        match self.map.get(path) {
            Some(v) => u32::try_from(v)
                .map_err(|_| IndexError::Corrupt("fst value overflow".into()))
                .map(Some),
            None => Ok(None),
        }
    }

    /// Return the full path bytes for a `path-id`.
    pub fn path_bytes(&self, path_id: u32) -> IndexResult<&[u8]> {
        let count = read_u32_le(&self.offsets, OFFSETS_MAGIC.len() + 4)?;
        if path_id >= count {
            return Err(IndexError::Corrupt(format!(
                "path-id {path_id} >= count {count}"
            )));
        }
        // Offsets file: magic (4) + version (4) + count (4) + offsets[]
        let off = read_u32_le(
            &self.offsets,
            OFFSETS_MAGIC.len()
                + 8
                + usize::try_from(path_id)
                    .map_err(|_| IndexError::Corrupt("path-id out of range".into()))?
                    * 4,
        )?;
        let base = STRINGS_MAGIC.len() + 4;
        let start = base
            + usize::try_from(off)
                .map_err(|_| IndexError::Corrupt("string offset overflow".into()))?;
        let tail = self
            .strings
            .as_ref()
            .get(start..)
            .ok_or_else(|| IndexError::Corrupt("string start out of bounds".into()))?;
        let end = tail
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| IndexError::Corrupt("unterminated path string".into()))?;
        self.strings
            .as_ref()
            .get(start..start + end)
            .ok_or_else(|| IndexError::Corrupt("string end out of bounds".into()))
    }

    /// Look up every `(StorePath, FileTreeEntry)` for the given path-id.
    ///
    /// Returns an empty vector when the path-id is absent.
    pub fn lookup_entries_by_id(
        &self,
        path_id: u32,
    ) -> IndexResult<Vec<(StorePath, FileTreeEntry)>> {
        let (start, end) = read_entry_offsets(&self.entries, path_id)?;
        let blob = self
            .entries
            .as_ref()
            .get(start..end)
            .ok_or_else(|| IndexError::Corrupt("entry blob out of bounds".into()))?;
        let records: Vec<PathEntryRecord> = postcard::from_bytes(blob)
            .map_err(|e| IndexError::Corrupt(format!("decode entries: {e}")))?;
        let mut out = Vec::with_capacity(records.len());
        let path = self.path_bytes(path_id)?.to_vec();
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
                    path: path.clone(),
                    node: r.node,
                },
            ));
        }
        Ok(out)
    }

    /// Look up every `(StorePath, FileTreeEntry)` whose full path equals `path`.
    ///
    /// Returns an empty vector when the path is absent.
    pub fn lookup_entries(&self, path: &[u8]) -> IndexResult<Vec<(StorePath, FileTreeEntry)>> {
        match self.lookup_path_id(path)? {
            Some(path_id) => self.lookup_entries_by_id(path_id),
            None => Ok(Vec::new()),
        }
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

fn validate_entries_header(entries: &[u8]) -> IndexResult<()> {
    if entries.len() < ENTRIES_MAGIC.len() + 8 {
        return Err(IndexError::Corrupt("entries too short".into()));
    }
    let magic = entries
        .get(..ENTRIES_MAGIC.len())
        .ok_or_else(|| IndexError::Corrupt("entries too short for magic".into()))?;
    if magic != ENTRIES_MAGIC {
        return Err(IndexError::Corrupt(format!(
            "entries magic {magic:?}, expected {ENTRIES_MAGIC:?}"
        )));
    }
    let ver = read_u32_le(entries, ENTRIES_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(IndexError::Corrupt(format!(
            "entries version {ver}, expected {SIDE_VERSION}"
        )));
    }
    let count = read_u32_le(entries, ENTRIES_MAGIC.len() + 4)?;
    let min_len = ENTRIES_MAGIC.len()
        + 8
        + (usize::try_from(count)
            .map_err(|_| IndexError::Corrupt("entry count overflow".into()))?
            + 1)
            * 4;
    if entries.len() < min_len {
        return Err(IndexError::Corrupt("entries file truncated".into()));
    }
    Ok(())
}

fn validate_strings_header(strings: &[u8]) -> IndexResult<()> {
    if strings.len() < STRINGS_MAGIC.len() + 4 {
        return Err(IndexError::Corrupt("strings too short".into()));
    }
    let magic = strings
        .get(..STRINGS_MAGIC.len())
        .ok_or_else(|| IndexError::Corrupt("strings too short for magic".into()))?;
    if magic != STRINGS_MAGIC {
        return Err(IndexError::Corrupt(format!(
            "strings magic {magic:?}, expected {STRINGS_MAGIC:?}"
        )));
    }
    let ver = read_u32_le(strings, STRINGS_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(IndexError::Corrupt(format!(
            "strings version {ver}, expected {SIDE_VERSION}"
        )));
    }
    Ok(())
}

fn validate_offsets_header(offsets: &[u8]) -> IndexResult<()> {
    if offsets.len() < OFFSETS_MAGIC.len() + 8 {
        return Err(IndexError::Corrupt("offsets too short".into()));
    }
    let magic = offsets
        .get(..OFFSETS_MAGIC.len())
        .ok_or_else(|| IndexError::Corrupt("offsets too short for magic".into()))?;
    if magic != OFFSETS_MAGIC {
        return Err(IndexError::Corrupt(format!(
            "offsets magic {magic:?}, expected {OFFSETS_MAGIC:?}"
        )));
    }
    let ver = read_u32_le(offsets, OFFSETS_MAGIC.len())?;
    if ver != SIDE_VERSION {
        return Err(IndexError::Corrupt(format!(
            "offsets version {ver}, expected {SIDE_VERSION}"
        )));
    }
    Ok(())
}

/// Read the `[start, end)` byte offsets for `path_id` from the entries sidecar.
fn read_entry_offsets(entries: &[u8], path_id: u32) -> IndexResult<(usize, usize)> {
    let count = read_u32_le(entries, ENTRIES_MAGIC.len() + 4)?;
    if path_id >= count {
        return Err(IndexError::Corrupt(format!(
            "path-id {path_id} >= count {count}"
        )));
    }
    let base = ENTRIES_MAGIC.len() + 8;
    let idx = base
        + usize::try_from(path_id).map_err(|_| IndexError::Corrupt("path-id overflow".into()))? * 4;
    let start = usize::try_from(read_u32_le(entries, idx)?)
        .map_err(|_| IndexError::Corrupt("entry start offset overflow".into()))?;
    let end = usize::try_from(read_u32_le(entries, idx + 4)?)
        .map_err(|_| IndexError::Corrupt("entry end offset overflow".into()))?;
    if end < start || end > entries.len() {
        return Err(IndexError::Corrupt("entry offset range invalid".into()));
    }
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_path::Origin;

    fn sp(name: &str, hash: &str) -> StorePath {
        StorePath::new(
            "/nix/store".into(),
            hash.into(),
            name.into(),
            Origin {
                attr: name.into(),
                output: "out".into(),
                toplevel: true,
                system: None,
            },
        )
    }

    fn entry(path: &[u8], executable: bool) -> FileTreeEntry {
        FileTreeEntry {
            path: path.to_vec(),
            node: FileNode::Regular {
                size: 42,
                executable,
            },
        }
    }

    #[test]
    fn build_and_lookup_by_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut builder = PathEntryIndexBuilder::new();
        let sp0 = sp(
            "hello",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        let sp1 = sp(
            "coreutils",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );

        builder
            .record_package(&sp0, &[entry(b"/bin/hello", true)])
            .expect("pkg0");
        builder
            .record_package(&sp1, &[entry(b"/bin/hello", true), entry(b"/bin/ls", true)])
            .expect("pkg1");

        builder.write_sidecars(dir.path()).expect("write");

        let index = PathEntryIndex::open(dir.path()).expect("open");
        let hits = index.lookup_entries(b"/bin/hello").expect("lookup");
        assert_eq!(hits.len(), 2);
        let names: Vec<_> = hits.iter().map(|(sp, _)| sp.name().to_string()).collect();
        assert!(names.contains(&"hello".to_string()));
        assert!(names.contains(&"coreutils".to_string()));

        let ls_hits = index.lookup_entries(b"/bin/ls").expect("lookup ls");
        assert_eq!(ls_hits.len(), 1);
        assert_eq!(ls_hits[0].0.name(), "coreutils");
    }
}
