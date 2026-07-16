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
    /// Check if this entry is expired given a TTL in seconds.
    #[must_use]
    pub fn is_expired(&self, ttl_secs: u64) -> bool {
        let now = now_secs();
        let age = now.saturating_sub(self.fetched_at);
        age > ttl_secs
    }

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
    ttl_secs: u64,
    entries: Vec<CachedEntry>,
}

/// Borrowed view of a [`Payload`] used for serialization without cloning entries.
#[derive(Serialize)]
struct SerializePayload<'a> {
    cache_key: &'a str,
    ttl_secs: u64,
    entries: Vec<&'a CachedEntry>,
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
    /// Time-to-live for cache entries in seconds (0 = no expiry).
    ttl_secs: u64,
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
    ///
    /// `ttl_secs` is the time-to-live for cache entries in seconds (0 = no expiry).
    #[must_use]
    pub fn new(cache_key: impl Into<String>) -> Self {
        Self {
            cache_key: cache_key.into(),
            map: HashMap::with_hasher(RandomState::new()),
            hits: AtomicUsize::new(0),
            ttl_secs: 7 * 24 * 60 * 60, // 7 days default
        }
    }

    /// Create an empty cache with a custom TTL.
    ///
    /// `ttl_secs` is the time-to-live for cache entries in seconds (0 = no expiry).
    #[must_use]
    pub fn new_with_ttl(cache_key: impl Into<String>, ttl_secs: u64) -> Self {
        Self {
            cache_key: cache_key.into(),
            map: HashMap::with_hasher(RandomState::new()),
            hits: AtomicUsize::new(0),
            ttl_secs,
        }
    }

    /// Return the cache key this cache was loaded or created with.
    #[must_use]
    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    /// Return the configured TTL in seconds (0 = no expiry).
    #[must_use]
    pub fn ttl_secs(&self) -> u64 {
        self.ttl_secs
    }

    /// Load a cache from `path`, returning `Ok(None)` if the file is missing,
    /// the header magic/version are wrong, or the stored `cache_key` does not
    /// match `expected_key`.
    ///
    /// Expired entries are filtered out based on the stored TTL.
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

        let cache = Self::new_with_ttl(expected_key, payload.ttl_secs);
        let total_entries = payload.entries.len();
        let mut expired_count = 0usize;
        {
            let map = cache.map.pin();
            for entry in payload.entries {
                if entry.is_expired(cache.ttl_secs) {
                    expired_count += 1;
                    continue;
                }
                let key = entry.store_path.hash().to_string();
                let _ = map.insert(key, entry);
            }
        }
        if expired_count > 0 {
            tracing::info!(
                expired_count,
                total = total_entries,
                "filtered expired entries from path cache"
            );
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
            let map = self.map.pin();
            let entries: Vec<&CachedEntry> = map.iter().map(|(_, entry)| entry).collect();
            let payload = SerializePayload {
                cache_key: &self.cache_key,
                ttl_secs: self.ttl_secs,
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

    /// Look up an entry by store-path hash, returning a cloned copy if found and not expired.
    #[must_use]
    pub fn get(&self, hash: &str) -> Option<CachedEntry> {
        let map = self.map.pin();
        let entry = map.get(hash)?;
        if entry.is_expired(self.ttl_secs) {
            return None;
        }
        Some(entry.clone())
    }

    /// Look up the cached narinfo references for a store-path hash.
    #[must_use]
    pub fn get_refs(&self, hash: &str) -> Option<Vec<StorePath>> {
        let map = self.map.pin();
        let entry = map.get(hash)?;
        if entry.is_expired(self.ttl_secs) {
            return None;
        }
        entry.refs.clone()
    }

    /// Look up the cached `.ls` tree for a store-path hash.
    #[must_use]
    pub fn get_tree(&self, hash: &str) -> Option<FileTree> {
        let map = self.map.pin();
        let entry = map.get(hash)?;
        if entry.is_expired(self.ttl_secs) {
            return None;
        }
        entry.tree.clone()
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
        assert_eq!(loaded.ttl_secs(), 7 * 24 * 60 * 60);
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

    #[test]
    fn wrong_version_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("paths.cache");

        let sp = sample_path();
        let cache = PathCache::new("key");
        cache.insert(sp.hash().to_string(), CachedEntry::new(sp));
        cache.save(&path).expect("save");

        // Corrupt the version field
        let mut data = std::fs::read(&path).expect("read");
        let version_slice = data.get_mut(4..8).expect("version slice");
        version_slice.copy_from_slice(&999u32.to_le_bytes());
        std::fs::write(&path, &data).expect("write");

        assert!(PathCache::load(&path, "key").expect("load").is_none());
    }

    #[test]
    fn cache_key_derivation() {
        let cache_a = PathCache::new("nixpkgs-21.11");
        let cache_b = PathCache::new("nixpkgs-22.05");

        assert_eq!(cache_a.cache_key(), "nixpkgs-21.11");
        assert_eq!(cache_b.cache_key(), "nixpkgs-22.05");
        assert_ne!(cache_a.cache_key(), cache_b.cache_key());
    }

    #[test]
    fn empty_cache_lookups_return_none() {
        let cache = PathCache::new("test");
        assert!(cache.get("nonexistent").is_none());
        assert!(cache.get_refs("nonexistent").is_none());
        assert!(cache.get_tree("nonexistent").is_none());
    }

    #[test]
    fn insert_and_retrieve_entry() {
        let cache = PathCache::new("test");
        let sp = sample_path();
        let hash = sp.hash().to_string();

        let mut entry = CachedEntry::new(sp.clone());
        entry.tree = Some(FileTree::regular(100, true));
        entry.refs = Some(vec![]);

        cache.insert(hash.clone(), entry.clone());

        let retrieved = cache.get(&hash).expect("entry");
        assert_eq!(retrieved.store_path.hash(), sp.hash());
        assert!(retrieved.tree.is_some());
        assert!(retrieved.refs.is_some());
    }

    #[test]
    fn corrupt_postcard_data_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("paths.cache");

        // Write valid magic and version, then corrupt the payload
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&VERSION.to_le_bytes());
        data.extend_from_slice(b"invalid postcard data");

        std::fs::write(&path, &data).expect("write");

        let result = PathCache::load(&path, "key");
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::Postcard(_))));
    }

    #[test]
    fn cached_entry_timestamp_is_set() {
        let sp = sample_path();
        let entry = CachedEntry::new(sp);
        // Timestamp should be reasonably recent (within last minute)
        let now = now_secs();
        assert!(entry.fetched_at <= now);
        assert!(entry.fetched_at > now.saturating_sub(60));
    }

    #[test]
    fn expired_entries_are_filtered_on_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("paths.cache");

        let sp = sample_path();
        let cache = PathCache::new_with_ttl("key", 1); // 1 second TTL
        let mut entry = CachedEntry::new(sp.clone());
        entry.tree = Some(FileTree::directory(vec![]));
        entry.fetched_at = now_secs().saturating_sub(10); // 10 seconds ago
        cache.insert(sp.hash(), entry);
        cache.save(&path).expect("save");

        let loaded = PathCache::load(&path, "key")
            .expect("load")
            .expect("cache exists");
        assert!(loaded.get(sp.hash()).is_none());
    }

    #[test]
    fn expired_entries_are_filtered_on_lookup() {
        let sp = sample_path();
        let cache = PathCache::new_with_ttl("key", 1); // 1 second TTL
        let mut entry = CachedEntry::new(sp.clone());
        entry.tree = Some(FileTree::directory(vec![]));
        entry.fetched_at = now_secs().saturating_sub(10); // 10 seconds ago
        cache.insert(sp.hash(), entry);

        assert!(cache.get(sp.hash()).is_none());
        assert!(cache.get_tree(sp.hash()).is_none());
        assert!(cache.get_refs(sp.hash()).is_none());
    }
}
