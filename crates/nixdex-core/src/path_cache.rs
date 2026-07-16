//! Persistent cache for fetched store-path file trees.
//!
//! The cache is stored beside the database as `paths.cache`. It is keyed by
//! store-path hash and records the `FileTree`, runtime references, and a
//! timestamp so a rebuild can skip HTTP fetches for unchanged closures.
//!
//! The in-memory representation uses a lock-free [`papaya::HashMap`] so the
//! cache can be shared directly between async fetch workers.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::time::{SystemTime, UNIX_EPOCH};

use ahash::RandomState;
use papaya::HashMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::files::FileTree;
use crate::store_path::StorePath;

const MAGIC: &[u8; 4] = b"NIXP";
const VERSION: u32 = 1;

/// One cached store path together with its parsed file tree and references.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedEntry {
    /// Store path this entry belongs to.
    pub store_path: StorePath,
    /// Parsed `.ls` tree, if one was successfully fetched.
    pub tree: Option<FileTree>,
    /// Runtime references from the narinfo, if successfully fetched.
    pub refs: Option<Vec<StorePath>>,
    /// Unix timestamp when the entry was created or updated.
    pub fetched_at: u64,
}

impl CachedEntry {
    /// Create a new entry for `store_path` with no tree or refs yet.
    #[must_use]
    pub fn new(store_path: StorePath) -> Self {
        Self {
            store_path,
            tree: None,
            refs: None,
            fetched_at: now_secs(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    cache_key: String,
    entries: Vec<CachedEntry>,
}

/// In-memory path cache keyed by store-path hash.
///
/// `PathCache` is `Sync` and is intended to be shared via `Arc` across the
/// fetch worker pool. Lookups and inserts are lock-free.
#[derive(Debug)]
pub struct PathCache {
    cache_key: String,
    map: HashMap<String, CachedEntry, RandomState>,
    /// Number of successful cache lookups during this build.
    pub hits: AtomicUsize,
}

/// Errors that can occur when reading or writing a path cache.
#[derive(Error, Debug)]
pub enum Error {
    /// Local filesystem I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Serialization or deserialization failed.
    #[error("postcard error: {0}")]
    Postcard(String),
}

/// Convenience alias used throughout the cache module.
pub type Result<T> = std::result::Result<T, Error>;

fn now_secs() -> u64 {
    if let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) {
        duration.as_secs()
    } else {
        0
    }
}

impl PathCache {
    /// Create an empty cache identified by `cache_key`.
    ///
    /// The key is written into the cache file and checked on load; a mismatch
    /// treats the cache as stale and causes a rebuild.
    #[must_use]
    pub fn new(cache_key: impl Into<String>) -> Self {
        Self {
            cache_key: cache_key.into(),
            map: HashMap::with_hasher(RandomState::new()),
            hits: AtomicUsize::new(0),
        }
    }

    /// Return the cache key this cache was loaded or created with.
    #[must_use]
    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    /// Load a cache from `path`, returning `Ok(None)` if the file is missing,
    /// the header magic/version are wrong, or the stored `cache_key` does not
    /// match `expected_key`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] or [`Error::Postcard`] for unreadable or corrupt
    /// files beyond a simple mismatch.
    pub fn load(path: &Path, expected_key: &str) -> Result<Option<Self>> {
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };

        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Ok(None);
        }

        let mut version_bytes = [0u8; 4];
        file.read_exact(&mut version_bytes)?;
        let version = u32::from_le_bytes(version_bytes);
        if version != VERSION {
            return Ok(None);
        }

        let mut payload_bytes = Vec::new();
        file.read_to_end(&mut payload_bytes)?;
        let payload: Payload =
            postcard::from_bytes(&payload_bytes).map_err(|e| Error::Postcard(e.to_string()))?;

        if payload.cache_key != expected_key {
            return Ok(None);
        }

        let cache = Self::new(expected_key);
        for entry in payload.entries {
            let key = entry.store_path.hash().to_string();
            let _ = cache.map.pin().insert(key, entry);
        }
        Ok(Some(cache))
    }

    /// Save this cache to `path` atomically via a temporary file and rename.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] or [`Error::Postcard`] on failure.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("cache.tmp");
        {
            let file = File::create(&tmp)?;
            let mut writer = BufWriter::new(file);
            writer.write_all(MAGIC)?;
            writer.write_all(&VERSION.to_le_bytes())?;
            let entries: Vec<CachedEntry> = self
                .map
                .pin()
                .iter()
                .map(|(_, entry)| entry.clone())
                .collect();
            let payload = Payload {
                cache_key: self.cache_key.clone(),
                entries,
            };
            let bytes =
                postcard::to_stdvec(&payload).map_err(|e| Error::Postcard(e.to_string()))?;
            writer.write_all(&bytes)?;
            writer.flush()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Look up an entry by store-path hash, returning a cloned copy if found.
    #[must_use]
    pub fn get(&self, hash: &str) -> Option<CachedEntry> {
        self.map.pin().get(hash).cloned()
    }

    /// Look up the cached narinfo references for a store-path hash.
    #[must_use]
    pub fn get_refs(&self, hash: &str) -> Option<Vec<StorePath>> {
        self.map
            .pin()
            .get(hash)
            .and_then(|entry| entry.refs.clone())
    }

    /// Look up the cached `.ls` tree for a store-path hash.
    #[must_use]
    pub fn get_tree(&self, hash: &str) -> Option<FileTree> {
        self.map
            .pin()
            .get(hash)
            .and_then(|entry| entry.tree.clone())
    }

    /// Insert or replace an entry keyed by its store-path hash.
    pub fn insert(&self, hash: impl Into<String>, entry: CachedEntry) {
        let _ = self.map.pin().insert(hash.into(), entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::FileTree;
    use crate::store_path::{Origin, StorePath};

    fn sample_path() -> StorePath {
        let origin = Origin {
            attr: "hello".into(),
            output: "out".into(),
            toplevel: true,
            system: None,
        };
        StorePath::parse(origin, "/nix/store/abc123hello-2.12.1/bin/hello")
            .expect("valid store path")
    }

    #[test]
    fn roundtrip_saves_and_loads_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("paths.cache");

        let sp = sample_path();
        let cache = PathCache::new("<nixpkgs>");
        let mut entry = CachedEntry::new(sp.clone());
        entry.tree = Some(FileTree::directory(vec![]));
        entry.refs = Some(vec![]);
        cache.insert(sp.hash(), entry);
        cache.save(&path).expect("save");

        let loaded = PathCache::load(&path, "<nixpkgs>")
            .expect("load")
            .expect("cache exists");
        assert_eq!(loaded.cache_key(), "<nixpkgs>");
        let found = loaded.get(sp.hash()).expect("entry");
        assert_eq!(found.store_path.hash(), sp.hash());
        assert!(found.tree.is_some());
        assert!(found.refs.is_some());
    }

    #[test]
    fn mismatched_cache_key_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("paths.cache");

        let sp = sample_path();
        let cache = PathCache::new("key-a");
        let hash = sp.hash().to_string();
        cache.insert(hash, CachedEntry::new(sp));
        cache.save(&path).expect("save");

        assert!(PathCache::load(&path, "key-b").expect("load").is_none());
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nonexistent.cache");

        assert!(PathCache::load(&path, "key").expect("load").is_none());
    }

    #[test]
    fn corrupt_magic_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("paths.cache");
        std::fs::write(&path, b"BAD0").expect("write");

        assert!(PathCache::load(&path, "key").expect("load").is_none());
    }
}
