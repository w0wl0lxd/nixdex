//! Recursive closure traversal for binary-cache `.ls` listings.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexSet;
use tokio::sync::{Mutex, Notify, Semaphore, mpsc};
use tracing::warn;

use crate::errors::Result;
use crate::files::FileTree;
use crate::hydra::{self, Fetcher};
use crate::store_path::StorePath;

/// Result item produced by [`fetch_listings`].
pub type ListingItem = (StorePath, Option<String>, FileTree);

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

/// Fetch `.ls` listings recursively over the runtime closure of `starting_set`.
///
/// The returned receiver yields every store path whose `.ls` listing could be
/// fetched and parsed. Missing `.ls` files or missing narinfos are skipped
/// silently; other errors are logged with [`tracing::warn`] and skipped.
///
/// Concurrency is bounded by `jobs` (clamped to at least 1).
pub async fn fetch_listings(
    fetcher: &Fetcher,
    jobs: usize,
    starting_set: Vec<StorePath>,
) -> Result<mpsc::Receiver<Result<ListingItem>>> {
    fetch_listings_with_source(fetcher, jobs, starting_set).await
}

async fn fetch_listings_with_source<S: ListingSource>(
    source: &S,
    jobs: usize,
    starting_set: Vec<StorePath>,
) -> Result<mpsc::Receiver<Result<ListingItem>>> {
    let jobs = jobs.max(1);
    let (out_tx, out_rx) = mpsc::channel::<Result<ListingItem>>(jobs * 2);
    let queue: Arc<Mutex<VecDeque<StorePath>>> = Arc::new(Mutex::new(VecDeque::new()));
    let seen: Arc<Mutex<IndexSet<String>>> = Arc::new(Mutex::new(IndexSet::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(Notify::new());
    let semaphore = Arc::new(Semaphore::new(jobs));

    {
        let mut q = queue.lock().await;
        let mut s = seen.lock().await;
        for path in starting_set {
            if s.insert(path.hash().to_string()) {
                in_flight.fetch_add(1, Ordering::SeqCst);
                q.push_back(path);
            }
        }
    }

    let source = source.clone();
    let out_tx_for_dispatcher = out_tx.clone();
    tokio::spawn(async move {
        let source = source;
        loop {
            let path = {
                let mut q = queue.lock().await;
                q.pop_front()
            };

            if let Some(path) = path {
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
                let out_tx = out_tx_for_dispatcher.clone();

                tokio::spawn(async move {
                    let _permit = permit;
                    if let Some(item) =
                        process_path(&source, &path, &queue, &seen, &in_flight, &notify).await
                    {
                        let _ = out_tx.send(Ok(item)).await;
                    }
                });
            } else {
                let done = in_flight.load(Ordering::SeqCst) == 0;
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
    path: &StorePath,
    queue: &Mutex<VecDeque<StorePath>>,
    seen: &Mutex<IndexSet<String>>,
    in_flight: &AtomicUsize,
    notify: &Notify,
) -> Option<ListingItem> {
    let result = process_path_inner(source, path, queue, seen, in_flight, notify).await;

    let remaining = in_flight.fetch_sub(1, Ordering::SeqCst);
    if remaining == 1 {
        notify.notify_one();
    }

    result
}

async fn process_path_inner<S: ListingSource>(
    source: &S,
    path: &StorePath,
    queue: &Mutex<VecDeque<StorePath>>,
    seen: &Mutex<IndexSet<String>>,
    in_flight: &AtomicUsize,
    notify: &Notify,
) -> Option<ListingItem> {
    let (refs, nar_url) = match source.fetch_narinfo_details(path).await {
        Ok(details) => details,
        Err(err) => {
            if !err.is_not_found() {
                warn!(path = %path, error = %err, "failed to fetch narinfo; skipping");
            }
            return None;
        }
    };

    for r in refs {
        let hash = r.hash().to_string();
        let new = {
            let mut s = seen.lock().await;
            s.insert(hash)
        };
        if new {
            in_flight.fetch_add(1, Ordering::SeqCst);
            queue.lock().await.push_back(r);
            notify.notify_one();
        }
    }

    match source.fetch_file_tree(path).await {
        Ok(tree) => Some((path.clone(), nar_url, tree)),
        Err(err) => {
            if !err.is_not_found() {
                warn!(path = %path, error = %err, "failed to fetch .ls listing; skipping");
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[derive(Clone)]
    struct MockSource {
        refs: indexmap::IndexMap<String, Vec<StorePath>>,
        trees: indexmap::IndexMap<String, FileTree>,
    }

    impl ListingSource for MockSource {
        async fn fetch_narinfo_details(
            &self,
            path: &StorePath,
        ) -> std::result::Result<(Vec<StorePath>, Option<String>), hydra::Error> {
            let refs = self.refs.get(path.hash()).cloned().unwrap_or_else(Vec::new);
            let url = Some(format!("nar/{}.nar.xz", path.hash()));
            Ok((refs, url))
        }

        async fn fetch_file_tree(
            &self,
            path: &StorePath,
        ) -> std::result::Result<FileTree, hydra::Error> {
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

        let source = MockSource { refs, trees };
        let mut rx = fetch_listings_with_source(&source, 2, vec![a.clone()])
            .await
            .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert_eq!(collected.len(), 3);
        let hashes: Vec<_> = collected.iter().map(|(p, _, _)| p.hash()).collect();
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

        let source = MockSource { refs, trees };
        let mut rx = fetch_listings_with_source(&source, 2, vec![a.clone()])
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

        let source = MockSource { refs, trees };
        let mut rx = fetch_listings_with_source(&source, 2, vec![a.clone()])
            .await
            .expect("start");

        let mut collected = Vec::new();
        while let Some(result) = rx.recv().await {
            collected.push(result.expect("item"));
        }

        assert_eq!(collected.len(), 1);
        assert_eq!(collected.first().map(|(p, _, _)| p.hash()), Some("b"));
    }
}
