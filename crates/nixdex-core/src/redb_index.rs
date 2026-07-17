//! redb-backed file index and exact-path cache.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use redb as redb_db;
use redb_db::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::basename_index::basename_of;
use crate::files::{FileTree, FileTreeEntry};
use crate::store_path::StorePath;

/// Siblings of the NIXI, `files`, database.
pub const DEFAULT_FILE: &str = "files.redb";
/// Postcard sidecar for exact store-relative path lookups.
pub const PATH_CACHE_FILE: &str = "files.pathcache";

/// Key format: `{attr}.{output}` to distinguish multiple origins for the same hash.
const PACKAGES: TableDefinition<&str, &[u8]> = TableDefinition::new("packages");
const BASENAMES: TableDefinition<&str, &[u8]> = TableDefinition::new("basenames");

#[derive(Error, Debug)]
pub enum Error {
    #[error("redb error: {0}")]
    Redb(String),

    #[error("index I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("postcard error: {0}")]
    Postcard(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Serialize, Deserialize)]
struct Package {
    store_path: StorePath,
    entries: Vec<SerializableEntry>,
}

/// Serializable form of FileTreeEntry for redb storage.
#[derive(Serialize, Deserialize)]
struct SerializableEntry {
    path: Vec<u8>,
    node: SerializableNode,
}

/// Serializable form of FileNode for redb storage.
#[derive(Serialize, Deserialize)]
enum SerializableNode {
    Regular { size: u64, executable: bool },
    Symlink { target: Vec<u8> },
    Directory { size: u64 },
}

impl From<FileTreeEntry> for SerializableEntry {
    fn from(entry: FileTreeEntry) -> Self {
        let node = match entry.node {
            crate::files::FileNode::Regular { size, executable } => {
                SerializableNode::Regular { size, executable }
            }
            crate::files::FileNode::Symlink { target } => SerializableNode::Symlink {
                target: target.to_vec(),
            },
            crate::files::FileNode::Directory { size, contents: () } => {
                SerializableNode::Directory { size }
            }
        };
        Self {
            path: entry.path,
            node,
        }
    }
}

impl TryFrom<SerializableEntry> for FileTreeEntry {
    type Error = String;

    fn try_from(value: SerializableEntry) -> std::result::Result<Self, Self::Error> {
        let node = match value.node {
            SerializableNode::Regular { size, executable } => {
                crate::files::FileNode::Regular { size, executable }
            }
            SerializableNode::Symlink { target } => crate::files::FileNode::Symlink {
                target: target.into(),
            },
            SerializableNode::Directory { size } => {
                crate::files::FileNode::Directory { size, contents: () }
            }
        };
        Ok(Self {
            path: value.path,
            node,
        })
    }
}

/// Writes the redb index and the postcard path-cache sidecar.
pub struct Writer {
    database: Database,
    path_cache: BTreeMap<Vec<u8>, Vec<String>>,
    path_cache_path: PathBuf,
}

impl Writer {
    /// Create a redb index next to a NIXI, `files`, database.
    pub fn create(db_dir: &Path) -> Result<Self> {
        let db_path = db_dir.join(DEFAULT_FILE);
        let database = Database::create(&db_path).map_err(|err| Error::Redb(err.to_string()))?;
        Ok(Self {
            database,
            path_cache: BTreeMap::new(),
            path_cache_path: db_dir.join(PATH_CACHE_FILE),
        })
    }

    /// Add one store path and its already-filtered file entries to the index.
    pub fn add(&mut self, store_path: &StorePath, entries: &[FileTreeEntry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let origin_key = format!(
            "{}.{}",
            store_path.origin().attr,
            store_path.origin().output
        );
        let serializable_entries: Vec<SerializableEntry> =
            entries.iter().cloned().map(Into::into).collect();
        let package = Package {
            store_path: store_path.clone(),
            entries: serializable_entries,
        };
        let package =
            postcard::to_stdvec(&package).map_err(|err| Error::Postcard(err.to_string()))?;
        let write = self.database.begin_write().map_err(self_redb_error)?;
        {
            let mut packages = write.open_table(PACKAGES).map_err(self_redb_error)?;
            packages
                .insert(origin_key.as_str(), package.as_slice())
                .map_err(self_redb_error)?;
        }
        {
            let mut basenames = write.open_table(BASENAMES).map_err(self_redb_error)?;
            for entry in &entries {
                let basename = basename_of(&entry.path);
                if basename.is_empty() {
                    continue;
                }
                let basename_str = String::from_utf8_lossy(basename).to_string();
                let mut origins = match basenames
                    .get(basename_str.as_str())
                    .map_err(self_redb_error)?
                {
                    Some(value) => postcard::from_bytes(value.value())
                        .map_err(|err| Error::Postcard(err.to_string()))?,
                    None => Vec::new(),
                };
                if !origins.iter().any(|origin| origin == &origin_key) {
                    origins.push(origin_key.clone());
                }
                let origins = postcard::to_stdvec(&origins)
                    .map_err(|err| Error::Postcard(err.to_string()))?;
                basenames
                    .insert(basename_str.as_str(), origins.as_slice())
                    .map_err(self_redb_error)?;
            }
        }
        write.commit().map_err(self_redb_error)?;

        for entry in entries {
            self.path_cache
                .entry(entry.path.clone())
                .or_default()
                .push(origin_key.clone());
        }
        Ok(())
    }

    /// Finalize the path cache sidecar.
    pub fn finish(self) -> Result<()> {
        let bytes = postcard::to_stdvec(&self.path_cache)
            .map_err(|err| Error::Postcard(err.to_string()))?;
        fs::write(self.path_cache_path, bytes)?;
        Ok(())
    }
}

/// Reader for the optional redb-backed index.
pub struct Reader {
    database: Database,
    path_cache: Option<BTreeMap<Vec<u8>, Vec<String>>>,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("database", &"<redb::Database>")
            .field("path_cache", &self.path_cache)
            .finish()
    }
}

impl Reader {
    /// Open the optional sidecars in `db_dir`.
    pub fn open(db_dir: &Path) -> Result<Self> {
        let database = Database::open(db_dir.join(DEFAULT_FILE)).map_err(self_redb_error)?;
        let path_cache = fs::read(db_dir.join(PATH_CACHE_FILE))
            .ok()
            .and_then(|bytes| postcard::from_bytes(&bytes).ok());
        Ok(Self {
            database,
            path_cache,
        })
    }

    /// Return the package recorded for the origin key (`attr.output`).
    pub fn package_by_origin(
        &self,
        origin: &str,
    ) -> Result<Option<(StorePath, Vec<FileTreeEntry>)>> {
        let read = self.database.begin_read().map_err(self_redb_error)?;
        let packages = read.open_table(PACKAGES).map_err(self_redb_error)?;
        let package = packages
            .get(origin)
            .map_err(self_redb_error)?
            .map(|value| {
                postcard::from_bytes::<Package>(value.value())
                    .map_err(|err| Error::Postcard(err.to_string()))
            })
            .transpose()?;
        match package {
            Some(pkg) => {
                let entries: std::result::Result<Vec<FileTreeEntry>, String> =
                    pkg.entries.into_iter().map(TryInto::try_into).collect();
                let entries = entries.map_err(Error::Postcard)?;
                Ok(Some((pkg.store_path, entries)))
            }
            None => Ok(None),
        }
    }

    /// Return origin keys of packages containing an exact basename.
    pub fn origins_for_basename(&self, basename: &str) -> Result<Vec<String>> {
        let read = self.database.begin_read().map_err(self_redb_error)?;
        let basenames = read.open_table(BASENAMES).map_err(self_redb_error)?;
        match basenames.get(basename).map_err(self_redb_error)? {
            Some(value) => {
                let origins = postcard::from_bytes::<Vec<String>>(value.value())
                    .map_err(|err| Error::Postcard(err.to_string()))?;
                Ok(origins)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Return exact-path hits when the postcard sidecar is available.
    pub fn exact_path_entries(
        &self,
        path: &[u8],
    ) -> Result<Option<Vec<(StorePath, FileTreeEntry)>>> {
        let Some(origins) = self.path_cache.as_ref().and_then(|cache| cache.get(path)) else {
            return Ok(None);
        };
        let mut hits = Vec::with_capacity(origins.len());
        for origin in origins {
            if let Some((store_path, entries)) = self.package_by_origin(origin)? {
                for entry in entries {
                    if entry.path == path {
                        hits.push((store_path.clone(), entry));
                    }
                }
            }
        }
        Ok(Some(hits))
    }
}

fn self_redb_error<E: std::fmt::Display>(error: E) -> Error {
    Error::Redb(error.to_string())
}
