//! Recursive closure traversal for binary-cache `.ls` listings.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use bytes::Bytes;
use scc::HashMap as SccHashMap;
use scc::hash_map::Entry as SccMapEntry;
use tokio::sync::{Mutex, Notify, Semaphore, mpsc};
use tracing::warn;

use crate::errors::Result;
use crate::files::FileTree;
use crate::hydra::{self, Fetcher};
use crate::path_cache::PathCache;
use crate::store_path::StorePath;

/// A root path together with an optional `meta.mainProgram` value.
///
/// When a root path's `.ls` listing is missing from the binary cache, the
/// `main_program` value is used to synthesize a `/bin/<mainProgram>` entry so
/// that `nix-locate` can still find the package's primary executable.
#[derive(Debug, Clone)]
pub struct PackageEntry {
    /// Store path to fetch/traverse.
    pub path: StorePath,
    /// Value of `meta.mainProgram` for this path, if any.
    pub main_program: Option<String>,
}

impl PackageEntry {
    /// Create a new entry with no `main_program`.
    #[must_use]
    pub fn new(path: StorePath) -> Self {
        Self {
            path,
            main_program: None,
        }
    }
}

/// Result item produced by [`fetch_listings`].
pub type ListingItem = (StorePath, FileTree);

/// Return `true` if `name` is a single, safe filename component.
fn is_valid_main_program(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    !name.contains(['/', '\\', '\0'])
}

/// Build a synthetic file tree containing just `/bin/<main_program>`.
fn synthesize_main_program(main_program: &str) -> FileTree {
    FileTree::directory(vec![(
        Bytes::from_static(b"bin"),
        FileTree::directory(vec![(
            Bytes::copy_from_slice(main_program.as_bytes()),
            FileTree::regular(0, true),
        )]),
    )])
}

/// Internal abstraction so the closure fetcher can be tested without HTTP.
#[allow(clippy::manual_async_fn)]
trait ListingSource: Clone + Send + Sync + 'static {
    fn fetch_narinfo_details<'a>(
        &'a self,
        path: &'a StorePath,
    ) -> impl std::future::Future<
        Output = std::result::Result<(Vec<StorePath>, Option<String>), hydra::Error>,
    > + Send;

    fn fetch_file_tree<'a>(
        &'a self,
        path: &'a StorePath,
    ) -> impl std::future::Future<Output = std::result::Result<FileTree, hydra::Error>> + Send;
}

#[allow(clippy::manual_async_fn)]
impl ListingSource for Fetcher {
    fn fetch_narinfo_details<'a>(
        &'a self,
        path: &'a StorePath,
    ) -> impl std::future::Future<
        Output = std::result::Result<(Vec<StorePath>, Option<String>), hydra::Error>,
    > + Send {
        async move { self.fetch_narinfo_details(path).await }
    }

    fn fetch_file_tree<'a>(
        &'a self,
        path: &'a StorePath,
    ) -> impl std::future::Future<Output = std::result::Result<FileTree, hydra::Error>> + Send {
        async move { self.fetch_file_tree(path).await }
    }
}

/// A [`ListingSource`] that consults a `paths.cache` sidecar before hitting the
/// network and persists new results back into the cache.
#[derive(Debug, Clone)]
struct CachedSource {
    inner: Fetcher,
    cache: Arc<PathCache>,
}

#[allow(clippy::manual_async_fn)]
impl ListingSource for CachedSource {
    fn fetch_narinfo_details<'a>(
        &'a self,
        path: &'a StorePath,
    ) -> impl std::future::Future<
        Output = std::result::Result<(Vec<StorePath>, Option<String>), hydra::Error>,
    > + Send {
        async move {
            let hash = path.hash();
            if let Some(refs) = self.cache.get_refs(hash) {
                self.cache.hits.fetch_add(1, Ordering::Relaxed);
                return Ok((refs, None));
            }

            let (refs, nar_url) = self.inner.fetch_narinfo_details(path).await?;

            // Preserve any already-cached tree for this path while storing refs.
            let mut entry = crate::path_cache::CachedEntry::new(path.clone());
            entry.tree = self.cache.get_tree(hash);
            entry.refs = Some(refs.clone());
            self.cache.insert(hash, entry);

            Ok((refs, nar_url))
        }
    }

    fn fetch_file_tree<'a>(
        &'a self,
        path: &'a StorePath,
    ) -> impl std::future::Future<Output = std::result::Result<FileTree, hydra::Error>> + Send {
        async move {
            let hash = path.hash();
            if let Some(tree) = self.cache.get_tree(hash) {
                self.cache.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(tree);
            }

            let tree = self.inner.fetch_file_tree(path).await?;

            let mut entry = crate::path_cache::CachedEntry::new(path.clone());
            entry.refs = self.cache.get_refs(hash);
            entry.tree = Some(tree.clone());
            self.cache.insert(hash, entry);

            Ok(tree)
        }
    }
}

/// Fetch `.ls` listings recursively over the package stream from `input`.
///
/// The returned receiver yields every store path whose `.ls` listing could be
/// fetched and parsed. Missing `.ls` files or missing narinfos are skipped
/// silently; other errors are logged with [`tracing::warn`] and skipped.
///
/// For root entries that carry a `main_program` value, a missing `.ls` or
/// narinfo causes a synthetic `/bin/<main_program>` listing to be emitted
/// instead of being skipped.
///
/// When `path_cache` is `Some`, fetched narinfo references and `.ls` trees are
/// looked up from the cache before making HTTP requests and written back on
/// misses.
///
/// Concurrency is bounded by `jobs` (clamped to at least 1).
pub async fn fetch_listings(
    fetcher: &Fetcher,
    jobs: usize,
    input: mpsc::Receiver<PackageEntry>,
    path_cache: Option<Arc<PathCache>>,
) -> Result<mpsc::Receiver<Result<ListingItem>>> {
    if let Some(cache) = path_cache {
        let source = CachedSource {
            inner: fetcher.clone(),
            cache,
        };
        fetch_listings_with_source(&source, jobs, input).await
    } else {
        fetch_listings_with_source(fetcher, jobs, input).await
    }
}

async fn fetch_listings_with_source<S: ListingSource>(
    source: &S,
    jobs: usize,
    mut input: mpsc::Receiver<PackageEntry>,
) -> Result<mpsc::Receiver<Result<ListingItem>>> {
    let jobs = jobs.max(1);
    let (out_tx, out_rx) = mpsc::channel::<Result<ListingItem>>(jobs * 2);
    let queue: Arc<Mutex<VecDeque<PackageEntry>>> = Arc::new(Mutex::new(VecDeque::new()));
    let seen: Arc<SccHashMap<String, Option<String>, ahash::RandomState>> =
        Arc::new(SccHashMap::with_hasher(ahash::RandomState::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let input_done = Arc::new(AtomicBool::new(false));
    let semaphore = Arc::new(Semaphore::new(jobs));

    // Feed incoming root entries into the shared queue. Workers also push
    // newly discovered references into the same queue.
    let notify_for_feeder = Arc::clone(&notify);
    let queue_for_feeder = Arc::clone(&queue);
    let seen_for_feeder = Arc::clone(&seen);
    let in_flight_for_feeder = Arc::clone(&in_flight);
    let input_done_for_feeder = Arc::clone(&input_done);
    tokio::spawn(async move {
        while let Some(entry) = input.recv().await {
            let hash = entry.path.hash().to_string();
            let is_new = match seen_for_feeder.entry_sync(hash) {
                SccMapEntry::Occupied(mut occupied) => {
                    if occupied.get().is_none() && entry.main_program.is_some() {
                        occupied.insert(entry.main_program.clone());
                    }
                    false
                }
                SccMapEntry::Vacant(vacant) => {
                    vacant.insert_entry(entry.main_program.clone());
                    true
                }
            };
            if is_new {
                in_flight_for_feeder.fetch_add(1, Ordering::SeqCst);
                queue_for_feeder.lock().await.push_back(entry);
                notify_for_feeder.notify_one();
            }
        }
        input_done_for_feeder.store(true, Ordering::SeqCst);
        notify_for_feeder.notify_one();
    });

    let source = source.clone();
    tokio::spawn(async move {
        let source = source;
        loop {
            let entry = {
                let mut q = queue.lock().await;
                q.pop_front()
            };

            if let Some(entry) = entry {
                let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    notify.notify_one();
                    continue;
                };

                let source = source.clone();
                let queue = Arc::clone(&queue);
                let seen = Arc::clone(&seen);
                let in_flight = Arc::clone(&in_flight);
                let notify = Arc::clone(&notify);
                let out_tx = out_tx.clone();

                tokio::spawn(async move {
                    let _permit = permit;
                    if let Some(item) =
                        process_path(&source, &entry, &queue, &seen, &in_flight, &notify).await
                    {
                        let _ = out_tx.send(Ok(item)).await;
                    }
                });
            } else {
                let done =
                    input_done.load(Ordering::SeqCst) && in_flight.load(Ordering::SeqCst) == 0;
                if done {
                    break;
                }
                notify.notified().await;
            }
        }
    });

    Ok(out_rx)
}

async fn process_path<S: ListingSource>(
    source: &S,
    entry: &PackageEntry,
    queue: &Mutex<VecDeque<PackageEntry>>,
    seen: &SccHashMap<String, Option<String>, ahash::RandomState>,
    in_flight: &AtomicUsize,
    notify: &Notify,
) -> Option<ListingItem> {
    let result = process_path_inner(source, entry, queue, seen, in_flight, notify).await;

    let remaining = in_flight.fetch_sub(1, Ordering::SeqCst);
    if remaining == 1 {
        notify.notify_one();
    }

    result
}

#[allow(clippy::cognitive_complexity)]
async fn process_path_inner<S: ListingSource>(
    source: &S,
    entry: &PackageEntry,
    queue: &Mutex<VecDeque<PackageEntry>>,
    seen: &SccHashMap<String, Option<String>, ahash::RandomState>,
    in_flight: &AtomicUsize,
    notify: &Notify,
) -> Option<ListingItem> {
    let path = &entry.path;
    let main_program_string = entry.main_program.clone().or_else(|| {
        seen.read_sync(&path.hash().to_string(), |_, mp| mp.clone())
            .flatten()
    });
    let main_program = main_program_string
        .as_deref()
        .filter(|mp| is_valid_main_program(mp));

    let refs = match source.fetch_narinfo_details(path).await {
        Ok((refs, _nar_url)) => refs,
        Err(err) => {
            if err.is_not_found() {
                if let Some(mp) = main_program {
                    return Some((path.clone(), synthesize_main_program(mp)));
                }
            } else {
                warn!(path = %path, error = %err, "failed to fetch narinfo; skipping");
            }
            return None;
        }
    };

    for r in refs {
        let hash = r.hash().to_string();
        let is_new = match seen.entry_sync(hash) {
            SccMapEntry::Occupied(_) => false,
            SccMapEntry::Vacant(vacant) => {
                vacant.insert_entry(None);
                true
            }
        };
        if is_new {
            in_flight.fetch_add(1, Ordering::SeqCst);
            queue.lock().await.push_back(PackageEntry::new(r));
            notify.notify_one();
        }
    }

    match source.fetch_file_tree(path).await {
        Ok(tree) => Some((path.clone(), tree)),
        Err(err) => {
            if err.is_not_found() {
                main_program.map(|mp| (path.clone(), synthesize_main_program(mp)))
            } else {
                warn!(path = %path, error = %err, "failed to fetch .ls listing; skipping");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{FileNode, FileTreeEntry};
    use bytes::Bytes;

    #[derive(Clone, Default)]
    struct MockSource {
        refs: indexmap::IndexMap<String, Vec<StorePath>>,
        trees: indexmap::IndexMap<String, FileTree>,
        missing_narinfo: indexmap::IndexSet<String>,
        missing_ls: indexmap::IndexSet<String>,
    }

    impl ListingSource for MockSource {
        async fn fetch_narinfo_details(
            &self,
            path: &StorePath,
        ) -> std::result::Result<(Vec<StorePath>, Option<String>), hydra::Error> {
            if self.missing_narinfo.contains(path.hash()) {
                return Err(hydra::Error::Request("HTTP 404".to_string()));
            }
            let refs = self.refs.get(path.hash()).cloned().unwrap_or_else(Vec::new);
            let url = Some(format!("nar/{}.nar.xz", path.hash()));
            Ok((refs, url))
        }

        async fn fetch_file_tree(
            &self,
            path: &StorePath,
        ) -> std::result::Result<FileTree, hydra::Error> {
            if self.missing_ls.contains(path.hash()) {
                return Err(hydra::Error::Request("HTTP 404".to_string()));
            }
            self.trees
                .get(path.hash())
                .cloned()
                .ok_or_else(|| hydra::Error::Request(format!("missing .ls for {}", path.hash())))
        }
    }

    fn sp(hash: &str, name: &str) -> StorePath {
        StorePath::new(
            "/nix/store".to_string(),
            hash.to_string(),
            name.to_string(),
            crate::store_path::Origin {
                attr: String::new(),
                output: "out".to_string(),
                toplevel: false,
                system: None,
            },
        )
    }

    fn empty_tree() -> FileTree {
        FileTree::directory(Vec::new())
    }

    fn input_channel(entries: Vec<PackageEntry>) -> mpsc::Receiver<PackageEntry> {
        let (tx, rx) = mpsc::channel(entries.len());
        for entry in entries {
            tx.try_send(entry).expect("enqueue test entry");
        }
        rx
    }

    fn leaf(name: &[u8]) -> FileTree {
        FileTree::directory(vec![(Bytes::copy_from_slice(name), empty_tree())])
    }

    #[tokio::test]
    async fn closure_fetcher_visits_all_reachable_paths() {
        let a = sp("a", "a");
        let b = sp("b", "b");
        let c = sp("c", "c");

        let mut refs = indexmap::IndexMap::new();
        refs.insert("a".to_string(), vec![b.clone()]);
        refs.insert("b".to_string(), vec![c.clone()]);
        refs.insert("c".to_string(), vec![]);

        let mut trees = indexmap::IndexMap::new();
        trees.insert("a".to_string(), leaf(b"a"));
        trees.insert("b".to_string(), leaf(b"b"));
        trees.insert("c".to_string(), leaf(b"c"));

        let source = MockSource {
            refs,
            trees,
            ..Default::default()
        };
        let mut rx = fetch_listings_with_source(
            &source,
            2,
            input_channel(vec![PackageEntry::new(a.clone())]),
        )
        .await
        .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert_eq!(collected.len(), 3);
        let hashes: Vec<_> = collected.iter().map(|(p, _)| p.hash()).collect();
        assert!(hashes.contains(&"a"));
        assert!(hashes.contains(&"b"));
        assert!(hashes.contains(&"c"));
    }

    #[tokio::test]
    async fn closure_fetcher_deduplicates_references() {
        let a = sp("a", "a");
        let b = sp("b", "b");

        let mut refs = indexmap::IndexMap::new();
        refs.insert("a".to_string(), vec![b.clone(), b.clone()]);
        refs.insert("b".to_string(), vec![b.clone()]);

        let mut trees = indexmap::IndexMap::new();
        trees.insert("a".to_string(), leaf(b"a"));
        trees.insert("b".to_string(), leaf(b"b"));

        let source = MockSource {
            refs,
            trees,
            ..Default::default()
        };
        let mut rx = fetch_listings_with_source(
            &source,
            2,
            input_channel(vec![PackageEntry::new(a.clone())]),
        )
        .await
        .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert_eq!(collected.len(), 2);
    }

    #[tokio::test]
    async fn closure_fetcher_skips_missing_ls() {
        let a = sp("a", "a");
        let b = sp("b", "b");

        let mut refs = indexmap::IndexMap::new();
        refs.insert("a".to_string(), vec![b.clone()]);

        let mut trees = indexmap::IndexMap::new();
        trees.insert("b".to_string(), leaf(b"b"));

        let source = MockSource {
            refs,
            trees,
            ..Default::default()
        };
        let mut rx = fetch_listings_with_source(
            &source,
            2,
            input_channel(vec![PackageEntry::new(a.clone())]),
        )
        .await
        .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert_eq!(collected.len(), 1);
        assert_eq!(collected.first().map(|(p, _)| p.hash()), Some("b"));
    }

    fn find_entry(tree: &FileTree, path: &[u8]) -> Option<FileTreeEntry> {
        tree.to_list(b"").into_iter().find(|e| e.path == path)
    }

    #[tokio::test]
    async fn synthesizes_main_program_when_narinfo_missing() {
        let a = sp("a", "a");

        let source = MockSource {
            missing_narinfo: indexmap::IndexSet::from_iter(["a".to_string()]),
            ..Default::default()
        };
        let mut rx = fetch_listings_with_source(
            &source,
            2,
            input_channel(vec![PackageEntry {
                path: a.clone(),
                main_program: Some("foo".to_string()),
            }]),
        )
        .await
        .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert_eq!(collected.len(), 1);
        let (path, tree) = collected.first().expect("one item");
        assert_eq!(path.hash(), "a");
        let entry = find_entry(tree, b"/bin/foo").expect("/bin/foo entry");
        assert!(matches!(
            entry.node,
            FileNode::Regular {
                executable: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn synthesizes_main_program_when_ls_missing() {
        let a = sp("a", "a");

        let source = MockSource {
            missing_ls: indexmap::IndexSet::from_iter(["a".to_string()]),
            ..Default::default()
        };
        let mut rx = fetch_listings_with_source(
            &source,
            2,
            input_channel(vec![PackageEntry {
                path: a.clone(),
                main_program: Some("foo".to_string()),
            }]),
        )
        .await
        .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert_eq!(collected.len(), 1);
        let (path, tree) = collected.first().expect("one item");
        assert_eq!(path.hash(), "a");
        assert!(find_entry(tree, b"/bin/foo").is_some());
    }

    #[tokio::test]
    async fn does_not_synthesize_main_program_for_references() {
        let a = sp("a", "a");
        let b = sp("b", "b");

        let mut refs = indexmap::IndexMap::new();
        refs.insert("a".to_string(), vec![b.clone()]);

        let source = MockSource {
            refs,
            missing_ls: indexmap::IndexSet::from_iter(["a".to_string(), "b".to_string()]),
            ..Default::default()
        };
        let mut rx = fetch_listings_with_source(
            &source,
            2,
            input_channel(vec![PackageEntry {
                path: a.clone(),
                main_program: Some("foo".to_string()),
            }]),
        )
        .await
        .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert_eq!(collected.len(), 1);
        assert_eq!(collected.first().map(|(p, _)| p.hash()), Some("a"));
    }

    #[tokio::test]
    async fn skips_invalid_main_program() {
        let a = sp("a", "a");

        let source = MockSource {
            missing_narinfo: indexmap::IndexSet::from_iter(["a".to_string()]),
            ..Default::default()
        };
        let mut rx = fetch_listings_with_source(
            &source,
            2,
            input_channel(vec![PackageEntry {
                path: a.clone(),
                main_program: Some("foo/bar".to_string()),
            }]),
        )
        .await
        .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert!(collected.is_empty());
    }
}
